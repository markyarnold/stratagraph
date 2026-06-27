//! Data-plane assembly: typed `Table`/`Column` nodes + `HasColumn` containment
//! edges + `ForeignKey` reference edges, from the SQL schemas the data plane
//! extracted.
//!
//! This is the data-plane analogue of `infra.rs`'s `build_infra_plane`. It is a
//! pure, deterministic function of the detected [`SchemaModel`]s, the code files'
//! captured SQL candidates, and the graph built so far — no IO — so it is
//! unit-testable in isolation and reproducible (R3). It runs LAST (after the
//! code/contract/infra planes) so the M2 code→table `Reads`/`Writes` edges can
//! target the code symbol/module nodes those planes built.
//!
//! **Honest provenance (R1).** Every node and edge is a **fact** → `Extracted` 0.95
//! (the EXTRACTED band floor). A `CREATE TABLE` / its cumulative `ALTER` declares the
//! table and columns; a foreign-key edge is emitted only from an explicit
//! `REFERENCES` / table-level `FOREIGN KEY` **and only when the referenced `Column`
//! node exists**; an M2 `Reads`/`Writes` edge is emitted only when a raw-SQL literal
//! in code names a table that matches a declared `Table`. A FK or a code reference to
//! a table/column the DDL never declares produces **no edge** — surfaced by its
//! absence and tallied in coverage (`*_unresolved`), never invented. Dynamic SQL
//! (concatenated/interpolated, not a single literal) is not captured upstream, so it
//! contributes no edge either. Nothing is `Inferred` this milestone (ORM
//! convention-name inference is deferred to M2b).

use strata_core::{
    Confidence, Edge, EdgeKind, Graph, Node, NodeKind, OrmModelHint, Provenance, Span,
    SqlCandidate, Uid,
};
use strata_data::{SchemaModel, SqlAccess, TableDef};

/// The language/plane tag for data-plane UIDs (distinct from code `"ts"`, contract
/// `"contract"`, and infra `"infra"`).
const DATA_LANG: &str = "data";

/// Every data-plane node and edge is an explicit-DDL fact → `Extracted` 0.95, the
/// EXTRACTED band floor. M1 infers nothing: a `CREATE TABLE` column/type/constraint
/// and an explicit `REFERENCES`/`FOREIGN KEY` are facts. (ORM convention-derived
/// columns — an `Inferred` tier — are M2.)
pub const CONF_DATA_FACT: f32 = 0.95;

/// An ORM model→table `MapsTo` edge from an **explicit** literal table name is an
/// `Extracted` fact → 0.95, the EXTRACTED band floor (Slice 25, D3, M2b). The
/// literal in `__tablename__ = "users"` / `@Entity("users")` / Django `db_table =
/// "users"` *is* the table name; matching it to a declared `Table` is as much a fact
/// as a raw-SQL `SELECT … FROM users`, so it earns the same band as `Reads`/`Writes`.
/// (A *convention*-derived name — guessing the table from the class name, no explicit
/// literal — would be `Inferred`, not Extracted; that tier is deferred to Phase 2 and
/// not built this slice.)
pub const CONF_ORM_EXPLICIT: f32 = 0.95;

/// Per-repo data-plane link coverage (R4): the headline numbers the committed
/// `docs/accuracy/data-linking.md` report publishes and the CI gate floors.
///
/// Counts are honest: a foreign key whose referenced table/column the parsed DDL
/// never declares is `fks_total` but not `fks_linked` (it is `fks_unresolved`); no
/// edge is invented for it. The `tables`/`columns` counts are the inventory the
/// plane added.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DataLinkCoverage {
    /// SQL schema files detected and extracted in this repo (one per
    /// [`SchemaModel`] with ≥1 table).
    pub schemas_detected: usize,
    /// Files carrying the SQL DDL textual signal that could not be parsed (a
    /// malformed/truncated `.sql`, surfaced as a diagnostic). `build_data_plane`
    /// only sees the schemas that parsed, so it always leaves this `0`; the indexer
    /// sets the real count from extraction (mirrors `InfraLinkCoverage`).
    pub schemas_failed: usize,
    /// Individual SQL statements that the per-statement splitter isolated but
    /// `sqlparser` could not parse (a PL/pgSQL `DO $$…$$`/`CREATE FUNCTION` body, a
    /// dialect-specific statement) **inside files that otherwise parsed**. This is an
    /// *informational* signal, NOT a failure: the surrounding `CREATE TABLE`s were
    /// still extracted (the data-plane robustness fix — one bad statement no longer
    /// drops a whole migration's tables). A wholly-unparseable DDL file is counted in
    /// `schemas_failed`, not here. Summed across this repo's detected schemas.
    pub statements_skipped: usize,
    /// Total tables across all detected schemas (each becomes a `Table` node).
    pub tables_total: usize,
    /// Total columns across all tables (each becomes a `Column` node).
    pub columns_total: usize,
    /// Total foreign keys declared across all tables (the FK-edge candidates).
    pub fks_total: usize,
    /// Foreign keys whose referenced `table.column` exists as a `Column` node → a
    /// `ForeignKey` edge was added.
    pub fks_linked: usize,
    /// Foreign keys whose referenced table/column the parsed DDL never declares →
    /// **no** edge. Surfaced, never invented (R1).
    pub fks_unresolved: usize,
    /// Code→table **read** references parsed from SQL string literals in code
    /// (`SELECT … FROM t` / `JOIN t`) whose table matches a declared `Table` → a
    /// `Reads` edge was added (Slice 16, D3, M2).
    pub reads_linked: usize,
    /// Code→table read references whose table the parsed DDL never declares → **no**
    /// edge. Counted, never invented (R1) — e.g. a query against a table in another
    /// database, or a generic name with no matching schema.
    pub reads_unresolved: usize,
    /// Code→table **write** references parsed from SQL string literals in code
    /// (`INSERT INTO t` / `UPDATE t` / `DELETE FROM t`) whose table matches a
    /// declared `Table` → a `Writes` edge was added (Slice 16, D3, M2).
    pub writes_linked: usize,
    /// Code→table write references whose table the parsed DDL never declares → **no**
    /// edge. Counted, never invented (R1).
    pub writes_unresolved: usize,
    /// ORM model→table hints seen across this repo's code (one per
    /// [`OrmModelHint`](strata_core::OrmModelHint) the analyzers captured) — the
    /// `MapsTo`-edge candidates (Slice 25, D3, M2b).
    pub orm_models_total: usize,
    /// ORM models whose explicit table name matches a declared `Table` AND whose model
    /// class node exists → a `MapsTo` edge was added (Extracted 0.95).
    pub orm_models_linked: usize,
    /// ORM models whose explicit table name the parsed DDL never declares (OR whose
    /// model class node is absent) → **no** edge. Counted, never invented (R1) — the
    /// table may live in another database, or the model is stale.
    pub orm_models_unresolved: usize,
}

/// One code file's SQL candidates, tagged with the language plane (`ts`/`py`/`cs`)
/// its code nodes were built under, so [`build_data_plane`] can reconstruct the
/// exact enclosing code-node UID (`Uid::new(lang, repo, path, fqn-or-"<module>", "")`)
/// a `Reads`/`Writes` edge originates from (Slice 16, D3, M2).
#[derive(Debug, Clone, Copy)]
pub struct CodeSqlFile<'a> {
    /// The language/plane tag the file's code nodes use (`"ts"`/`"py"`/`"cs"`),
    /// matching the `language` component of its symbol/module UIDs.
    pub lang: &'a str,
    /// The repo-relative, `/`-normalized file path (the `path` component of the UID).
    pub path: &'a str,
    /// The SQL-looking string literals the analyzer captured for this file.
    pub candidates: &'a [SqlCandidate],
}

/// One code file's ORM model hints, tagged with the language plane (`ts`/`py`/`cs`)
/// its code nodes were built under — the data-plane analogue of [`CodeSqlFile`] for
/// the M2b model→table `MapsTo` linking (Slice 25, D3). [`build_data_plane`] uses the
/// `lang`/`path` to reconstruct the model class node's UID
/// (`Uid::new(lang, repo, path, model_fqn, "")`) a `MapsTo` edge originates from.
#[derive(Debug, Clone, Copy)]
pub struct CodeOrmFile<'a> {
    /// The language/plane tag the file's code nodes use (`"ts"`/`"py"`/`"cs"`),
    /// matching the `language` component of its symbol/module UIDs.
    pub lang: &'a str,
    /// The repo-relative, `/`-normalized file path (the `path` component of the UID).
    pub path: &'a str,
    /// The ORM model hints (explicit-table-name model classes) the analyzer captured
    /// for this file.
    pub hints: &'a [OrmModelHint],
}

/// Add the data plane to `g`: a `Table` node per table, a `Column` node per column,
/// a `Table —HasColumn→ Column` edge (Extracted), a `Column —ForeignKey→ Column`
/// edge across tables for each resolvable foreign key (Extracted), AND the M2
/// code→table `Reads`/`Writes` edges from each code file's SQL string literals
/// (Extracted). Returns the [`DataLinkCoverage`].
///
/// `schemas` are all the [`SchemaModel`]s detected in this repo (in path order, the
/// caller's responsibility). `code_sql` is each code file's captured SQL candidates,
/// tagged with the language plane its code nodes were built under — the data plane
/// runs LAST (after the code/contract/infra planes), so those code nodes already
/// exist and a `Reads`/`Writes` edge can target them. Pure and deterministic. Nodes
/// are idempotent by UID (a re-add replaces in place); the [`DataLinkCoverage`] is a
/// pure function of the input. (Like the infra builder, the production path always
/// builds into a fresh graph, so edge accumulation on a re-run is not a concern.)
pub fn build_data_plane(
    g: &mut Graph,
    repo_name: &str,
    schemas: &[SchemaModel],
    code_sql: &[CodeSqlFile],
    code_orm: &[CodeOrmFile],
) -> DataLinkCoverage {
    let mut cov = DataLinkCoverage {
        schemas_detected: schemas.len(),
        // Per-statement skips inside files that DID parse — an informational signal
        // (the robustness fix), summed from the models the splitter produced. NOT a
        // failure (that is `schemas_failed`, set by the indexer from extraction).
        statements_skipped: schemas.iter().map(|s| s.skipped_statements).sum(),
        ..DataLinkCoverage::default()
    };

    // ── Pass 1: nodes (Extracted). Idempotent by UID. ──
    // A `Table` node per table; a `Column` node per column. The column UID's fqn is
    // `table.column`, so the same column name in two tables stays distinct.
    for schema in schemas {
        for table in &schema.tables {
            cov.tables_total += 1;
            g.add_node(table_node(repo_name, &schema.path, table));
            for col in &table.columns {
                cov.columns_total += 1;
                g.add_node(column_node(
                    repo_name,
                    &schema.path,
                    &table.name,
                    &col.name,
                    &col.sql_type,
                ));
            }
        }
    }

    // ── Pass 2: edges (HasColumn containment + ForeignKey reference). ──
    for schema in schemas {
        for table in &schema.tables {
            let table_uid = table_uid(repo_name, &schema.path, &table.name);
            // Table —HasColumn→ each Column (membership; impact does not traverse).
            for col in &table.columns {
                let col_uid = column_uid(repo_name, &schema.path, &table.name, &col.name);
                g.add_edge(Edge {
                    src: table_uid.clone(),
                    dst: col_uid,
                    kind: EdgeKind::HasColumn,
                    provenance: Provenance::Extracted,
                    confidence: Confidence::new(CONF_DATA_FACT),
                });
            }
            // Column —ForeignKey→ referenced Column (dependency; impact traverses).
            for fk in &table.foreign_keys {
                cov.fks_total += 1;
                let src = column_uid(repo_name, &schema.path, &table.name, &fk.column);
                // The referenced column lives under the referenced table, in
                // whichever schema file declared it. We resolve it across ALL
                // schemas (a migration file may declare the target table separately).
                match resolve_ref_column(repo_name, schemas, &fk.ref_table, &fk.ref_column) {
                    Some(dst) => {
                        g.add_edge(Edge {
                            src,
                            dst,
                            kind: EdgeKind::ForeignKey,
                            provenance: Provenance::Extracted,
                            confidence: Confidence::new(CONF_DATA_FACT),
                        });
                        cov.fks_linked += 1;
                    }
                    // The FK names a table/column the parsed DDL never declares — no
                    // edge, surfaced honestly (never an edge to a phantom column).
                    None => cov.fks_unresolved += 1,
                }
            }
        }
    }

    // ── Pass 3: code→table Reads/Writes (M2). ──
    // For each code file's SQL candidates, parse the literal into (table, access)
    // refs and, when the table matches a declared `Table` node, add a `Reads`/
    // `Writes` edge from the enclosing code symbol (or the file `Module`) to the
    // table. An undeclared table → NO edge, counted unresolved (never invented, R1).
    link_code_to_tables(g, repo_name, schemas, code_sql, &mut cov);

    // ── Pass 4: ORM model→table MapsTo (M2b). ──
    // For each code file's ORM hints, resolve the explicit table name to a declared
    // `Table` node and, when the model class node exists, add a `MapsTo` edge from the
    // model to the table. An undeclared table OR an absent model node → NO edge,
    // counted unresolved (never invented, R1) — the same rule as Reads/Writes.
    link_orm_to_tables(g, repo_name, schemas, code_orm, &mut cov);

    cov
}

/// Phase 3: parse each code file's SQL candidates and add `Reads`/`Writes` edges
/// from the enclosing code node to the declared `Table` it touches.
///
/// The source code node UID is reconstructed from the candidate's language tag,
/// file path, and enclosing fqn — `Uid::new(lang, repo, path, fqn, "")`, falling
/// back to the file's `Module` UID (`<module>` fqn-slot) when the enclosing fqn is
/// empty OR no symbol node exists for it (a literal in an un-extracted scope still
/// attributes to its module rather than vanishing). The edge is added only when the
/// referenced table resolves to a declared `Table` node; otherwise the ref is
/// counted `reads_unresolved`/`writes_unresolved` and NO edge is created (R1).
fn link_code_to_tables(
    g: &mut Graph,
    repo_name: &str,
    schemas: &[SchemaModel],
    code_sql: &[CodeSqlFile],
    cov: &mut DataLinkCoverage,
) {
    for file in code_sql {
        for cand in file.candidates {
            for tref in strata_data::parse_table_refs(&cand.text) {
                // Resolve the referenced table to its declared `Table` node, across
                // ALL schemas (the table may be declared in any schema file).
                let table_uid = resolve_table(repo_name, schemas, &tref.table);
                let (kind, linked, unresolved) = match tref.access {
                    SqlAccess::Read => (
                        EdgeKind::Reads,
                        &mut cov.reads_linked,
                        &mut cov.reads_unresolved,
                    ),
                    SqlAccess::Write => (
                        EdgeKind::Writes,
                        &mut cov.writes_linked,
                        &mut cov.writes_unresolved,
                    ),
                };
                let Some(dst) = table_uid else {
                    // The literal names a table the parsed DDL never declares — no
                    // edge, counted honestly (never invented).
                    *unresolved += 1;
                    continue;
                };
                let src = code_node_uid(g, file.lang, repo_name, file.path, &cand.enclosing_fqn);
                g.add_edge(Edge {
                    src,
                    dst,
                    kind,
                    provenance: Provenance::Extracted,
                    confidence: Confidence::new(CONF_DATA_FACT),
                });
                *linked += 1;
            }
        }
    }
}

/// Phase 4: add `MapsTo` edges from each ORM model class to the declared `Table` its
/// explicit table name resolves to (Slice 25, D3, M2b).
///
/// The model class node UID is reconstructed from the hint's language tag, file path,
/// and `model_fqn` — `Uid::new(lang, repo, path, model_fqn, "")`. Unlike the
/// `Reads`/`Writes` path, there is **no `<module>` fallback**: a hint names a specific
/// class, so if that class node is absent (e.g. it was pruned, or extraction missed
/// it) we add NO edge and count it unresolved — never attach a structural mapping to
/// the file module, which would misattribute it. The edge is added only when BOTH the
/// model node exists AND the table resolves to a declared `Table`; otherwise the hint
/// is counted `orm_models_unresolved` and NO edge is created (never invented, R1).
fn link_orm_to_tables(
    g: &mut Graph,
    repo_name: &str,
    schemas: &[SchemaModel],
    code_orm: &[CodeOrmFile],
    cov: &mut DataLinkCoverage,
) {
    for file in code_orm {
        for hint in file.hints {
            cov.orm_models_total += 1;
            // Resolve the explicit table name to a declared `Table` node (REUSE the
            // shared resolver — same rule as Reads/Writes), across ALL schemas.
            let Some(dst) = resolve_table(repo_name, schemas, &hint.table_name) else {
                // The model names a table the parsed DDL never declares — no edge,
                // counted honestly (never invented).
                cov.orm_models_unresolved += 1;
                continue;
            };
            // The model class node MUST exist; a class hint never falls back to the
            // module (a structural mapping belongs to the class, not the file).
            let src = Uid::new(file.lang, repo_name, file.path, &hint.model_fqn, "");
            if g.get_node(&src).is_none() {
                cov.orm_models_unresolved += 1;
                continue;
            }
            g.add_edge(Edge {
                src,
                dst,
                kind: EdgeKind::MapsTo,
                provenance: Provenance::Extracted,
                confidence: Confidence::new(CONF_ORM_EXPLICIT),
            });
            cov.orm_models_linked += 1;
        }
    }
}

/// The code-plane node UID a `Reads`/`Writes` edge originates from: the enclosing
/// symbol (`Uid::new(lang, repo, path, fqn, "")`) when a node exists for that fqn,
/// else the file's `Module` node (`<module>` fqn-slot). The module always exists for
/// an indexed file, so this never targets a phantom source.
fn code_node_uid(g: &Graph, lang: &str, repo_name: &str, path: &str, enclosing_fqn: &str) -> Uid {
    if !enclosing_fqn.is_empty() {
        let sym = Uid::new(lang, repo_name, path, enclosing_fqn, "");
        if g.get_node(&sym).is_some() {
            return sym;
        }
    }
    // Module fqn-slot is the literal `<module>` (matches build.rs / link.rs).
    Uid::new(lang, repo_name, path, "<module>", "")
}

/// Resolve a bare table name to its declared `Table` node UID, when that table is
/// declared in ANY of the parsed schemas. Returns `None` when no parsed schema
/// declares it — the honest miss (no `Reads`/`Writes` edge is invented).
fn resolve_table(repo_name: &str, schemas: &[SchemaModel], table: &str) -> Option<Uid> {
    for schema in schemas {
        if schema.tables.iter().any(|t| t.name == table) {
            return Some(table_uid(repo_name, &schema.path, table));
        }
    }
    None
}

/// Build a graph that is ONLY the data plane (no code/contract/infra), from a set
/// of [`SchemaModel`]s. A convenience for hermetic tests (and any in-memory caller)
/// that want the data-plane graph without reading a repo from disk. Returns the
/// graph and its [`DataLinkCoverage`]. No code files, so no `Reads`/`Writes` edges.
pub fn assemble_graph_with_data(
    repo_name: &str,
    schemas: &[SchemaModel],
) -> (Graph, DataLinkCoverage) {
    let mut g = Graph::new();
    let cov = build_data_plane(&mut g, repo_name, schemas, &[], &[]);
    (g, cov)
}

/// Resolve a foreign key's referenced `ref_table`.`ref_column` to the UID of the
/// `Column` node, when that table+column are declared in ANY of the parsed schemas
/// (the FK target may live in a different migration file than the referencing
/// table). Returns `None` when no parsed schema declares it — the honest miss.
fn resolve_ref_column(
    repo_name: &str,
    schemas: &[SchemaModel],
    ref_table: &str,
    ref_column: &str,
) -> Option<Uid> {
    for schema in schemas {
        for table in &schema.tables {
            if table.name != ref_table {
                continue;
            }
            if table.columns.iter().any(|c| c.name == ref_column) {
                return Some(column_uid(repo_name, &schema.path, ref_table, ref_column));
            }
        }
    }
    None
}

/// The data-plane node for one table: `Extracted` 1.0, `name` = `fqn` = table name,
/// `path` = schema file path.
fn table_node(repo_name: &str, schema_path: &str, table: &TableDef) -> Node {
    Node {
        uid: table_uid(repo_name, schema_path, &table.name),
        kind: NodeKind::Table,
        name: table.name.clone(),
        fqn: table.name.clone(),
        path: schema_path.to_string(),
        span: Span::default(),
        provenance: Provenance::Extracted,
        confidence: Confidence::new(1.0),
    }
}

/// The data-plane node for one column: `Extracted` 1.0, `name` = column name, `fqn`
/// = `table.column`, `path` = schema file path. The SQL type is carried in the
/// node name's companion? No — kept minimal (name/fqn) like every other plane's
/// node; the type lives in the source `SchemaModel` (and is surfaced by `context`
/// via the column node's identity). The `_sql_type` is accepted so the call site
/// reads naturally and a future enrichment has a home.
fn column_node(
    repo_name: &str,
    schema_path: &str,
    table: &str,
    column: &str,
    _sql_type: &str,
) -> Node {
    Node {
        uid: column_uid(repo_name, schema_path, table, column),
        kind: NodeKind::Column,
        name: column.to_string(),
        fqn: format!("{table}.{column}"),
        path: schema_path.to_string(),
        span: Span::default(),
        provenance: Provenance::Extracted,
        confidence: Confidence::new(1.0),
    }
}

/// The UID of a table node: `data | repo | schema_path | <table> |`.
fn table_uid(repo_name: &str, schema_path: &str, table: &str) -> Uid {
    Uid::new(DATA_LANG, repo_name, schema_path, table, "")
}

/// The UID of a column node: `data | repo | schema_path | <table>.<column> |`. The
/// `table.column` fqn keeps the same column name in two tables distinct.
fn column_uid(repo_name: &str, schema_path: &str, table: &str, column: &str) -> Uid {
    Uid::new(
        DATA_LANG,
        repo_name,
        schema_path,
        &format!("{table}.{column}"),
        "",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use strata_core::Direction;
    use strata_data::{ColumnDef, ForeignKey};

    /// A `SchemaModel` with one table built from `(name, type, nullable, pk)` cols
    /// and `(col, ref_table, ref_col)` foreign keys.
    fn schema(path: &str, tables: Vec<TableDef>) -> SchemaModel {
        SchemaModel {
            path: path.to_string(),
            tables,
            ..Default::default()
        }
    }

    fn col(name: &str, ty: &str, nullable: bool, pk: bool) -> ColumnDef {
        ColumnDef {
            name: name.into(),
            sql_type: ty.into(),
            nullable,
            primary_key: pk,
        }
    }

    fn fk(column: &str, ref_table: &str, ref_column: &str) -> ForeignKey {
        ForeignKey {
            column: column.into(),
            ref_table: ref_table.into(),
            ref_column: ref_column.into(),
        }
    }

    /// The canonical two-table FK shape: `accounts.org_id → orgs.id`.
    fn two_table_schema() -> Vec<SchemaModel> {
        vec![schema(
            "schema.sql",
            vec![
                TableDef {
                    name: "orgs".into(),
                    columns: vec![
                        col("id", "BIGINT", false, true),
                        col("name", "TEXT", false, false),
                    ],
                    foreign_keys: vec![],
                },
                TableDef {
                    name: "accounts".into(),
                    columns: vec![
                        col("id", "BIGINT", false, true),
                        col("org_id", "BIGINT", false, false),
                    ],
                    foreign_keys: vec![fk("org_id", "orgs", "id")],
                },
            ],
        )]
    }

    #[test]
    fn builds_table_and_column_nodes() {
        let (g, cov) = assemble_graph_with_data("app", &two_table_schema());
        assert_eq!(cov.tables_total, 2);
        assert_eq!(cov.columns_total, 4);

        // Both tables are Table nodes; the columns are Column nodes with table.col fqns.
        let kinds_of = |fqn: &str| {
            g.nodes()
                .find(|n| n.fqn == fqn)
                .map(|n| n.kind)
                .unwrap_or_else(|| panic!("node {fqn} not found"))
        };
        assert_eq!(kinds_of("orgs"), NodeKind::Table);
        assert_eq!(kinds_of("accounts"), NodeKind::Table);
        assert_eq!(kinds_of("orgs.id"), NodeKind::Column);
        assert_eq!(kinds_of("accounts.org_id"), NodeKind::Column);
    }

    #[test]
    fn table_has_column_edges_are_extracted() {
        let (g, _cov) = assemble_graph_with_data("app", &two_table_schema());
        let orgs = table_uid("app", "schema.sql", "orgs");
        let cols: Vec<&str> = g
            .neighbors(&orgs, Direction::Outgoing, &[EdgeKind::HasColumn])
            .into_iter()
            .map(|(_, n)| n.name.as_str())
            .collect();
        // orgs HasColumn id, name (the membership edge).
        assert!(cols.contains(&"id"));
        assert!(cols.contains(&"name"));
        // The edge is an Extracted fact.
        for (e, _) in g.neighbors(&orgs, Direction::Outgoing, &[EdgeKind::HasColumn]) {
            assert_eq!(e.provenance, Provenance::Extracted);
            assert!(e.confidence.value() >= 0.95);
        }
    }

    #[test]
    fn foreign_key_edge_links_column_to_referenced_column() {
        let (g, cov) = assemble_graph_with_data("app", &two_table_schema());
        assert_eq!(cov.fks_total, 1);
        assert_eq!(cov.fks_linked, 1);
        assert_eq!(cov.fks_unresolved, 0);

        // accounts.org_id —ForeignKey→ orgs.id.
        let src = column_uid("app", "schema.sql", "accounts", "org_id");
        let targets: Vec<&str> = g
            .neighbors(&src, Direction::Outgoing, &[EdgeKind::ForeignKey])
            .into_iter()
            .map(|(_, n)| n.fqn.as_str())
            .collect();
        assert_eq!(
            targets,
            vec!["orgs.id"],
            "FK edge points at the referenced column"
        );
    }

    #[test]
    fn foreign_key_to_undeclared_table_is_unresolved_no_edge() {
        // An FK to a table the parsed DDL never declares → no edge, counted.
        let schemas = vec![schema(
            "schema.sql",
            vec![TableDef {
                name: "accounts".into(),
                columns: vec![
                    col("id", "BIGINT", false, true),
                    col("ext_id", "BIGINT", true, false),
                ],
                foreign_keys: vec![fk("ext_id", "external_table", "id")],
            }],
        )];
        let (g, cov) = assemble_graph_with_data("app", &schemas);
        assert_eq!(cov.fks_total, 1);
        assert_eq!(cov.fks_linked, 0);
        assert_eq!(
            cov.fks_unresolved, 1,
            "an FK to an undeclared table is unresolved"
        );
        let src = column_uid("app", "schema.sql", "accounts", "ext_id");
        assert!(
            g.neighbors(&src, Direction::Outgoing, &[EdgeKind::ForeignKey])
                .is_empty(),
            "no edge is invented for an unresolved FK"
        );
    }

    #[test]
    fn cross_file_foreign_key_resolves_across_schemas() {
        // The referenced table is declared in a DIFFERENT schema file.
        let schemas = vec![
            schema(
                "001_orgs.sql",
                vec![TableDef {
                    name: "orgs".into(),
                    columns: vec![col("id", "BIGINT", false, true)],
                    foreign_keys: vec![],
                }],
            ),
            schema(
                "002_accounts.sql",
                vec![TableDef {
                    name: "accounts".into(),
                    columns: vec![col("org_id", "BIGINT", false, false)],
                    foreign_keys: vec![fk("org_id", "orgs", "id")],
                }],
            ),
        ];
        let (g, cov) = assemble_graph_with_data("app", &schemas);
        assert_eq!(
            cov.fks_linked, 1,
            "the FK resolves to the orgs table in the other file"
        );
        let src = column_uid("app", "002_accounts.sql", "accounts", "org_id");
        let dst = column_uid("app", "001_orgs.sql", "orgs", "id");
        let targets: Vec<Uid> = g
            .neighbors(&src, Direction::Outgoing, &[EdgeKind::ForeignKey])
            .into_iter()
            .map(|(_, n)| n.uid.clone())
            .collect();
        assert_eq!(targets, vec![dst]);
    }

    #[test]
    fn nodes_are_idempotent_by_uid() {
        // Nodes are keyed by UID: building twice over the same schema adds no new
        // nodes (a re-add replaces in place). Edges accumulate — `Graph::add_edge`
        // appends — but the production path (`index_impl`) always builds into a
        // FRESH graph, so this is the relevant guarantee, matching the infra
        // builder. The coverage struct is a pure function of the input regardless.
        let schemas = two_table_schema();
        let mut g = Graph::new();
        let cov1 = build_data_plane(&mut g, "app", &schemas, &[], &[]);
        let n1 = g.node_count();
        let cov2 = build_data_plane(&mut g, "app", &schemas, &[], &[]);
        assert_eq!(g.node_count(), n1, "nodes are idempotent by UID");
        assert_eq!(cov1, cov2, "coverage is a pure function of the input");
    }

    // ── Pass 3: code→table Reads/Writes (Slice 16, D3, M2). ──

    /// A `SqlCandidate` with the given inner SQL text and enclosing fqn.
    fn cand(text: &str, enclosing: &str) -> SqlCandidate {
        SqlCandidate {
            text: text.into(),
            enclosing_fqn: enclosing.into(),
            span: Span::default(),
        }
    }

    /// Seed a code `Module` node and (optionally) a `Function` node so the data
    /// plane's `Reads`/`Writes` edges have a real source to target, mimicking the
    /// code plane that `index_impl` builds before the data plane runs.
    fn seed_code_node(g: &mut Graph, lang: &str, repo: &str, path: &str, fqn: &str) -> Uid {
        let uid = Uid::new(lang, repo, path, fqn, "");
        g.add_node(Node {
            uid: uid.clone(),
            kind: if fqn == "<module>" {
                NodeKind::Module
            } else {
                NodeKind::Function
            },
            name: fqn.to_string(),
            fqn: fqn.to_string(),
            path: path.to_string(),
            span: Span::default(),
            provenance: Provenance::Extracted,
            confidence: Confidence::new(1.0),
        });
        uid
    }

    #[test]
    fn code_select_links_reads_edge_to_declared_table_extracted() {
        // A `SELECT … FROM orgs` in fn `loadOrg` → orgs Reads edge, Extracted 0.95.
        let schemas = two_table_schema();
        let mut g = Graph::new();
        let fn_uid = seed_code_node(&mut g, "ts", "app", "src/a.ts", "loadOrg");
        let candidates = [cand("SELECT name FROM orgs WHERE id = 1", "loadOrg")];
        let code = [CodeSqlFile {
            lang: "ts",
            path: "src/a.ts",
            candidates: &candidates,
        }];
        let cov = build_data_plane(&mut g, "app", &schemas, &code, &[]);

        assert_eq!(cov.reads_linked, 1);
        assert_eq!(cov.reads_unresolved, 0);
        assert_eq!(cov.writes_linked, 0);

        // loadOrg —Reads→ orgs, an Extracted 0.95 fact.
        let targets: Vec<(&str, Provenance, f32)> = g
            .neighbors(&fn_uid, Direction::Outgoing, &[EdgeKind::Reads])
            .into_iter()
            .map(|(e, n)| (n.name.as_str(), e.provenance, e.confidence.value()))
            .collect();
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].0, "orgs");
        assert_eq!(targets[0].1, Provenance::Extracted);
        assert!(targets[0].2 >= 0.95);
    }

    #[test]
    fn code_insert_and_update_link_writes_edges() {
        let schemas = two_table_schema();
        let mut g = Graph::new();
        let ins = seed_code_node(&mut g, "py", "app", "app.py", "create_account");
        let candidates = [cand(
            "INSERT INTO accounts (id) VALUES (1)",
            "create_account",
        )];
        let code = [CodeSqlFile {
            lang: "py",
            path: "app.py",
            candidates: &candidates,
        }];
        let cov = build_data_plane(&mut g, "app", &schemas, &code, &[]);
        assert_eq!(cov.writes_linked, 1);
        assert_eq!(cov.reads_linked, 0);
        let writes: Vec<&str> = g
            .neighbors(&ins, Direction::Outgoing, &[EdgeKind::Writes])
            .into_iter()
            .map(|(_, n)| n.name.as_str())
            .collect();
        assert_eq!(writes, vec!["accounts"], "INSERT INTO accounts is a Write");
    }

    #[test]
    fn code_sql_to_undeclared_table_is_unresolved_no_edge() {
        // A query against a table no schema declares → NO edge, counted unresolved.
        let schemas = two_table_schema();
        let mut g = Graph::new();
        let fn_uid = seed_code_node(&mut g, "ts", "app", "src/a.ts", "f");
        let candidates = [cand("SELECT * FROM legacy_widgets", "f")];
        let code = [CodeSqlFile {
            lang: "ts",
            path: "src/a.ts",
            candidates: &candidates,
        }];
        let cov = build_data_plane(&mut g, "app", &schemas, &code, &[]);
        assert_eq!(cov.reads_linked, 0);
        assert_eq!(
            cov.reads_unresolved, 1,
            "a read of an undeclared table is unresolved"
        );
        assert!(
            g.neighbors(&fn_uid, Direction::Outgoing, &[EdgeKind::Reads])
                .is_empty(),
            "no Reads edge is invented for an undeclared table"
        );
    }

    #[test]
    fn code_sql_falls_back_to_module_when_no_enclosing_symbol() {
        // A module-top-level SQL literal (empty enclosing fqn) attributes to the
        // file's `Module` node, not a phantom symbol.
        let schemas = two_table_schema();
        let mut g = Graph::new();
        let module = seed_code_node(&mut g, "ts", "app", "src/a.ts", "<module>");
        let candidates = [cand("SELECT id FROM orgs", "")];
        let code = [CodeSqlFile {
            lang: "ts",
            path: "src/a.ts",
            candidates: &candidates,
        }];
        let cov = build_data_plane(&mut g, "app", &schemas, &code, &[]);
        assert_eq!(cov.reads_linked, 1);
        let reads: Vec<&str> = g
            .neighbors(&module, Direction::Outgoing, &[EdgeKind::Reads])
            .into_iter()
            .map(|(_, n)| n.name.as_str())
            .collect();
        assert_eq!(
            reads,
            vec!["orgs"],
            "a top-level read attributes to the module"
        );
    }

    #[test]
    fn impact_table_reaches_reading_and_writing_code() {
        // THE §6.2 ENGINE PROOF (table-level): a reader and a writer of `orgs`, plus
        // impact(orgs) must reach BOTH via the (incoming) Reads/Writes edges.
        let schemas = two_table_schema();
        let mut g = Graph::new();
        let reader = seed_code_node(&mut g, "ts", "app", "src/read.ts", "listOrgs");
        let writer = seed_code_node(&mut g, "py", "app", "write.py", "rename_org");
        let read_cands = [cand("SELECT name FROM orgs", "listOrgs")];
        let write_cands = [cand(
            "UPDATE orgs SET name = 'x' WHERE id = 1",
            "rename_org",
        )];
        let code = [
            CodeSqlFile {
                lang: "ts",
                path: "src/read.ts",
                candidates: &read_cands,
            },
            CodeSqlFile {
                lang: "py",
                path: "write.py",
                candidates: &write_cands,
            },
        ];
        build_data_plane(&mut g, "app", &schemas, &code, &[]);

        let orgs = table_uid("app", "schema.sql", "orgs");
        let r = strata_core::impact(&g, &orgs, &strata_core::ImpactOptions::default());
        let reached: Vec<&str> = r.affected.iter().map(|a| a.uid.as_str()).collect();
        assert!(
            reached.contains(&reader.as_str()),
            "impact(orgs) reaches the reading code (listOrgs); got {reached:?}"
        );
        assert!(
            reached.contains(&writer.as_str()),
            "impact(orgs) reaches the writing code (rename_org); got {reached:?}"
        );
        // Both reach via an Extracted fact → labelled will-break, not "may affect".
        let reader_node = r.affected.iter().find(|a| a.uid == reader).unwrap();
        assert!(
            reader_node.will_break && !reader_node.ambiguous,
            "an Extracted Reads edge yields a will-break, non-ambiguous verdict"
        );
    }

    // ── Pass 4: ORM model→table MapsTo (Slice 25, D3, M2b). ──

    use strata_core::OrmFramework;

    /// An `OrmModelHint` for the given model fqn + table name (framework irrelevant
    /// to linking; use SqlAlchemy as the default).
    fn orm_hint(model_fqn: &str, table_name: &str) -> OrmModelHint {
        OrmModelHint {
            model_fqn: model_fqn.into(),
            table_name: table_name.into(),
            framework: OrmFramework::SqlAlchemy,
            span: Span::default(),
        }
    }

    /// Seed a code `Class` node (the model class a MapsTo edge originates from).
    fn seed_class_node(g: &mut Graph, lang: &str, repo: &str, path: &str, fqn: &str) -> Uid {
        let uid = Uid::new(lang, repo, path, fqn, "");
        g.add_node(Node {
            uid: uid.clone(),
            kind: NodeKind::Class,
            name: fqn.to_string(),
            fqn: fqn.to_string(),
            path: path.to_string(),
            span: Span::default(),
            provenance: Provenance::Extracted,
            confidence: Confidence::new(1.0),
        });
        uid
    }

    #[test]
    fn orm_model_links_mapsto_edge_to_declared_table_extracted() {
        // A model `User` mapping to declared `orgs` → User —MapsTo→ orgs, Extracted 0.95.
        let schemas = two_table_schema();
        let mut g = Graph::new();
        let user = seed_class_node(&mut g, "py", "app", "models.py", "User");
        let hints = [orm_hint("User", "orgs")];
        let orm = [CodeOrmFile {
            lang: "py",
            path: "models.py",
            hints: &hints,
        }];
        let cov = build_data_plane(&mut g, "app", &schemas, &[], &orm);

        assert_eq!(cov.orm_models_total, 1);
        assert_eq!(cov.orm_models_linked, 1);
        assert_eq!(cov.orm_models_unresolved, 0);

        let targets: Vec<(&str, Provenance, f32)> = g
            .neighbors(&user, Direction::Outgoing, &[EdgeKind::MapsTo])
            .into_iter()
            .map(|(e, n)| (n.name.as_str(), e.provenance, e.confidence.value()))
            .collect();
        assert_eq!(targets.len(), 1, "one MapsTo edge");
        assert_eq!(targets[0].0, "orgs");
        assert_eq!(targets[0].1, Provenance::Extracted);
        assert!(targets[0].2 >= 0.95, "Extracted band floor");
    }

    #[test]
    fn orm_model_to_undeclared_table_is_unresolved_no_edge() {
        // A model naming a table no schema declares → NO edge, counted unresolved.
        let schemas = two_table_schema();
        let mut g = Graph::new();
        let user = seed_class_node(&mut g, "py", "app", "models.py", "Widget");
        let hints = [orm_hint("Widget", "legacy_widgets")];
        let orm = [CodeOrmFile {
            lang: "py",
            path: "models.py",
            hints: &hints,
        }];
        let cov = build_data_plane(&mut g, "app", &schemas, &[], &orm);
        assert_eq!(cov.orm_models_total, 1);
        assert_eq!(cov.orm_models_linked, 0);
        assert_eq!(
            cov.orm_models_unresolved, 1,
            "an ORM model naming an undeclared table is unresolved"
        );
        assert!(
            g.neighbors(&user, Direction::Outgoing, &[EdgeKind::MapsTo])
                .is_empty(),
            "no MapsTo edge is invented for an undeclared table"
        );
    }

    #[test]
    fn orm_model_with_absent_class_node_is_unresolved_no_module_fallback() {
        // The table is declared but the model class node is ABSENT (not seeded). A
        // class hint never falls back to the module → NO edge, counted unresolved.
        let schemas = two_table_schema();
        let mut g = Graph::new();
        // Seed only the module node, NOT the class — to prove no module fallback.
        let module = seed_code_node(&mut g, "py", "app", "models.py", "<module>");
        let hints = [orm_hint("User", "orgs")];
        let orm = [CodeOrmFile {
            lang: "py",
            path: "models.py",
            hints: &hints,
        }];
        let cov = build_data_plane(&mut g, "app", &schemas, &[], &orm);
        assert_eq!(cov.orm_models_linked, 0);
        assert_eq!(
            cov.orm_models_unresolved, 1,
            "an absent model class node is unresolved (no module fallback)"
        );
        assert!(
            g.neighbors(&module, Direction::Outgoing, &[EdgeKind::MapsTo])
                .is_empty(),
            "a class hint never attaches a MapsTo edge to the module"
        );
    }

    #[test]
    fn orm_mapsto_is_idempotent_by_uid() {
        // Re-running over the same hints adds no new nodes (idempotent by uid) and the
        // coverage is a pure function of the input.
        let schemas = two_table_schema();
        let mut g = Graph::new();
        seed_class_node(&mut g, "py", "app", "models.py", "User");
        let hints = [orm_hint("User", "orgs")];
        let orm = [CodeOrmFile {
            lang: "py",
            path: "models.py",
            hints: &hints,
        }];
        let cov1 = build_data_plane(&mut g, "app", &schemas, &[], &orm);
        let n1 = g.node_count();
        let cov2 = build_data_plane(&mut g, "app", &schemas, &[], &orm);
        assert_eq!(g.node_count(), n1, "nodes idempotent by uid");
        assert_eq!(cov1, cov2, "coverage is a pure function of the input");
    }

    #[test]
    fn impact_table_reaches_mapping_model_and_its_caller() {
        // THE §6.2 ENGINE PROOF (ORM): impact(orgs) reaches the User model (MapsTo)
        // AND a function that instantiates User (transitive MapsTo + Calls).
        let schemas = two_table_schema();
        let mut g = Graph::new();
        let user = seed_class_node(&mut g, "py", "app", "models.py", "User");
        let caller = seed_code_node(&mut g, "py", "app", "svc.py", "load_user");
        // load_user —Calls→ User (instantiation), an Inferred-grade code edge.
        g.add_edge(Edge {
            src: caller.clone(),
            dst: user.clone(),
            kind: EdgeKind::Calls,
            provenance: Provenance::Inferred,
            confidence: Confidence::new(0.8),
        });
        let hints = [orm_hint("User", "orgs")];
        let orm = [CodeOrmFile {
            lang: "py",
            path: "models.py",
            hints: &hints,
        }];
        build_data_plane(&mut g, "app", &schemas, &[], &orm);

        let orgs = table_uid("app", "schema.sql", "orgs");
        let r = strata_core::impact(&g, &orgs, &strata_core::ImpactOptions::default());
        let reached: Vec<&str> = r.affected.iter().map(|a| a.uid.as_str()).collect();
        assert!(
            reached.contains(&user.as_str()),
            "impact(orgs) reaches the mapping model User via MapsTo; got {reached:?}"
        );
        assert!(
            reached.contains(&caller.as_str()),
            "impact(orgs) reaches load_user transitively (MapsTo + Calls); got {reached:?}"
        );
        // The model reaches via an Extracted MapsTo edge → will-break, non-ambiguous.
        let user_node = r.affected.iter().find(|a| a.uid == user).unwrap();
        assert!(
            user_node.will_break && !user_node.ambiguous,
            "an Extracted MapsTo edge yields a will-break, non-ambiguous verdict: {user_node:?}"
        );
    }

    #[test]
    fn context_table_lists_mapping_model_in_mapped_by() {
        // context(orgs).mapped_by lists the User model; context(User).maps_to is orgs.
        let schemas = two_table_schema();
        let mut g = Graph::new();
        let user = seed_class_node(&mut g, "py", "app", "models.py", "User");
        let hints = [orm_hint("User", "orgs")];
        let orm = [CodeOrmFile {
            lang: "py",
            path: "models.py",
            hints: &hints,
        }];
        build_data_plane(&mut g, "app", &schemas, &[], &orm);

        let orgs = table_uid("app", "schema.sql", "orgs");
        let ctx = strata_core::context(&g, &orgs).expect("orgs table in graph");
        let mapped: Vec<&str> = ctx.mapped_by.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(mapped, vec!["User"], "orgs.mapped_by lists the User model");

        let user_ctx = strata_core::context(&g, &user).expect("User class in graph");
        let maps: Vec<&str> = user_ctx.maps_to.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(maps, vec!["orgs"], "User.maps_to is the orgs table");
    }
}
