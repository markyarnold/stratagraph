//! strata-data: the database-schema (data) plane — SQL DDL extraction.
//!
//! A [`SqlSchemaAdapter`] turns a `.sql` file's text (committed DDL / a migration)
//! into a typed [`SchemaModel`] — the tables it declares, each table's columns
//! (name + SQL type + nullability + primary-key flag), and the foreign-key edges
//! between columns. The shape deliberately mirrors `strata-infra`'s adapter
//! (`detects` + `extract`, pure, no IO, fixture-tested) and `strata-contract`'s,
//! so it flows through `strata-index`'s one `build_data_plane` path the same way
//! the IaC plane flows through `build_infra_plane`.
//!
//! ## Pure & honest
//!
//! Pure: the caller reads the file and hands the adapter its text, so the same
//! `(path, content)` always yields the same [`SchemaModel`] (determinism). A
//! genuinely unparseable file returns [`DataError::Parse`] and never panics, and
//! never partially extracts a broken document — the caller surfaces the
//! `(path, error)` as a diagnostic rather than silently dropping it (the infra
//! `templates_failed` precedent).
//!
//! ## What it models (and the documented bound)
//!
//! M1 is **pure DDL** — explicit facts only, nothing inferred (ORM
//! convention-name inference is M2). A migration *set* applies its `CREATE`/
//! `ALTER` statements **in file order** to build the *latest declared* schema:
//! `ALTER TABLE … ADD/DROP/RENAME COLUMN` mutate the table built so far. The bound
//! is explicit: we model the **declared end-state**, not the migration history
//! (we do not replay-then-diff; the final shape is what the graph sees).
//!
//! Statements we do not model — `CREATE INDEX` / `CREATE EXTENSION` /
//! `CREATE FUNCTION` / `CREATE VIEW`, `INSERT`s, and anything else that is not a
//! `CREATE TABLE` / `ALTER TABLE` — are **skipped, not errored**: a migration file
//! that interleaves them still yields its tables. A foreign key is recorded only
//! from an explicit inline `REFERENCES` or a table-level `FOREIGN KEY`; a table or
//! column that the parsed DDL never declares is never invented.

use serde::{Deserialize, Serialize};
use sqlparser::ast::{
    AlterTableOperation, ColumnDef as SqlColumnDef, ColumnOption, Expr, FromTable, IndexColumn,
    ObjectName, Query, SetExpr, Statement, TableConstraint, TableFactor, TableObject,
    TableWithJoins,
};
use sqlparser::dialect::{ClickHouseDialect, PostgreSqlDialect};
use sqlparser::parser::Parser;
use thiserror::Error;

/// An error from extracting a SQL schema file.
#[derive(Debug, Error)]
pub enum DataError {
    /// The SQL text could not be parsed (a genuinely malformed/truncated DDL
    /// file). Carries the file path and a human-readable reason so the caller can
    /// surface `path + error` as a diagnostic rather than skip it silently.
    #[error("parse error in {path}: {msg}")]
    Parse {
        /// The file path (caller-supplied; repo-relative).
        path: String,
        /// Human-readable reason the file could not be parsed.
        msg: String,
    },
}

/// One column of a table: its name, SQL type as written, nullability, and whether
/// it participates in the table's primary key.
///
/// `sql_type` is the type *as declared* (e.g. `"BIGINT"`, `"VARCHAR(255)"`,
/// `"JSONB"`, `"TEXT[]"`), rendered losslessly from the parsed AST — never
/// normalized or guessed. `nullable` is `true` unless a `NOT NULL` or a
/// primary-key constraint forces it `false`. `primary_key` is `true` for a column
/// declared `PRIMARY KEY` inline or named in a table-level `PRIMARY KEY (...)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColumnDef {
    /// The column name (unquoted; a quoted `"UserId"` keeps its raw value).
    pub name: String,
    /// The SQL type as declared, rendered from the AST.
    pub sql_type: String,
    /// Whether the column may be NULL. `false` iff `NOT NULL` or primary-key.
    pub nullable: bool,
    /// Whether the column is (part of) the primary key.
    pub primary_key: bool,
}

/// A foreign-key relationship: `column` in this table references
/// `ref_table`.`ref_column`. Recorded only from an explicit inline `REFERENCES`
/// or a table-level `FOREIGN KEY` — never inferred.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForeignKey {
    /// The referencing column in this table.
    pub column: String,
    /// The referenced table (unquoted; the last segment of a qualified name).
    pub ref_table: String,
    /// The referenced column in `ref_table`.
    pub ref_column: String,
}

/// A table declared by the DDL: its name, its columns (in declared order), and its
/// foreign keys. The cumulative end-state after all `CREATE`/`ALTER` statements
/// for this table have been applied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableDef {
    /// The table name (unquoted; the last segment of a qualified name).
    pub name: String,
    /// The columns, in declared order (ALTER ADD appends; DROP removes; RENAME
    /// renames in place).
    pub columns: Vec<ColumnDef>,
    /// The foreign keys declared on this table.
    pub foreign_keys: Vec<ForeignKey>,
}

impl TableDef {
    fn new(name: String) -> Self {
        TableDef {
            name,
            columns: Vec::new(),
            foreign_keys: Vec::new(),
        }
    }

    /// The index of a column by name, if present.
    fn column_index(&self, name: &str) -> Option<usize> {
        self.columns.iter().position(|c| c.name == name)
    }
}

/// The result of extracting one `.sql` file: its path plus the tables declared,
/// in first-declared order.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SchemaModel {
    /// The file path (caller-supplied; repo-relative).
    pub path: String,
    /// The tables declared, in first-`CREATE`-seen order.
    pub tables: Vec<TableDef>,
    /// Statements in this file that the statement splitter isolated but
    /// `sqlparser` could not parse — a PL/pgSQL `DO $$…$$`/`CREATE FUNCTION` body,
    /// a dialect-specific statement, etc. They are **skipped, not errored**: the
    /// file's parseable `CREATE TABLE`/`ALTER TABLE` statements are still extracted
    /// (mirrors the CFN adapter's "skip non-resource statements" precedent). This
    /// is an *informational* signal — a file with some-good-some-bad statements is a
    /// SUCCESS that yields its tables, NOT a failure (see [`DataError::Parse`] for
    /// the wholly-unparseable-DDL case). Serialized with a default so older models
    /// deserialize.
    #[serde(default)]
    pub skipped_statements: usize,
}

/// A data-plane adapter. SQL DDL is the first (M1) implementation; the interface
/// mirrors `strata_infra::IacAdapter` and `strata_contract::ContractAdapter`.
pub trait SchemaAdapter {
    /// Cheap heuristic: does `filename`/`content` look like this adapter's schema
    /// format? Used to pick schema files out of a repo before the (more expensive)
    /// [`extract`](SchemaAdapter::extract). Must reject non-schema files (a `.sql`
    /// with only `SELECT`/`INSERT`, or a non-SQL file that happens to end `.sql`).
    fn detects(&self, filename: &str, content: &str) -> bool;

    /// Parse a `.sql` file's text into a [`SchemaModel`]. A genuinely malformed
    /// file returns [`DataError::Parse`] so the caller can degrade (skip it, keep
    /// indexing) rather than crash, and never yields a partial extraction.
    fn extract(&self, path: &str, content: &str) -> Result<SchemaModel, DataError>;
}

/// Adapter for SQL DDL / migration files, parsed with the PostgreSQL dialect (the
/// AWS RDS dogfood target).
pub struct SqlSchemaAdapter;

/// The cheap textual signal that a file is *attempting* to declare schema: it
/// mentions both `CREATE`/`ALTER` and `TABLE` (case-insensitively). A `.sql` file
/// of pure `SELECT`/`INSERT` carries neither pairing, so it is cheaply rejected
/// before the full parse; a file that has the signal but won't parse is surfaced
/// as a malformed schema worth reporting — not silently dropped.
fn has_ddl_textual_signal(content: &str) -> bool {
    let lower = content.to_ascii_lowercase();
    (lower.contains("create") || lower.contains("alter")) && lower.contains("table")
}

/// Whether `content` carries the cheap SQL DDL textual signal (it mentions
/// `CREATE`/`ALTER` and `TABLE`, case-insensitively) — the public companion to
/// [`SqlSchemaAdapter::detects`] used by the indexer to decide whether a `.sql`
/// file that FAILED to parse is a malformed-but-schema-shaped file worth surfacing
/// as a diagnostic (vs. a non-SQL `.sql` of prose to skip silently). This is the
/// data-plane analogue of CFN's `has_cfn_textual_signal` discriminator.
pub fn looks_like_ddl(content: &str) -> bool {
    has_ddl_textual_signal(content)
}

/// How a code symbol touches a table, parsed from a raw-SQL DML fragment
/// (Slice 16, D3, M2). A `SELECT`/`JOIN` source is a [`Read`](SqlAccess::Read); an
/// `INSERT`/`UPDATE`/`DELETE` target is a [`Write`](SqlAccess::Write).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SqlAccess {
    /// The statement reads the table (`SELECT … FROM t`, `… JOIN t`, the source of
    /// an `INSERT … SELECT`).
    Read,
    /// The statement writes the table (`INSERT INTO t`, `UPDATE t`, `DELETE FROM t`).
    Write,
}

/// One table a parsed SQL fragment references, plus how it is accessed. The
/// `table` is the bare (unquoted) last segment of the name — `app.widgets` →
/// `widgets`, `"Orders"` → `Orders` — so it reconciles with a [`TableDef::name`]
/// declared either way (the same normalization `object_name_last` applies to DDL).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TableRef {
    /// The referenced table's bare name (last segment, unquoted).
    pub table: String,
    /// Whether this reference reads or writes the table.
    pub access: SqlAccess,
}

/// Parse a single raw-SQL fragment into the tables it references and how
/// (`SELECT`/`JOIN` → [`Read`](SqlAccess::Read); `INSERT`/`UPDATE`/`DELETE` →
/// [`Write`](SqlAccess::Write)) — the data-plane code→table linker's core
/// primitive (Slice 16, D3, M2).
///
/// **Honest by construction.** A fragment that does not parse as a complete
/// statement (a partial/interpolated query, a non-DML statement, prose that slipped
/// the keyword prefilter) yields an **empty** vec — never an error, never a guessed
/// table (R1/R5). Only the statement forms the data plane links are inspected;
/// anything else returns empty. Results are deduplicated (a table named twice in one
/// statement yields one ref per access kind) and returned in a deterministic,
/// first-seen order so the resulting edge set is reproducible (R3).
///
/// Scope (M2 raw-SQL backbone, documented bound): table-level granularity only —
/// `SELECT a, b FROM t` is a Read of `t`, not of `t.a`/`t.b` (column-level
/// resolution is deferred). `INSERT INTO t SELECT … FROM s` records `t` Write **and**
/// `s` Read. Subqueries nested in a SELECT's FROM are followed, and a `DELETE … USING s`
/// records `s` Read. Documented gap (conservative — a missed read, never a phantom):
/// an `UPDATE t … FROM s`'s auxiliary source `s` is NOT yet captured (only `t` Write);
/// capturing it is a small follow-up.
pub fn parse_table_refs(sql: &str) -> Vec<TableRef> {
    // Parse with the same PostgreSQL dialect the DDL adapter uses. A fragment that
    // won't parse is not an error here — many code string literals are partial SQL.
    let Ok(statements) = Parser::parse_sql(&PostgreSqlDialect {}, sql) else {
        return Vec::new();
    };
    let mut refs: Vec<TableRef> = Vec::new();
    for stmt in &statements {
        collect_stmt_table_refs(stmt, &mut refs);
    }
    refs
}

/// Push a `(table, access)` ref, de-duplicating on the exact pair so a table named
/// twice with the same access yields one entry (first-seen order preserved).
fn push_ref(refs: &mut Vec<TableRef>, table: String, access: SqlAccess) {
    let candidate = TableRef { table, access };
    if !refs.contains(&candidate) {
        refs.push(candidate);
    }
}

/// Collect the table references of one top-level statement into `refs`, tagging the
/// access kind by statement form.
fn collect_stmt_table_refs(stmt: &Statement, refs: &mut Vec<TableRef>) {
    match stmt {
        // SELECT (or any query): every table in its FROM/JOIN chain is a Read.
        Statement::Query(q) => collect_query_reads(q, refs),
        // INSERT INTO t [...] [SELECT … FROM s]: t is a Write; a SELECT source's
        // tables are Reads (INSERT … SELECT copies from s into t).
        Statement::Insert(insert) => {
            if let TableObject::TableName(name) = &insert.table {
                push_ref(refs, object_name_last(name), SqlAccess::Write);
            }
            if let Some(source) = &insert.source {
                collect_query_reads(source, refs);
            }
        }
        // UPDATE t SET …: t is a Write. Documented gap: an auxiliary `FROM s`
        // (`UPDATE … FROM s`) source read is not yet captured — conservative (a
        // missed read, never a phantom); a small follow-up.
        Statement::Update(update) => {
            collect_table_with_joins(&update.table, refs, SqlAccess::Write);
        }
        // DELETE FROM t: t is a Write; a USING clause reads its tables.
        Statement::Delete(delete) => {
            let tables = match &delete.from {
                FromTable::WithFromKeyword(t) | FromTable::WithoutKeyword(t) => t,
            };
            for twj in tables {
                collect_table_with_joins(twj, refs, SqlAccess::Write);
            }
            if let Some(using) = &delete.using {
                for twj in using {
                    collect_table_with_joins(twj, refs, SqlAccess::Read);
                }
            }
        }
        // Any other statement carries no code→table Reads/Writes fact this milestone.
        _ => {}
    }
}

/// Collect every table referenced by a query body as a [`Read`](SqlAccess::Read):
/// the SELECT's `FROM` relations and their `JOIN`s, recursing into subquery
/// derived tables. A set operation (`UNION`/`INTERSECT`) recurses into both sides.
fn collect_query_reads(query: &Query, refs: &mut Vec<TableRef>) {
    collect_setexpr_reads(&query.body, refs);
}

/// Walk a [`SetExpr`] (a SELECT, a set operation, or a parenthesised/`VALUES` body)
/// collecting Read table refs.
fn collect_setexpr_reads(body: &SetExpr, refs: &mut Vec<TableRef>) {
    match body {
        SetExpr::Select(select) => {
            for twj in &select.from {
                collect_table_with_joins(twj, refs, SqlAccess::Read);
            }
        }
        SetExpr::Query(q) => collect_query_reads(q, refs),
        SetExpr::SetOperation { left, right, .. } => {
            collect_setexpr_reads(left, refs);
            collect_setexpr_reads(right, refs);
        }
        // VALUES / INSERT / UPDATE / TABLE bodies carry no FROM-relation reads here.
        _ => {}
    }
}

/// Collect a [`TableWithJoins`]' base relation and each join's relation at the given
/// access kind, recursing into subquery (`Derived`) relations as Reads.
fn collect_table_with_joins(twj: &TableWithJoins, refs: &mut Vec<TableRef>, access: SqlAccess) {
    collect_table_factor(&twj.relation, refs, access);
    for join in &twj.joins {
        // A joined table is always read (you cannot write through a JOIN here).
        collect_table_factor(&join.relation, refs, SqlAccess::Read);
    }
}

/// Collect a single [`TableFactor`]: a named table contributes one ref at `access`;
/// a derived subquery recurses (its inner tables are Reads); other factors (table
/// functions, UNNEST, …) carry no linkable table name and are skipped.
fn collect_table_factor(factor: &TableFactor, refs: &mut Vec<TableRef>, access: SqlAccess) {
    match factor {
        TableFactor::Table { name, .. } => {
            push_ref(refs, object_name_last(name), access);
        }
        TableFactor::Derived { subquery, .. } => collect_query_reads(subquery, refs),
        TableFactor::NestedJoin {
            table_with_joins, ..
        } => collect_table_with_joins(table_with_joins, refs, access),
        _ => {}
    }
}

impl SqlSchemaAdapter {
    /// Parse `content` **statement-by-statement** into the cumulative
    /// [`SchemaModel`]. Shared by [`detects`](SchemaAdapter::detects) (which throws
    /// the model away and just checks for ≥1 table) and
    /// [`extract`](SchemaAdapter::extract).
    ///
    /// ## Why per-statement (the robustness fix)
    ///
    /// `sqlparser` cannot parse every Postgres statement — a PL/pgSQL `DO $$…$$`
    /// block, a `CREATE FUNCTION` with a dollar-quoted body, an exotic dialect
    /// statement. Parsing the *whole file* in one call meant a single such
    /// statement failed the ENTIRE migration, dropping every `CREATE TABLE` around
    /// it (found in real-world dogfooding: migrations can lose their tables to a `DO` block or a
    /// dollar-quoted string). Instead we [`split_sql_statements`] the file on
    /// top-level `;` (respecting strings/dollar-quotes/comments so an inner `;`
    /// never splits), parse each statement individually, and **skip** the ones that
    /// will not parse (counting them in [`SchemaModel::skipped_statements`]) — the
    /// surrounding `CREATE TABLE`/`ALTER TABLE` statements still build the schema.
    /// This mirrors the CFN adapter's "skip non-resource statements, only diagnose a
    /// wholly-bad file" precedent.
    ///
    /// ## Fail vs. skip (honest coverage)
    ///
    /// A file is a [`DataError::Parse`] failure (→ the indexer's `schemas_failed`)
    /// **only** when it has the DDL textual signal yet yields **zero** parseable
    /// statements — it looked like schema but nothing parsed. A file with
    /// some-good-some-bad statements is a SUCCESS that yields its tables; the skipped
    /// count is surfaced informationally, never as a failure. A file with no DDL
    /// signal that parses to nothing (prose, a query-only `.sql`) is `Ok` with no
    /// tables — the caller skips it silently (no false alarm), exactly as before.
    fn parse(&self, path: &str, content: &str) -> Result<SchemaModel, DataError> {
        let mut statements: Vec<Statement> = Vec::new();
        let mut skipped: usize = 0;

        for stmt_sql in split_sql_statements(content) {
            // A split segment that is blank/comment-only carries no statement — it
            // is not a skip (nothing was dropped), just empty. `parse_sql` returns
            // an empty Vec for it.
            match parse_statement_lenient(&stmt_sql) {
                Some(parsed) => statements.extend(parsed),
                None => skipped += 1,
            }
        }

        // Honest fail-vs-skip: only a DDL-signalled file that produced NOTHING
        // parseable is a parse failure. (A non-DDL `.sql` that parses to nothing —
        // prose, a query-only file — is `Ok` with no tables, skipped silently by the
        // caller; the CFN `detect_kind` Malformed-vs-NotCfn precedent.)
        if statements.is_empty() && has_ddl_textual_signal(content) {
            return Err(DataError::Parse {
                path: path.to_string(),
                msg: format!(
                    "no parseable SQL statement in a DDL-shaped file \
                     ({skipped} statement(s) could not be parsed)"
                ),
            });
        }

        let mut model = build_schema(path, &statements);
        model.skipped_statements = skipped;
        Ok(model)
    }
}

/// Parse one split statement into AST statements, in order of preference:
/// 1. the **PostgreSQL** dialect (primary — every statement that parsed before still
///    parses here, identically, so this is a pure superset: no regression);
/// 2. the **ClickHouse** dialect (recovers e.g. `CREATE MATERIALIZED VIEW … TO …`);
/// 3. for a `CREATE TABLE` whose ClickHouse engine/settings tail (`ENGINE = …`,
///    `PARTITION BY`, `TTL`, `SETTINGS`, `ON CLUSTER …`) defeats *both* dialects, a
///    recovery that re-parses just the balanced column-list prefix `CREATE TABLE x (…)`.
///
/// Returns `None` only when none of these parse it, so the caller counts it as a skip
/// (exactly as before for genuinely unparseable statements). **Honest by construction**
/// (R1/R5): the recovered prefix is accepted *only* when it re-parses to a real
/// [`Statement::CreateTable`] — sqlparser still validates it — and the recovery drops
/// only the engine/settings tail, never a column, so a recovered table is the declared
/// one, never a guess. A blank/comment-only segment parses to an empty `Vec` under (1),
/// returned as `Some(vec![])` so it is not miscounted as a skip.
fn parse_statement_lenient(stmt_sql: &str) -> Option<Vec<Statement>> {
    if let Ok(parsed) = Parser::parse_sql(&PostgreSqlDialect {}, stmt_sql) {
        return Some(parsed);
    }
    if let Ok(parsed) = Parser::parse_sql(&ClickHouseDialect {}, stmt_sql) {
        return Some(parsed);
    }
    let prefix = recover_create_table_prefix(stmt_sql)?;
    match Parser::parse_sql(&PostgreSqlDialect {}, prefix) {
        Ok(parsed)
            if parsed
                .iter()
                .any(|s| matches!(s, Statement::CreateTable(_))) =>
        {
            Some(parsed)
        }
        _ => None,
    }
}

/// For a statement carrying a top-level parenthesised list — a `CREATE TABLE name
/// (… columns …) <tail>` — return the slice up to and including the **balanced close**
/// of that first top-level `(…)`, i.e. `CREATE TABLE name (… columns …)`, so it can be
/// re-parsed without a ClickHouse-specific engine/settings tail that sqlparser rejects.
///
/// The scan is string- and comment-aware (single `'…'` with `''` and `\` escapes, `"…"`
/// and `` `…` `` quoted identifiers, `-- …` line and `/* … */` block comments) and
/// paren-depth aware, so a `(` inside a string/comment, or a nested type like
/// `Decimal(10, 2)` / `Map(String, String)`, never mis-terminates the list. Returns
/// `None` if no balanced top-level `(…)` is found. The caller is responsible for only
/// *using* the result when it re-parses to a `CreateTable` (this function does not
/// itself decide the statement kind).
fn recover_create_table_prefix(stmt: &str) -> Option<&str> {
    let b = stmt.as_bytes();
    let mut i = 0;
    let mut depth: i32 = 0;
    let mut opened = false;
    while i < b.len() {
        match b[i] {
            // Single-quoted string: honour `\` escapes and the doubled `''` literal.
            b'\'' => {
                i += 1;
                while i < b.len() {
                    match b[i] {
                        b'\\' => i += 2,
                        b'\'' if i + 1 < b.len() && b[i + 1] == b'\'' => i += 2,
                        b'\'' => break,
                        _ => i += 1,
                    }
                }
            }
            // Double-quoted / backtick-quoted identifiers: inert until the close.
            b'"' => {
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    i += 1;
                }
            }
            b'`' => {
                i += 1;
                while i < b.len() && b[i] != b'`' {
                    i += 1;
                }
            }
            // Line comment to end of line.
            b'-' if i + 1 < b.len() && b[i + 1] == b'-' => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            // Block comment to the closing `*/`.
            b'/' if i + 1 < b.len() && b[i + 1] == b'*' => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i += 1; // sit on the '/'; the loop's `i += 1` steps past it
            }
            b'(' => {
                depth += 1;
                opened = true;
            }
            b')' => {
                depth -= 1;
                if opened && depth == 0 {
                    return Some(&stmt[..=i]);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Split SQL `content` into its top-level statements, separated by `;`, returning
/// each statement's source text (the trailing `;` is dropped) in file order.
///
/// The crux of the data-plane robustness fix: a `;` is a statement separator
/// **only** at the top level — never when it sits inside any of these, where the
/// lexer treats it as ordinary text:
///
/// - a **single-quoted string** `'…'` — with the SQL `''` escape (a doubled quote
///   is a literal quote, not a close), so `';'` is one string, not a separator. A
///   Postgres **E-string** `E'…'`/`e'…'` (a `'` preceded by a standalone `e`/`E`
///   token) additionally honours **backslash escapes**: `\'` and `\\` do not close
///   or mis-pair, so `SELECT E'a\'; b';` keeps its inner `;` in the string;
/// - a **double-quoted identifier** `"…"` — its own state: a `'`, `;`, `--`, `/*`,
///   or `$` inside it is **inert** (ordinary identifier text), and the only escape
///   is the SQL doubled `""` (a literal quote that stays inside). So `"it's"` does
///   not flip into string state and `"a;b"` is not split mid-identifier;
/// - a **dollar-quoted body** `$$…$$` or `$tag$…$tag$` — Postgres PL/pgSQL and
///   string syntax. The opening tag (`$$` or `$name$`) must be matched **exactly**
///   to close, so a `DO $$ BEGIN …; … END $$;` keeps its inner `;`s, and a body
///   opened `$body$` is not closed by a bare `$$`. **This is the load-bearing
///   case**;
/// - a **line comment** `-- …` to end-of-line;
/// - a **block comment** `/* … */`, which in Postgres **nests** (`/* /* */ */` is
///   one comment) — tracked with a depth counter.
///
/// Honest bound: this is a careful lexer, not a full SQL grammar. It tracks the
/// quote/comment states that determine where a statement ends; everything else is
/// ordinary text. A statement whose body it isolates but `sqlparser` cannot parse
/// is the caller's concern (it is skipped, not mis-split). One narrow shape is not
/// tracked: `SET standard_conforming_strings = off` (deprecated, non-default) would
/// make backslashes escape in **normal** (non-`E`) strings too — we treat backslash
/// as literal in normal strings (the default `standard_conforming_strings = on`).
/// This cannot silently mislead: a mis-split there yields segments that fail to
/// parse and are skip-counted, never an invented table.
///
/// Encoding-safe: the lexer scans `content.as_bytes()` (all the structural tokens
/// — `;`, `'`, `$`, `--`, `/* */` — are ASCII), but every emitted statement is a
/// **slice of the original `&str`** (`content[start..end]`), so any multi-byte
/// UTF-8 inside a string literal, identifier, or comment is reproduced byte-exact.
/// Byte indices only ever land on ASCII boundaries (the structural tokens), which
/// are valid `str` slice boundaries.
fn split_sql_statements(content: &str) -> Vec<String> {
    let mut statements: Vec<String> = Vec::new();
    let bytes = content.as_bytes();
    let n = bytes.len();
    let mut i = 0;
    // Byte offset where the in-progress statement began.
    let mut start = 0;

    // Emit `content[start..end]` as a finished statement and advance `start`.
    let push_stmt = |statements: &mut Vec<String>, start: &mut usize, end: usize| {
        statements.push(content[*start..end].to_string());
        *start = end;
    };

    while i < n {
        let c = bytes[i];

        // ── Line comment: `-- …` to end-of-line (the newline is ordinary). ──
        if c == b'-' && i + 1 < n && bytes[i + 1] == b'-' {
            i += 2;
            while i < n && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }

        // ── Block comment: `/* … */`, NESTING (Postgres `/* /* */ */`). ──
        if c == b'/' && i + 1 < n && bytes[i + 1] == b'*' {
            let mut depth = 1usize;
            i += 2;
            while i < n && depth > 0 {
                if bytes[i] == b'/' && i + 1 < n && bytes[i + 1] == b'*' {
                    depth += 1;
                    i += 2;
                } else if bytes[i] == b'*' && i + 1 < n && bytes[i + 1] == b'/' {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            continue;
        }

        // ── Single-quoted string: `'…'`, with the `''` doubled-quote escape. ──
        // A Postgres E-string (`E'…'`/`e'…'`) ALSO honours backslash escapes, so we
        // detect it when the opening `'` is immediately preceded by a standalone
        // `e`/`E` token (an `e` that is NOT the tail of a longer identifier — so
        // `E'…'`, `= e'…'` are E-strings but the `e` in `table'x'` is not).
        if c == b'\'' {
            let is_estring = i >= 1
                && (bytes[i - 1] == b'e' || bytes[i - 1] == b'E')
                && (i == 1 || !is_ident_byte(bytes[i - 2]));
            i += 1;
            while i < n {
                // In an E-string a backslash escapes the NEXT byte, so `\'` and `\\`
                // neither close nor mis-pair. (Guard `i + 1 < n` so a trailing lone
                // `\` at EOF does not panic.) Not an escape in a normal string —
                // there a backslash is ordinary text (default
                // `standard_conforming_strings = on`).
                if is_estring && bytes[i] == b'\\' && i + 1 < n {
                    i += 2;
                    continue;
                }
                if bytes[i] == b'\'' {
                    // A doubled `''` is an escaped quote — skip both, stay inside;
                    // a lone `'` closes the string.
                    if i + 1 < n && bytes[i + 1] == b'\'' {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }

        // ── Double-quoted identifier: `"…"`, with the `""` doubled-quote escape. ──
        // Its own lexer state, parallel to the single-quote branch: everything inside
        // is ordinary identifier text — a `'`, `;`, `--`, `/*`, or `$` here is INERT
        // — and the only escape is the SQL doubled `""` (a literal quote that stays
        // inside; a lone `"` closes). This stops a `'` in `"it's"` from flipping into
        // string state and a `;` in `"a;b"` from splitting mid-identifier.
        if c == b'"' {
            i += 1;
            while i < n {
                if bytes[i] == b'"' {
                    if i + 1 < n && bytes[i + 1] == b'"' {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }

        // ── Dollar-quoted body: `$$…$$` or `$tag$…$tag$` (the load-bearing case). ──
        if c == b'$' {
            if let Some(tag_len) = dollar_tag_len(bytes, i) {
                let tag = &bytes[i..i + tag_len];
                i += tag_len;
                // Consume verbatim until the EXACT matching closing tag, so an inner
                // `;` — or a different-tag `$$` — never ends the body.
                loop {
                    if i >= n {
                        // Unterminated dollar-quote: consume to EOF (malformed; the
                        // per-statement parse skips it). No infinite loop.
                        break;
                    }
                    if bytes[i] == b'$' {
                        if let Some(close_len) = dollar_tag_len(bytes, i) {
                            if &bytes[i..i + close_len] == tag {
                                i += close_len;
                                break;
                            }
                        }
                    }
                    i += 1;
                }
                continue;
            }
            // A bare `$` that opens no valid tag (e.g. a `$1` placeholder) is text.
            i += 1;
            continue;
        }

        // ── A top-level `;` ends the current statement (the `;` itself is dropped). ──
        if c == b';' {
            push_stmt(&mut statements, &mut start, i);
            i += 1;
            start = i;
            continue;
        }

        // ── Any other byte (incl. UTF-8 continuation bytes) is ordinary text. ──
        i += 1;
    }

    // The trailing text after the last `;` (a final statement with no terminator).
    if start < n {
        statements.push(content[start..].to_string());
    }

    statements
}

/// If a dollar-quote tag opens at `bytes[i]` (which must be `$`), return its byte
/// length (`2` for `$$`, `2 + ident.len()` for `$ident$`); otherwise `None`. A tag
/// is `$`, an optional identifier (`[A-Za-z_][A-Za-z0-9_]*`), then a closing `$` —
/// the exact Postgres dollar-quote tag grammar. Matching the full tag (not just
/// `$$`) lets nested bodies with different tags coexist.
fn dollar_tag_len(bytes: &[u8], i: usize) -> Option<usize> {
    debug_assert_eq!(bytes[i], b'$');
    let n = bytes.len();
    let mut j = i + 1;
    // Optional tag identifier: first char letter/underscore, then alphanumeric/_.
    while j < n {
        let b = bytes[j];
        let first = j == i + 1;
        let ok = b == b'_' || b.is_ascii_alphabetic() || (!first && b.is_ascii_digit());
        if ok {
            j += 1;
        } else {
            break;
        }
    }
    // Must close with `$`.
    if j < n && bytes[j] == b'$' {
        Some(j - i + 1)
    } else {
        None
    }
}

/// Is `b` an identifier byte — ASCII alphanumeric or `_`? Used to decide whether a
/// leading `e`/`E` before a `'` is a standalone E-string prefix (it is only when the
/// byte before it is NOT an identifier byte, so `E'…'` / `= e'…'` are E-strings but
/// the trailing `e` of an identifier like `table'x'` is not).
fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

impl SchemaAdapter for SqlSchemaAdapter {
    /// Detect a SQL DDL/migration file: it carries the cheap DDL textual signal AND
    /// parses into at least one `CREATE TABLE`. A file of pure `SELECT`/`INSERT`,
    /// or a non-SQL file that won't parse, is rejected. (A file with the signal that
    /// fails to parse is `detects == false` here; the indexer uses `extract`'s error
    /// to surface the malformed-but-DDL-shaped case as a diagnostic.)
    fn detects(&self, _filename: &str, content: &str) -> bool {
        if !has_ddl_textual_signal(content) {
            return false;
        }
        match self.parse("<detect>", content) {
            Ok(model) => !model.tables.is_empty(),
            Err(_) => false,
        }
    }

    fn extract(&self, path: &str, content: &str) -> Result<SchemaModel, DataError> {
        self.parse(path, content)
    }
}

/// Apply every `CREATE TABLE` / `ALTER TABLE` statement in `statements`, in order,
/// to build the cumulative declared schema. Non-table statements are skipped.
fn build_schema(path: &str, statements: &[Statement]) -> SchemaModel {
    let mut tables: Vec<TableDef> = Vec::new();

    for stmt in statements {
        match stmt {
            Statement::CreateTable(ct) => {
                let name = object_name_last(&ct.name);
                // A repeated `CREATE TABLE` of the same name (rare; e.g. a
                // DROP-then-CREATE migration) replaces the prior declaration so the
                // end-state reflects the latest CREATE.
                let table = build_table_from_create(name.clone(), &ct.columns, &ct.constraints);
                match tables.iter().position(|t| t.name == name) {
                    Some(idx) => tables[idx] = table,
                    None => tables.push(table),
                }
            }
            Statement::AlterTable(at) => {
                let name = object_name_last(&at.name);
                // An ALTER of a table we have not seen a CREATE for (the CREATE lives
                // in another file, or earlier history we were not given) is skipped:
                // we never invent a table from an ALTER alone (honest absence).
                if let Some(idx) = tables.iter().position(|t| t.name == name) {
                    for op in &at.operations {
                        apply_alter_op(&mut tables[idx], op);
                    }
                }
            }
            // CREATE INDEX/EXTENSION/FUNCTION/VIEW, INSERT, and every other
            // statement are deliberately skipped — not errors. A migration that
            // interleaves them still yields its tables.
            _ => {}
        }
    }

    SchemaModel {
        path: path.to_string(),
        tables,
        // The caller (`SqlSchemaAdapter::parse`) sets the real skipped-statement
        // count after splitting; `build_schema` only sees the statements that parsed.
        skipped_statements: 0,
    }
}

/// Build a [`TableDef`] from a `CREATE TABLE`'s columns and table-level
/// constraints.
fn build_table_from_create(
    name: String,
    columns: &[SqlColumnDef],
    constraints: &[TableConstraint],
) -> TableDef {
    let mut table = TableDef::new(name);

    for col in columns {
        let (column, inline_fk) = build_column(col);
        table.columns.push(column);
        if let Some(fk) = inline_fk {
            table.foreign_keys.push(fk);
        }
    }

    // Table-level constraints: PRIMARY KEY (a, b) flips those columns' pk flag (and
    // their nullability), FOREIGN KEY (x) REFERENCES t(y) adds an FK per column.
    for c in constraints {
        match c {
            TableConstraint::PrimaryKey(pk) => {
                for ic in &pk.columns {
                    if let Some(col_name) = index_col_name(ic) {
                        if let Some(idx) = table.column_index(&col_name) {
                            table.columns[idx].primary_key = true;
                            table.columns[idx].nullable = false;
                        }
                    }
                }
            }
            TableConstraint::ForeignKey(fk) => {
                let ref_table = object_name_last(&fk.foreign_table);
                // Pair each local column with the referenced column at the same
                // position; PostgreSQL requires matching arity. A bare `FOREIGN KEY
                // (x) REFERENCES t` with no referred column list cannot name a
                // ref_column, so it is skipped (we never invent one).
                for (i, local) in fk.columns.iter().enumerate() {
                    if let Some(ref_col) = fk.referred_columns.get(i) {
                        table.foreign_keys.push(ForeignKey {
                            column: local.value.clone(),
                            ref_table: ref_table.clone(),
                            ref_column: ref_col.value.clone(),
                        });
                    }
                }
            }
            // CHECK / UNIQUE / INDEX and other table constraints carry no
            // node/edge fact this milestone — skipped.
            _ => {}
        }
    }

    table
}

/// Build a [`ColumnDef`] from a parsed column, returning any inline-`REFERENCES`
/// foreign key alongside it.
fn build_column(col: &SqlColumnDef) -> (ColumnDef, Option<ForeignKey>) {
    let mut nullable = true;
    let mut primary_key = false;
    let mut inline_fk = None;

    for opt in &col.options {
        match &opt.option {
            ColumnOption::NotNull => nullable = false,
            ColumnOption::PrimaryKey(_) => {
                primary_key = true;
                nullable = false;
            }
            ColumnOption::ForeignKey(fkc) => {
                // Inline `col … REFERENCES foreign_table(ref_col)`. The FK columns
                // list is the single declaring column; the referred column is the
                // first (and only) of `referred_columns` when present.
                if let Some(ref_col) = fkc.referred_columns.first() {
                    inline_fk = Some(ForeignKey {
                        column: col.name.value.clone(),
                        ref_table: object_name_last(&fkc.foreign_table),
                        ref_column: ref_col.value.clone(),
                    });
                }
            }
            _ => {}
        }
    }

    (
        ColumnDef {
            name: col.name.value.clone(),
            sql_type: col.data_type.to_string(),
            nullable,
            primary_key,
        },
        inline_fk,
    )
}

/// Apply one `ALTER TABLE` operation to the table built so far. ADD COLUMN appends
/// (with its inline FK, if any); DROP COLUMN removes the column (and any FK on it);
/// RENAME COLUMN renames in place (updating any FK that referenced the old name).
/// Operations we do not model (ADD CONSTRAINT, ALTER COLUMN TYPE, …) are skipped.
fn apply_alter_op(table: &mut TableDef, op: &AlterTableOperation) {
    match op {
        AlterTableOperation::AddColumn { column_def, .. } => {
            let (column, inline_fk) = build_column(column_def);
            // Idempotent on a re-applied ADD (e.g. `ADD COLUMN IF NOT EXISTS` over a
            // column already present): replace in place rather than duplicate.
            match table.column_index(&column.name) {
                Some(idx) => table.columns[idx] = column,
                None => table.columns.push(column),
            }
            if let Some(fk) = inline_fk {
                table.foreign_keys.push(fk);
            }
        }
        AlterTableOperation::DropColumn { column_names, .. } => {
            for name in column_names {
                let dropped = name.value.as_str();
                table.columns.retain(|c| c.name != dropped);
                // An FK on (or referencing within this table to) the dropped column
                // is no longer valid — drop FKs whose local column was removed.
                table.foreign_keys.retain(|fk| fk.column != dropped);
            }
        }
        AlterTableOperation::RenameColumn {
            old_column_name,
            new_column_name,
        } => {
            let old = old_column_name.value.as_str();
            let new = new_column_name.value.clone();
            if let Some(idx) = table.column_index(old) {
                table.columns[idx].name = new.clone();
            }
            // Keep any FK whose local column was renamed pointing at the new name.
            for fk in &mut table.foreign_keys {
                if fk.column == old {
                    fk.column = new.clone();
                }
            }
        }
        // ADD CONSTRAINT, ALTER COLUMN, DROP CONSTRAINT, RENAME TABLE, etc. carry
        // no modelled fact this milestone — skipped (never errored).
        _ => {}
    }
}

/// The last segment of a (possibly schema-qualified) object name, unquoted —
/// `app.widgets` → `widgets`, `"Orders"` → `Orders`. Using the bare `value` (which
/// strips the quote style) so a quoted `"Orders"` and an unquoted `Orders`
/// reconcile, and an FK `foreign_table` matches a table declared either way.
fn object_name_last(name: &ObjectName) -> String {
    name.0
        .last()
        .and_then(|part| part.as_ident())
        .map(|id| id.value.clone())
        // An object name is always at least one part in practice; fall back to the
        // whole rendered name rather than panic on an exotic shape.
        .unwrap_or_else(|| name.to_string())
}

/// Recover a bare column name from an [`IndexColumn`] (a table-level PK/UNIQUE
/// column entry): its `column` is an `OrderByExpr` whose `expr` is normally an
/// `Expr::Identifier`. A non-identifier expression (a functional index) yields
/// `None` — we never invent a column name.
fn index_col_name(ic: &IndexColumn) -> Option<String> {
    match &ic.column.expr {
        Expr::Identifier(id) => Some(id.value.clone()),
        Expr::CompoundIdentifier(parts) => parts.last().map(|p| p.value.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Extract `content` (panicking with the parse error on failure so a regression
    /// reads clearly).
    fn extract(content: &str) -> SchemaModel {
        SqlSchemaAdapter
            .extract("schema.sql", content)
            .unwrap_or_else(|e| panic!("extract: {e}"))
    }

    /// The table named `name` in the model.
    fn table<'a>(m: &'a SchemaModel, name: &str) -> &'a TableDef {
        m.tables
            .iter()
            .find(|t| t.name == name)
            .unwrap_or_else(|| panic!("table {name} not found in {:?}", m.tables))
    }

    /// The column named `name` in `t`.
    fn column<'a>(t: &'a TableDef, name: &str) -> &'a ColumnDef {
        t.columns
            .iter()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("column {name} not found in {:?}", t.columns))
    }

    // ── CREATE TABLE: types, nullability, pk, inline FK ──

    #[test]
    fn create_table_extracts_typed_columns_pk_nullable_and_fk() {
        let m = extract(concat!(
            "CREATE TABLE users (\n",
            "  id BIGINT PRIMARY KEY,\n",
            "  email VARCHAR(255) NOT NULL,\n",
            "  org_id BIGINT REFERENCES orgs(id),\n",
            "  created_at TIMESTAMPTZ\n",
            ");\n",
        ));
        let users = table(&m, "users");

        // The pk column is non-nullable and flagged.
        let id = column(users, "id");
        assert_eq!(id.sql_type, "BIGINT");
        assert!(id.primary_key, "id is the primary key");
        assert!(!id.nullable, "a primary-key column is NOT NULL");

        // NOT NULL is honoured; the type is rendered as written.
        let email = column(users, "email");
        assert_eq!(email.sql_type, "VARCHAR(255)");
        assert!(!email.nullable);
        assert!(!email.primary_key);

        // A bare column is nullable.
        let created = column(users, "created_at");
        assert!(created.nullable);

        // The inline REFERENCES is an explicit FK fact.
        assert_eq!(
            users.foreign_keys,
            vec![ForeignKey {
                column: "org_id".into(),
                ref_table: "orgs".into(),
                ref_column: "id".into(),
            }],
            "inline REFERENCES is recorded as a foreign key"
        );
    }

    // ── ClickHouse: recover a CREATE TABLE whose ClickHouse tail defeats sqlparser ──

    #[test]
    fn clickhouse_create_table_with_engine_tail_recovers_its_columns() {
        // ENGINE / PARTITION BY / TTL / SETTINGS and ClickHouse types defeat BOTH the
        // Postgres and ClickHouse dialects. The column-list recovery should still yield
        // the table + columns (the engine/settings tail is not graph-relevant), with
        // nested-paren types (Decimal(10,2), Map(String,String)) preserved intact.
        let m = extract(concat!(
            "CREATE TABLE events (\n",
            "  id UInt64,\n",
            "  amount Decimal(10, 2),\n",
            "  tags Map(String, String),\n",
            "  created_at DateTime64(3)\n",
            ") ENGINE = MergeTree\n",
            "PARTITION BY toYYYYMM(created_at)\n",
            "ORDER BY (created_at, id)\n",
            "TTL created_at + INTERVAL 90 DAY\n",
            "SETTINGS index_granularity = 8192;\n",
        ));
        let events = table(&m, "events");
        let names: Vec<&str> = events.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["id", "amount", "tags", "created_at"],
            "ClickHouse CREATE TABLE columns recovered from the column-list prefix"
        );
    }

    #[test]
    fn clickhouse_kafka_engine_table_recovers_columns() {
        // A Kafka-engine table (ENGINE = Kafka SETTINGS …) also defeats both dialects;
        // the same column-list recovery applies — its declared columns are the schema.
        let m = extract(concat!(
            "CREATE TABLE kafka_in (id UInt64, msg String) ENGINE = Kafka\n",
            "SETTINGS kafka_broker_list = 'h:9092', kafka_topic_list = 't', kafka_format = 'JSONEachRow';\n",
        ));
        let names: Vec<&str> = table(&m, "kafka_in")
            .columns
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(names, vec!["id", "msg"]);
    }

    #[test]
    fn unrecoverable_clickhouse_statement_is_skipped_never_invents_a_table() {
        // A recoverable table sits beside a CREATE USER (no column list, parses under no
        // dialect). The table is kept; the user statement is skipped, NOT turned into a
        // phantom table — honesty (R1/R5) holds through the recovery path.
        let m = extract(concat!(
            "CREATE TABLE good (id UInt64) ENGINE = MergeTree ORDER BY id SETTINGS x = 1;\n",
            "CREATE USER alice IDENTIFIED WITH sha256_password BY 'secret';\n",
        ));
        let table_names: Vec<&str> = m.tables.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(
            table_names,
            vec!["good"],
            "only the real table; CREATE USER invents nothing"
        );
        assert_eq!(
            m.skipped_statements, 1,
            "the unparseable CREATE USER is counted as skipped"
        );
    }

    #[test]
    fn clickhouse_materialized_view_parses_and_is_not_a_skip() {
        // `CREATE MATERIALIZED VIEW … TO …` fails Postgres but parses under the ClickHouse
        // dialect, so the file is not a failure and the statement is not skipped; a view is
        // not a table, so it adds no table node (the co-located table is still extracted).
        let m = extract(concat!(
            "CREATE TABLE dest (id UInt64) ENGINE = MergeTree() ORDER BY id;\n",
            "CREATE MATERIALIZED VIEW mv TO dest AS SELECT id FROM src;\n",
        ));
        let table_names: Vec<&str> = m.tables.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(table_names, vec!["dest"]);
        assert_eq!(
            m.skipped_statements, 0,
            "the MV parses under the ClickHouse dialect, not skipped"
        );
    }

    // ── ALTER: cumulative ADD + RENAME builds the latest declared schema ──

    #[test]
    fn alter_add_then_rename_column_is_cumulative() {
        let m = extract(concat!(
            "CREATE TABLE users (id BIGINT PRIMARY KEY, email TEXT);\n",
            "ALTER TABLE users ADD COLUMN last_login TIMESTAMPTZ;\n",
            "ALTER TABLE users RENAME COLUMN email TO email_address;\n",
        ));
        let users = table(&m, "users");
        let names: Vec<&str> = users.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["id", "email_address", "last_login"],
            "ADD appends, RENAME renames in place — the latest declared shape"
        );
        // The renamed column keeps its type; the added column has its declared type.
        assert_eq!(column(users, "email_address").sql_type, "TEXT");
        assert_eq!(column(users, "last_login").sql_type, "TIMESTAMPTZ");
    }

    #[test]
    fn alter_drop_column_removes_it_and_its_fk() {
        let m = extract(concat!(
            "CREATE TABLE accounts (\n",
            "  id BIGINT PRIMARY KEY,\n",
            "  org_id BIGINT REFERENCES orgs(id),\n",
            "  note TEXT\n",
            ");\n",
            "ALTER TABLE accounts DROP COLUMN org_id;\n",
        ));
        let accounts = table(&m, "accounts");
        let names: Vec<&str> = accounts.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["id", "note"], "the dropped column is gone");
        assert!(
            accounts.foreign_keys.is_empty(),
            "the FK on the dropped column is gone too"
        );
    }

    // ── Multi-table FK graph + table-level constraints ──

    #[test]
    fn table_level_pk_and_fk_constraints_across_tables() {
        let m = extract(concat!(
            "CREATE TABLE users (id BIGINT PRIMARY KEY, email TEXT NOT NULL);\n",
            "CREATE TABLE orgs (id BIGINT PRIMARY KEY);\n",
            "CREATE TABLE memberships (\n",
            "  user_id BIGINT NOT NULL,\n",
            "  org_id BIGINT NOT NULL,\n",
            "  role TEXT,\n",
            "  PRIMARY KEY (user_id, org_id),\n",
            "  FOREIGN KEY (user_id) REFERENCES users(id),\n",
            "  CONSTRAINT fk_org FOREIGN KEY (org_id) REFERENCES orgs(id)\n",
            ");\n",
        ));
        assert_eq!(m.tables.len(), 3, "three tables declared");

        let mem = table(&m, "memberships");
        // The composite PK flags BOTH columns.
        assert!(column(mem, "user_id").primary_key);
        assert!(column(mem, "org_id").primary_key);
        assert!(!column(mem, "role").primary_key);

        // Both table-level FKs are recorded, in declared order.
        assert_eq!(
            mem.foreign_keys,
            vec![
                ForeignKey {
                    column: "user_id".into(),
                    ref_table: "users".into(),
                    ref_column: "id".into(),
                },
                ForeignKey {
                    column: "org_id".into(),
                    ref_table: "orgs".into(),
                    ref_column: "id".into(),
                },
            ]
        );
    }

    // ── A migration with non-table statements interleaved → tables still out ──

    #[test]
    fn migration_with_index_and_extension_still_yields_tables() {
        let m = extract(concat!(
            "CREATE EXTENSION IF NOT EXISTS \"uuid-ossp\";\n",
            "CREATE TABLE orgs (id BIGINT PRIMARY KEY, name TEXT NOT NULL);\n",
            "CREATE INDEX idx_orgs_name ON orgs (name);\n",
            "CREATE TABLE accounts (\n",
            "  id BIGINT PRIMARY KEY,\n",
            "  org_id BIGINT NOT NULL REFERENCES orgs(id)\n",
            ");\n",
            "INSERT INTO orgs (id, name) VALUES (1, 'acme');\n",
        ));
        let names: Vec<&str> = m.tables.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["orgs", "accounts"],
            "the non-table statements (EXTENSION/INDEX/INSERT) are skipped, not errored"
        );
        // The cross-table FK survived the interleaving.
        assert_eq!(table(&m, "accounts").foreign_keys[0].ref_table, "orgs");
    }

    #[test]
    fn dollar_quoted_function_body_does_not_break_the_file() {
        // A migration with a plpgsql trigger function between two tables: the
        // dollar-quoted body must not derail the parse of the surrounding DDL.
        let m = extract(concat!(
            "CREATE TABLE t (id INT PRIMARY KEY);\n",
            "CREATE FUNCTION bump() RETURNS trigger AS $$\n",
            "BEGIN\n",
            "  NEW.updated_at := now();\n",
            "  RETURN NEW;\n",
            "END;\n",
            "$$ LANGUAGE plpgsql;\n",
            "CREATE TABLE u (id INT PRIMARY KEY);\n",
        ));
        let names: Vec<&str> = m.tables.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["t", "u"],
            "both tables around the function body"
        );
    }

    // ── Statement splitter: adversarial `;`-boundary cases ──
    //
    // The splitter is the crux of the robustness fix: a `;` is a separator ONLY at
    // the top level — never inside a string, a dollar-quoted body, or a comment.
    // These pin every state the lexer tracks; the per-statement parse rides on top.

    /// Bare statement texts (whitespace-trimmed, blanks dropped) the splitter
    /// produces — the unit under test for the boundary cases.
    fn split(content: &str) -> Vec<String> {
        split_sql_statements(content)
            .into_iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    }

    #[test]
    fn splitter_plain_statements_split_on_top_level_semicolons() {
        assert_eq!(
            split("CREATE TABLE a (id INT); CREATE TABLE b (id INT);"),
            vec!["CREATE TABLE a (id INT)", "CREATE TABLE b (id INT)"]
        );
        // A final statement with no trailing `;` is still emitted.
        assert_eq!(
            split("CREATE TABLE a (id INT);\nCREATE TABLE b (id INT)"),
            vec!["CREATE TABLE a (id INT)", "CREATE TABLE b (id INT)"]
        );
    }

    #[test]
    fn splitter_semicolon_inside_single_quoted_string_does_not_split() {
        // A `';'` literal: the inner `;` is inside the string, so this is ONE
        // statement, not two.
        let parts = split("INSERT INTO t (s) VALUES (';'); CREATE TABLE u (id INT)");
        assert_eq!(
            parts,
            vec!["INSERT INTO t (s) VALUES (';')", "CREATE TABLE u (id INT)"],
            "a `;` inside '…' is text, not a separator"
        );
    }

    #[test]
    fn splitter_doubled_quote_escape_keeps_string_open() {
        // The `''` escape: `'a;''b;'` is a single string containing a literal quote,
        // so neither inner `;` splits.
        let parts = split("INSERT INTO t VALUES ('a;''b;'); CREATE TABLE u (id INT)");
        assert_eq!(
            parts,
            vec!["INSERT INTO t VALUES ('a;''b;')", "CREATE TABLE u (id INT)"],
            "the '' escape keeps the string open across both inner ;"
        );
    }

    #[test]
    fn splitter_semicolon_inside_dollar_quote_does_not_split() {
        // THE LOAD-BEARING CASE: a `DO $$ BEGIN …; … END $$;` — the inner `;`s are
        // inside the dollar body and must NOT split; only the trailing `;` does.
        let parts = split(concat!(
            "DO $$ BEGIN PERFORM 1; PERFORM 2; END $$;\n",
            "CREATE TABLE u (id INT);\n",
        ));
        assert_eq!(
            parts,
            vec![
                "DO $$ BEGIN PERFORM 1; PERFORM 2; END $$",
                "CREATE TABLE u (id INT)"
            ],
            "inner ; inside $$…$$ are body text, not separators"
        );
    }

    #[test]
    fn splitter_tagged_dollar_quote_matches_exact_tag() {
        // A `$body$…$body$` body must close ONLY on its exact tag — a bare `$$` (or a
        // `;`) inside it does not end it. Here the inner `$$ ;` stays in the body.
        let parts = split(concat!(
            "CREATE FUNCTION f() RETURNS text AS $body$ SELECT '$$'; $body$ LANGUAGE sql;\n",
            "CREATE TABLE u (id INT);\n",
        ));
        assert_eq!(parts.len(), 2, "two statements: the function and the table");
        assert_eq!(parts[1], "CREATE TABLE u (id INT)");
        assert!(
            parts[0].contains("$body$") && parts[0].ends_with("LANGUAGE sql"),
            "the tagged body closed on $body$, not the inner $$: {:?}",
            parts[0]
        );
    }

    #[test]
    fn splitter_semicolon_in_line_comment_does_not_split() {
        // A `-- ;` line comment: the `;` is commentary, not a separator. The comment
        // text rides along in the NEXT segment (a leading comment sqlparser ignores),
        // so the contract is: no spurious third statement, and BOTH tables extract.
        let content = concat!(
            "CREATE TABLE a (id INT); -- a trailing ; in a comment\n",
            "CREATE TABLE b (id INT);\n",
        );
        let parts = split(content);
        assert_eq!(
            parts.len(),
            2,
            "the `;` in the -- comment does not start a third statement: {parts:?}"
        );
        let m = extract(content);
        let names: Vec<&str> = m.tables.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["a", "b"],
            "both tables extract despite the comment ;"
        );
    }

    #[test]
    fn splitter_semicolon_in_block_comment_does_not_split_and_comments_nest() {
        // A `/* ; */` block comment hides its `;`; and Postgres block comments NEST,
        // so `/* /* ; */ ; */` is ONE comment — no `;` inside it splits. (The comment
        // text rides in the next segment; the contract is no spurious split + both
        // tables extract.)
        let content = concat!(
            "CREATE TABLE a (id INT); /* a ; and /* nested ; */ still one ; */\n",
            "CREATE TABLE b (id INT);\n",
        );
        let parts = split(content);
        assert_eq!(
            parts.len(),
            2,
            "nested block comment hides every inner ;: {parts:?}"
        );
        let m = extract(content);
        let names: Vec<&str> = m.tables.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["a", "b"],
            "both tables extract despite the nested comment ;"
        );
    }

    #[test]
    fn splitter_dollar_placeholder_is_not_a_dollar_quote() {
        // asyncpg-style `$1` placeholders are NOT dollar quotes (no closing `$`), so
        // the `;` after them still splits normally.
        let parts = split("UPDATE t SET a = $1 WHERE id = $2; CREATE TABLE u (id INT)");
        assert_eq!(
            parts,
            vec![
                "UPDATE t SET a = $1 WHERE id = $2",
                "CREATE TABLE u (id INT)"
            ],
            "$1/$2 are placeholders, not dollar quotes"
        );
    }

    #[test]
    fn splitter_preserves_non_ascii_bytes_in_strings() {
        // Encoding-safety: a multi-byte UTF-8 literal must survive the byte-level
        // lexer intact (it slices the original &str, never reassembles bytes).
        let parts = split("INSERT INTO t VALUES ('café ☕ ;'); CREATE TABLE u (id INT)");
        assert_eq!(
            parts,
            vec![
                "INSERT INTO t VALUES ('café ☕ ;')",
                "CREATE TABLE u (id INT)"
            ],
            "multi-byte UTF-8 inside a string is byte-exact and its ; does not split"
        );
    }

    // ── Double-quoted identifiers and E-strings: two more lexer states ──
    //
    // A `"…"` identifier is its own state (a `'`/`;` inside it is inert; the only
    // escape is the doubled `""`). A Postgres `E'…'` string uses backslash escapes,
    // so `\'`/`\\` do not close it. Both bugs SILENTLY LOST a real top-level `;` —
    // collapsing or truncating the file — before these states existed.

    #[test]
    fn splitter_double_quoted_identifier_with_apostrophe_does_not_split() {
        // A `'` inside a `"…"` identifier is inert — it must NOT flip the lexer into
        // single-quote-string state and swallow the two real top-level `;`.
        let parts = split(concat!(
            "CREATE TABLE \"it's\" (id INT); ",
            "CREATE TABLE u (id INT); ",
            "CREATE TABLE v (id INT)",
        ));
        assert_eq!(
            parts.len(),
            3,
            "the ' inside \"it's\" is inert; all three top-level ; split: {parts:?}"
        );
        assert!(parts[0].contains("\"it's\""), "stmt 0 keeps its identifier");
        assert!(parts[1].contains("u"), "stmt 1 is table u");
        assert!(parts[2].contains("v"), "stmt 2 is table v");
    }

    #[test]
    fn splitter_double_quoted_identifier_with_semicolon() {
        // A `;` inside a `"…"` identifier is inert — it must NOT split mid-identifier
        // and lose table `"a;b"`.
        let parts = split("CREATE TABLE \"a;b\" (id INT); CREATE TABLE u (id INT)");
        assert_eq!(
            parts.len(),
            2,
            "the ; inside \"a;b\" is inert; only the top-level ; splits: {parts:?}"
        );
        assert!(
            parts[0].contains("\"a;b\""),
            "the first statement keeps the whole quoted identifier: {:?}",
            parts[0]
        );
    }

    #[test]
    fn splitter_estring_with_escaped_quote_does_not_swallow_separator() {
        // A Postgres E-string: `\'` is an escaped quote, NOT a close. The later real
        // `'` therefore does not re-open a string, and the real `;` after it splits.
        let parts = split("SELECT E'a\\'; b'; CREATE TABLE u (id INT)");
        assert_eq!(
            parts.len(),
            2,
            "the \\' inside E'…' is an escape; the trailing ; splits: {parts:?}"
        );
        // The CREATE must be a CLEAN standalone statement — not buried behind a
        // swallowed E-string tail (`b'; CREATE …`). On the unfixed lexer the `\'`
        // closes early, the later `'` re-opens, and the tail collapses into one
        // segment that merely CONTAINS the CREATE; `starts_with` rejects that.
        assert!(
            parts[1].trim().starts_with("CREATE TABLE u"),
            "the second statement is the CREATE TABLE, not a swallowed E-string tail: {:?}",
            parts[1]
        );
        assert!(
            parts[0].trim().starts_with("SELECT"),
            "the first statement is the whole SELECT E'…': {:?}",
            parts[0]
        );
    }

    #[test]
    fn splitter_estring_double_backslash() {
        // `E'\\'` is an escaped backslash, then a lone `'` closes the string — so the
        // following top-level `;` splits and table u survives.
        let parts = split("SELECT E'\\\\'; CREATE TABLE u (id INT)");
        assert_eq!(
            parts.len(),
            2,
            "E'\\\\' closes after the escaped backslash; the ; splits: {parts:?}"
        );
        assert!(
            parts[1].contains("CREATE TABLE u"),
            "the second statement is the CREATE TABLE: {:?}",
            parts[1]
        );
    }

    #[test]
    fn splitter_normal_string_backslash_is_literal_unchanged() {
        // Regression guard: a NON-E string keeps standard SQL behaviour — a backslash
        // is ordinary text, so in `'a\'` the `'` after `\` CLOSES the string (it is
        // NOT an escaped quote). The `;` after `)` then splits into two statements.
        // If the E-string backslash rule leaked into normal strings, the `'` would be
        // escaped, the string would stay open, the `;` would be swallowed, and this
        // would collapse to ONE statement — the assertion that distinguishes the modes.
        let parts = split("INSERT INTO t VALUES ('a\\'); CREATE TABLE u (id INT)");
        assert_eq!(
            parts.len(),
            2,
            "backslash is literal in a normal string; the ' closes and the ; splits: {parts:?}"
        );
        assert!(
            parts[1].contains("CREATE TABLE u"),
            "the second statement is the CREATE TABLE: {:?}",
            parts[1]
        );
    }

    #[test]
    fn double_quote_doubled_escape() {
        // The `""` escape inside a `"…"` identifier: `"a""b"` is one identifier
        // containing a literal quote, so the following top-level `;` splits normally.
        let parts = split("CREATE TABLE \"a\"\"b\" (id INT); CREATE TABLE u (id INT)");
        assert_eq!(
            parts.len(),
            2,
            "the \"\" escape keeps the identifier open; the top-level ; splits: {parts:?}"
        );
        assert!(
            parts[0].contains("\"a\"\"b\""),
            "the first statement keeps the doubled-quote identifier: {:?}",
            parts[0]
        );
    }

    // ── The robustness payoff: a mixed file keeps its tables, skips the bad parts ──

    #[test]
    fn quoted_identifier_apostrophe_with_do_block_keeps_both_tables_and_is_not_failed() {
        // Coverage-level guard for BUG 1 end-to-end: a CREATE TABLE whose name is a
        // double-quoted identifier containing an apostrophe (`"it's"`), an inline FK,
        // a `DO $$ … $$` block (which sqlparser skips), and a trailing CREATE TABLE.
        // BOTH tables must be extracted, the DO block skipped (counted), and the file
        // must NOT be a DataError::Parse failure. Before the double-quote state, the
        // `'` in `"it's"` swallowed the real `;`, collapsing the file.
        let m = extract(concat!(
            "CREATE TABLE \"it's\" (id INT, ref INT REFERENCES u(id)); ",
            "DO $$ BEGIN END $$; ",
            "CREATE TABLE u (id INT)",
        ));
        let names: Vec<&str> = m.tables.iter().map(|t| t.name.as_str()).collect();
        assert!(
            names.contains(&"it's") && names.contains(&"u"),
            "both tables survive the apostrophe-identifier + DO block: {names:?}"
        );
        assert!(
            m.skipped_statements >= 1,
            "the DO block is counted as skipped, got {}",
            m.skipped_statements
        );
        // extract() panics on Err, so reaching here proves the file is NOT a failure.
    }

    #[test]
    fn mixed_file_keeps_tables_skips_unparseable_do_block_and_records_skip() {
        // THE fix in one fixture: a CREATE TABLE, a `DO $$ … ; … $$;` PL/pgSQL block
        // (which sqlparser cannot parse) with an inner `;`, a dollar-quoted COMMENT,
        // and a trailing CREATE TABLE. BOTH tables must be extracted, the DO block
        // skipped (NOT failing the file), and the skipped count recorded. On the OLD
        // whole-file logic the DO block failed the ENTIRE file → zero tables.
        let m = extract(concat!(
            "CREATE TABLE first (id BIGINT PRIMARY KEY, name TEXT NOT NULL);\n",
            "DO $$\n",
            "BEGIN\n",
            "  IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'app') THEN\n",
            "    CREATE ROLE app;\n",
            "  END IF;\n",
            "END\n",
            "$$;\n",
            "COMMENT ON TABLE first IS $tag$a table; with a ; in its comment$tag$;\n",
            "CREATE TABLE second (id BIGINT PRIMARY KEY, first_id BIGINT REFERENCES first(id));\n",
        ));

        let names: Vec<&str> = m.tables.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["first", "second"],
            "both CREATE TABLEs survive around the DO block and the dollar-quoted COMMENT"
        );
        // The cross-table FK from the trailing CREATE TABLE is intact.
        assert_eq!(
            table(&m, "second").foreign_keys,
            vec![ForeignKey {
                column: "first_id".into(),
                ref_table: "first".into(),
                ref_column: "id".into(),
            }]
        );
        // The unparseable statements were SKIPPED (counted), not errored. The DO
        // block is one skip; the COMMENT may or may not parse depending on the
        // dialect, so assert at least the DO block is counted.
        assert!(
            m.skipped_statements >= 1,
            "the DO block (and any other unparseable statement) is counted as skipped, got {}",
            m.skipped_statements
        );
        // And crucially the file did NOT fail — extract() returned Ok (it panics on
        // Err), so a mixed file is a SUCCESS that yields its tables.
    }

    #[test]
    fn mixed_file_is_not_a_failure_unlike_old_whole_file_parse() {
        // Prove the red→green delta directly: the SAME content, parsed as ONE blob
        // (the OLD logic), fails; split statement-by-statement (the NEW logic), it
        // succeeds with both tables. This is the regression guard for the fix.
        let content = concat!(
            "CREATE TABLE keep_me (id INT PRIMARY KEY);\n",
            "DO $$ BEGIN PERFORM 1; END $$;\n",
            "CREATE TABLE keep_me_too (id INT PRIMARY KEY);\n",
        );
        // OLD behaviour (whole-file): the DO block defeats sqlparser → the file fails.
        assert!(
            Parser::parse_sql(&PostgreSqlDialect {}, content).is_err(),
            "the whole-file parse still fails on the DO block (the bug we fix)"
        );
        // NEW behaviour (per-statement): both tables extracted, file not failed.
        let m = extract(content);
        let names: Vec<&str> = m.tables.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["keep_me", "keep_me_too"]);
        assert!(m.skipped_statements >= 1);
    }

    #[test]
    fn wholly_unparseable_ddl_file_still_fails_with_skip_count() {
        // A file that carries the DDL signal but yields ZERO parseable statements is
        // still a failure (honest: it looked like schema, nothing parsed). The error
        // message mentions the skipped count.
        let err = SqlSchemaAdapter
            .extract(
                "all_bad.sql",
                "CREATE TABLE ( oops;\nDO $$ BEGIN garbage END $$;\n",
            )
            .unwrap_err();
        match err {
            DataError::Parse { path, msg } => {
                assert_eq!(path, "all_bad.sql");
                assert!(!msg.is_empty());
            }
        }
    }

    #[test]
    fn a_clean_file_records_zero_skipped_statements() {
        // The common case is unchanged: a wholly-valid migration parses identically
        // and reports zero skips.
        let m = extract(concat!(
            "CREATE TABLE a (id INT PRIMARY KEY);\n",
            "ALTER TABLE a ADD COLUMN n TEXT;\n",
            "CREATE TABLE b (id INT PRIMARY KEY);\n",
        ));
        assert_eq!(m.tables.len(), 2);
        assert_eq!(
            m.skipped_statements, 0,
            "a clean file skips nothing (no regression for valid migrations)"
        );
    }

    // ── Honest degradation: malformed → Err, non-DDL → detects=false ──

    #[test]
    fn malformed_sql_is_a_parse_error_not_a_panic() {
        let err = SqlSchemaAdapter
            .extract("broken.sql", "CREATE TABLE ( oops")
            .unwrap_err();
        match err {
            DataError::Parse { path, msg } => {
                assert_eq!(path, "broken.sql");
                assert!(!msg.is_empty(), "the parse error carries a reason");
            }
        }
    }

    #[test]
    fn detects_a_ddl_file_but_not_a_query_only_or_non_sql_file() {
        // A real DDL file is detected.
        assert!(
            SqlSchemaAdapter.detects("schema.sql", "CREATE TABLE t (id INT PRIMARY KEY);"),
            "a CREATE TABLE file is a schema file"
        );
        // A `.sql` of pure SELECT/INSERT declares no table — not a schema file.
        assert!(
            !SqlSchemaAdapter.detects("query.sql", "SELECT * FROM t;\nINSERT INTO t VALUES (1);"),
            "a query-only .sql declares no tables"
        );
        // A non-SQL file that happens to be named `.sql` does not parse → not detected.
        assert!(
            !SqlSchemaAdapter.detects("notes.sql", "this is just prose, not sql at all"),
            "a non-SQL .sql file is not a schema file"
        );
        // Malformed-but-DDL-shaped is detects=false (the indexer surfaces it via
        // extract's error instead).
        assert!(
            !SqlSchemaAdapter.detects("broken.sql", "CREATE TABLE ( oops"),
            "a malformed DDL file fails detection (surfaced as a diagnostic by extract)"
        );
    }

    #[test]
    fn empty_and_whitespace_files_yield_no_tables_without_panic() {
        // Pathological-but-benign inputs: no panic, no tables, no error for empty.
        assert!(extract("").tables.is_empty());
        assert!(extract("   \n\t  ").tables.is_empty());
        assert!(extract("-- just a comment\n").tables.is_empty());
        assert!(!SqlSchemaAdapter.detects("e.sql", ""));
    }

    #[test]
    fn quoted_and_schema_qualified_names_are_unquoted_to_last_segment() {
        // A quoted, mixed-case table + a schema-qualified table; the FK target uses
        // the bare value so it reconciles with the declared name.
        let m = extract(concat!(
            "CREATE TABLE \"Users\" (\"Id\" BIGINT PRIMARY KEY);\n",
            "CREATE TABLE app.orders (\n",
            "  id BIGINT PRIMARY KEY,\n",
            "  user_id BIGINT REFERENCES \"Users\"(\"Id\")\n",
            ");\n",
        ));
        // The quoted name is unquoted to its raw value.
        let _ = table(&m, "Users");
        // The schema-qualified name keeps only its last segment.
        let orders = table(&m, "orders");
        assert_eq!(
            orders.foreign_keys[0],
            ForeignKey {
                column: "user_id".into(),
                ref_table: "Users".into(),
                ref_column: "Id".into(),
            },
            "FK to a quoted table reconciles by the bare (unquoted) value"
        );
    }

    #[test]
    fn alter_on_unknown_table_is_skipped_never_invents_a_table() {
        // An ALTER whose CREATE we were never given does not fabricate a table.
        let m = extract("ALTER TABLE ghost ADD COLUMN x INT;\n");
        assert!(
            m.tables.is_empty(),
            "an ALTER alone never invents a table (honest absence)"
        );
    }

    #[test]
    fn determinism_same_input_same_model() {
        let sql = "CREATE TABLE t (id INT PRIMARY KEY, ref_id INT REFERENCES o(id));\n";
        assert_eq!(extract(sql), extract(sql), "extraction is deterministic");
    }

    #[test]
    fn postgres_specific_types_render_losslessly() {
        let m = extract(concat!(
            "CREATE TABLE t (\n",
            "  id SERIAL PRIMARY KEY,\n",
            "  data JSONB,\n",
            "  tags TEXT[],\n",
            "  qty NUMERIC(10,2),\n",
            "  u UUID\n",
            ");\n",
        ));
        let t = table(&m, "t");
        assert_eq!(column(t, "id").sql_type, "SERIAL");
        assert_eq!(column(t, "data").sql_type, "JSONB");
        assert_eq!(column(t, "tags").sql_type, "TEXT[]");
        assert_eq!(column(t, "qty").sql_type, "NUMERIC(10,2)");
        assert_eq!(column(t, "u").sql_type, "UUID");
    }

    // ── parse_table_refs: code→table DML extraction (Slice 16, D3, M2) ──

    /// A `(table, access)` pair, terse for the assertions below.
    fn r(table: &str, access: SqlAccess) -> TableRef {
        TableRef {
            table: table.into(),
            access,
        }
    }

    #[test]
    fn select_from_is_a_read() {
        assert_eq!(
            parse_table_refs("SELECT email FROM users WHERE id = 1"),
            vec![r("users", SqlAccess::Read)]
        );
    }

    #[test]
    fn select_join_reads_both_tables() {
        let refs = parse_table_refs("SELECT u.id FROM users u JOIN orgs o ON o.id = u.org_id");
        assert_eq!(
            refs,
            vec![r("users", SqlAccess::Read), r("orgs", SqlAccess::Read)],
            "the base table and the joined table are both Reads"
        );
    }

    #[test]
    fn insert_is_a_write() {
        assert_eq!(
            parse_table_refs("INSERT INTO orders (id) VALUES (1)"),
            vec![r("orders", SqlAccess::Write)]
        );
    }

    #[test]
    fn insert_select_writes_target_and_reads_source() {
        let refs = parse_table_refs("INSERT INTO orders (id) SELECT id FROM staging_orders");
        assert_eq!(
            refs,
            vec![
                r("orders", SqlAccess::Write),
                r("staging_orders", SqlAccess::Read)
            ],
            "INSERT … SELECT writes the target AND reads the source"
        );
    }

    #[test]
    fn update_is_a_write() {
        assert_eq!(
            parse_table_refs("UPDATE users SET last_login = now() WHERE id = 1"),
            vec![r("users", SqlAccess::Write)]
        );
    }

    #[test]
    fn delete_from_is_a_write() {
        assert_eq!(
            parse_table_refs("DELETE FROM sessions WHERE expired = true"),
            vec![r("sessions", SqlAccess::Write)]
        );
    }

    #[test]
    fn qualified_and_quoted_names_reduce_to_bare_table() {
        // The same normalization DDL uses, so a code ref reconciles with a declared
        // table whether the DDL named it `widgets`, `app.widgets`, or `"Widgets"`.
        assert_eq!(
            parse_table_refs("SELECT * FROM app.widgets"),
            vec![r("widgets", SqlAccess::Read)]
        );
        assert_eq!(
            parse_table_refs("SELECT * FROM \"Orders\""),
            vec![r("Orders", SqlAccess::Read)]
        );
    }

    #[test]
    fn unparseable_or_non_dml_fragment_yields_empty_never_errors() {
        // A partial fragment (the constant part of a concatenated query), a DDL
        // statement, and prose all yield NO refs — never an error, never a guess.
        assert!(parse_table_refs("SELECT * FROM ").is_empty());
        assert!(parse_table_refs("\"DELETE FROM \"").is_empty());
        assert!(parse_table_refs("CREATE TABLE t (id INT)").is_empty());
        assert!(parse_table_refs("just some prose here").is_empty());
        assert!(parse_table_refs("").is_empty());
    }

    #[test]
    fn determinism_same_fragment_same_refs() {
        let sql = "SELECT a FROM t1 JOIN t2 ON t1.id = t2.id";
        assert_eq!(parse_table_refs(sql), parse_table_refs(sql));
    }
}
