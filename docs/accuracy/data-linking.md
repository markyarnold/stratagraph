# Data Plane Link Coverage

Measured coverage of Strata's **data-plane linking**: how much of a detected SQL
schema the Slice-16 (Track D3) data plane turns into graph structure: `Table` and
`Column` nodes, the `HasColumn` membership edges, the `ForeignKey` reference edges
between columns, the (M2) `Reads`/`Writes` edges from code to the tables it touches,
AND (M2b) the `MapsTo` edges from ORM model classes to the tables they map to, and
at what honest-provenance tier. This is the data-plane companion to
`docs/accuracy/infra-linking.md`, `docs/accuracy/openapi-linking.md`, and
`docs/accuracy/graphql-linking.md`.

The numbers are produced by indexing a committed, hermetic fixture estate (sources
only; no `node_modules`, no Node at test time) and aggregating each repo's per-repo
`DataLinkCoverage` (surfaced in `IndexStats`, and printed by `strata index`). They
are kept honest two ways, the same discipline as the other plane reports:

- **`tests/data_coverage.rs::data_report_matches_committed_numbers`** asserts the
  live aggregated `DataLinkCoverage` equals the numbers tabulated below (so this
  report cannot silently drift from the code).
- **`tests/data_coverage.rs::data_coverage_meets_documented_floors`** is the CI
  gate: it fails the build if any documented floor regresses.

Regenerate the raw figures with:

```
cargo test -p strata-index --test data_coverage print_data_coverage -- --ignored --nocapture
```

## Honesty / scope caveat

**The corpus is a single 2-repo fixture estate.** These numbers are a *starting*
coverage measurement on a deliberately small, hand-built estate that exercises
every data-linking path once, not a statistically authoritative claim about
real-world recall. The durable deliverables are the `strata-data` SQL-DDL adapter,
the `data.rs` builder, the band invariant over the two data edge kinds, the CI
gate, and this report, all of which sharpen automatically as the corpus grows.

**Scope: explicit DDL (M1) + raw-SQL code→table links (M2).** The data plane
models the **declared end-state** of committed SQL DDL: it applies `CREATE TABLE`
and the cumulative `ALTER TABLE ADD/DROP/RENAME COLUMN` in file order, and reads
foreign keys from explicit inline `REFERENCES` and table-level `FOREIGN KEY`
constraints. It infers **nothing** from DDL: ORM convention-derived columns/tables
(Rails/Django implicit names, SQLAlchemy/Prisma models) are deferred (see *ORM,
deferred to M2b* below) and contribute no nodes. Statements the plane does not
model (`CREATE INDEX`/`EXTENSION`/`FUNCTION`/`VIEW`, `INSERT`s) are **skipped, not
errored**; a migration that interleaves them still yields its tables. A genuinely
unparseable `.sql` is **skipped but surfaced** as a `[data] FAILED …` diagnostic and
counted in `schemas_failed`, never silently dropped. We model the declared schema,
**not the migration history** (no replay-then-diff): the final shape is what the
graph sees.

**Per-statement parsing (robustness).** A `.sql` file is split into its top-level
statements **before** parsing, on `;`, by a small dollar-quote/string/comment-aware
lexer so a `;` inside any of these is **not** a separator: a `'…'` string (with the
`''` escape, and backslash escapes inside a Postgres `E'…'`/`e'…'` E-string so `\'`
and `\\` do not close it); a `"…"` double-quoted identifier (its own state: a `'`,
`;`, `--`, `/*`, or `$` inside it is inert, the only escape the doubled `""`, so
`"it's"` does not flip into string state and `"a;b"` is not split mid-identifier); a
`$$…$$`/`$tag$…$tag$` dollar-quoted body (matched on the exact opening tag); a `-- …`
line comment; or a (Postgres-nesting) `/* … */` block comment. Each statement is
then parsed individually — first with the PostgreSQL dialect, then a **ClickHouse
recovery ladder**: the ClickHouse dialect as a whole-statement fallback; a
column-list recovery for a `CREATE TABLE` whose ClickHouse tail (`ENGINE`,
`PARTITION BY`, `TTL`, `SETTINGS`, `ON CLUSTER`) defeats both dialects; and a
normaliser that strips ClickHouse-only column decoration (`CODEC(…)`, column
`TTL`/`ALIAS`/`MATERIALIZED`/`EPHEMERAL`, inline `INDEX`/`PROJECTION` entries,
aggregate-state type parameters) before re-parsing. Every recovered statement is
re-validated by `sqlparser` as a real `CREATE TABLE`, so the declared column set
is exact and nothing is guessed; a segment led by non-SQL prose retries from its
embedded `CREATE` (string-aware, so quoted DDL text can never become a phantom
table). Statements recognized as declaring no static shape (RBAC, index/TTL
maintenance, teardown, clone/CTAS tables whose shape lives in another table or a
query, bare `SETTINGS`) are skipped with counts rather than failing their file.
A statement `sqlparser` still cannot parse (a PL/pgSQL
`DO $$ … $$` block, a `CREATE FUNCTION` with a dollar-quoted body, a dialect-specific
statement) is **skipped** (counted in `statements_skipped`), and the surrounding
`CREATE TABLE`/`ALTER TABLE` statements still build the schema. This mirrors the CFN
adapter's "skip non-resource statements, only diagnose a wholly-bad file" precedent:
one unparseable statement no longer drops a whole migration file's tables (dogfound
in real-world testing, where a `DO`/dollar-quote in some migrations had been dropping their
tables under whole-file parsing). A file is counted in `schemas_failed` **only** when
it carries the DDL signal yet yields **zero** parseable statements; a
some-good-some-bad file is a SUCCESS that yields its tables, the skip surfaced
informationally, never as a failure, and never inventing a table from an
unparseable statement.

*Honest bound (narrow).* The lexer assumes the Postgres default
`standard_conforming_strings = on`: a backslash is literal in a **normal** `'…'`
string and only escapes inside an `E'…'` string. A migration that flips the
deprecated `SET standard_conforming_strings = off` would make backslashes escape in
normal strings too (not tracked). This cannot silently mislead: such a mis-split
yields segments that fail to parse and are skip-counted, never an invented table.

**Code→table links (M2).** Each language analyzer (ts/py/cs) captures the
**SQL-looking string literals** in code (a cheap `SELECT…FROM`/`INSERT INTO`/
`UPDATE…SET`/`DELETE…FROM` keyword prefilter, never every string), tagged with the
enclosing function/method. The data plane parses each with `sqlparser` and adds a
`Reads` edge (from `SELECT`/`JOIN`) or a `Writes` edge (from `INSERT`/`UPDATE`/
`DELETE`) from the enclosing code symbol (or the file `Module` when none) to the
**declared** `Table` it touches, at **Extracted 0.95**, because a literal table
name matching a `CREATE TABLE` is a fact. A literal naming a table **no** parsed
schema declares yields **no edge** (counted `reads_unresolved`/`writes_unresolved`,
never invented, the never-invent rule). **Dynamic SQL is honestly unlinked:** a
runtime-concatenated or interpolated query (a `${…}` template, a Python f-string, a
C# `$"…"`) is not a single literal, so only the constant fragment (if any) is seen,
which does not parse to a complete statement and is therefore never linked.
Granularity is **table-level** (a documented bound): `SELECT email FROM users` is a
Read of `users`, not of `users.email`. A *column* change still reaches its dependent
code **transitively**: `impact(users.email)` reverse-walks
`users.email ←HasColumn— users ←Reads/Writes— code`, one hop beyond the table.
Column-level edges are deferred.

**ORM model→table links (M2b), explicit names only.** An ORM **model class** that
declares an **explicit literal** table name is linked to its `Table` with a `MapsTo`
edge (model → table; the model depends on the table), at **Extracted 0.95**: the
literal *is* the table name, so matching it to a declared `CREATE TABLE` is a fact,
the same tier as a raw-SQL link. Captured this slice:

- **Python SQLAlchemy**: a class-body `__tablename__ = "users"`.
- **Python Django**: a nested `class Meta:` whose body has `db_table = "users"`
  (the hint is the OUTER model class, not `Meta`).
- **TS TypeORM**: a class decorator `@Entity("users")` with a first string-literal
  arg (handled whether the decorator sits on the class or is hoisted to its `export`
  statement by the grammar).

A model naming a table **no** parsed schema declares yields **no edge** (counted
`orm_models_unresolved`, never invented; the table may live in another DB or the
model is stale); a model whose class node is absent yields **no edge** either (a
class hint never falls back to the file module). `impact(table)` reverse-walks the
`MapsTo` edge, so it now reaches the mapping model **and** (transitively, via the
model's incoming `Calls`) the code that instantiates/uses the model; `context(table)`
gains a `mapped_by` bucket listing the models, and a model's `maps_to` is its table.

**Deferred (honest bound, NOT built):** **convention-derived** names (guessing the
table from the class name when there is **no** explicit literal: a bare `@Entity()`,
a `__tablename__`-less SQLAlchemy class) are a future **Inferred**-tier phase, not
this slice; a no-explicit-name model yields no hint. **TS Drizzle**
(`export const x = pgTable("x", …)`) is deferred: the TS analyzer emits no symbol
node for a non-function top-level `const`, so there is no model node to originate a
`MapsTo` from. Adding variable-symbol extraction would balloon scope, so a Drizzle
table yields no hint today (honest deferral, not a silent half-link). **C# EF**
`[Table("x")]`, **Prisma** `.prisma` files (a new parser), and **column-level** ORM
field→column mapping (this slice is table-level, like the raw-SQL M2 path) are also
deferred. A repo whose models rely entirely on convention (no explicit names) reports
zero ORM links today; that is honest, not a silent miss.

## What is counted

Per repo, over the `.sql` files `SqlSchemaAdapter` detects (a file that parses with
the PostgreSQL dialect into ≥1 `CREATE TABLE`), the data plane records:

- `schemas_detected`: `.sql` schema files detected and extracted.
- `schemas_failed`: `.sql` files carrying the SQL DDL textual signal that could
  **not** be parsed **at all** (a malformed/truncated schema, or a DDL-shaped file
  whose every statement was unparseable). Skipped so the rest of the repo indexes,
  but surfaced as a `[data] FAILED …` diagnostic and counted here.
- `statements_skipped`: individual statements the splitter isolated but `sqlparser`
  could not parse **inside files that otherwise parsed** (a `DO $$…$$`/`CREATE
  FUNCTION` body, a dialect-specific statement). The surrounding tables were still
  extracted; this is an *informational* signal (the per-statement robustness fix),
  **not** a failure (a wholly-unparseable file is counted in `schemas_failed`, not
  here). Summed across the repo's detected schemas.
- `tables_total`: every table across those schemas (each becomes a `Table` node).
- `columns_total`: every column across all tables (each becomes a `Column` node),
  reflecting the cumulative end-state (an `ALTER … ADD COLUMN` adds one).
- `fks_total`: every foreign key declared (the FK-edge candidates).
- `fks_linked`: foreign keys whose referenced `table.column` is a `Column` node in
  the graph (the target table+column are declared in some parsed schema) → a
  `ForeignKey` edge was added.
- `fks_unresolved`: foreign keys whose referenced table/column the parsed DDL
  **never declares** (it lives in another database, or is a typo) → **no** edge.
  Surfaced by its absence, never invented (R1).
- `reads_linked`: code→table **read** references (a `SELECT … FROM t` / `JOIN t`
  literal) whose table matches a declared `Table` → a `Reads` edge was added.
- `reads_unresolved`: read references whose table the parsed DDL never declares →
  **no** edge. Counted, never invented (R1).
- `writes_linked`: code→table **write** references (an `INSERT INTO t` / `UPDATE t`
  / `DELETE FROM t` literal) whose table matches a declared `Table` → a `Writes`
  edge was added.
- `writes_unresolved`: write references whose table the parsed DDL never declares →
  **no** edge. Counted, never invented (R1).
- `orm_models_total`: ORM model classes with an explicit literal table name (the
  `MapsTo`-edge candidates) seen across this repo's code (M2b).
- `orm_models_linked`: ORM models whose explicit table name matches a declared
  `Table` AND whose model class node exists → a `MapsTo` edge was added (Extracted
  0.95).
- `orm_models_unresolved`: ORM models whose explicit table name the parsed DDL never
  declares (or whose model class node is absent) → **no** edge. Counted, never
  invented (R1).

## Node & edge tiers (honest provenance, R1)

| element | trigger | tier |
|---|---|---:|
| `Table` node | a `CREATE TABLE` (cumulative end-state) | Extracted **1.0** |
| `Column` node | a declared column (CREATE / `ALTER … ADD`) | Extracted **1.0** |
| `HasColumn` edge | a `Table` → each of its `Column`s | Extracted **0.95** |
| `ForeignKey` edge | an explicit `REFERENCES` / `FOREIGN KEY` whose target column is declared | Extracted **0.95** |
| `ForeignKey` edge | the referenced table/column is not declared in any parsed schema | n/a (no edge; counted) |
| `Reads` edge | a `SELECT … FROM t` / `JOIN t` literal whose table is declared | Extracted **0.95** |
| `Writes` edge | an `INSERT INTO t` / `UPDATE t` / `DELETE FROM t` literal whose table is declared | Extracted **0.95** |
| `Reads`/`Writes` edge | the literal names a table no parsed schema declares | n/a (no edge; counted) |
| `Reads`/`Writes` edge | the SQL is dynamic (concatenated/interpolated, not a single literal) | n/a (not captured; honestly absent) |
| `MapsTo` edge | an ORM model class with an **explicit** literal table name (`__tablename__`/`db_table`/`@Entity("x")`) whose table is declared | Extracted **0.95** |
| `MapsTo` edge | the model's explicit table name is not declared in any parsed schema, OR the model class node is absent | n/a (no edge; counted) |
| `MapsTo` edge | the model has no explicit name (relies on convention) | n/a (not captured; deferred Inferred tier) |

Every data node and edge, including the M2 code→table `Reads`/`Writes` and the M2b
ORM `MapsTo`, is an **Extracted fact** (an explicit literal table name matching a
declared table is as much a fact as a raw-SQL string). Nothing is `Inferred` yet;
that tier arrives only with ORM **convention-name** inference (a future phase). `0.95`
sits at the EXTRACTED band floor; the §4.1 band invariant (Extracted ≥ 0.95) extends
to all five data edge kinds (`HasColumn`/`ForeignKey`/`Reads`/`Writes`/`MapsTo`),
guarded **non-vacuously** by
`tests/confidence_bands.rs::data_edges_satisfy_band_invariant` (membership/FK),
`::data_code_to_table_reads_writes_satisfy_band_invariant_non_vacuously`
(Reads/Writes), and `::data_orm_mapsto_satisfies_band_invariant_non_vacuously`
(MapsTo), plus `tests/data_linking.rs::data_edges_satisfy_the_extracted_band_non_vacuously`.

**`HasColumn` is traversed by `impact`** (unlike the infra `Contains` membership
edge): `impact` reverse-walks INCOMING dependency edges, so `impact(column)`
reaches its owning `Table` ("you changed a column; the table is affected"). It does
NOT run the other way: `impact(table)` does not re-list its columns (a `Table` has
no incoming `HasColumn`); that view is `context(table).members`. **`ForeignKey`,
`Reads`, and `Writes` are dependency edges**: `impact(column)` reaches the columns
that reference it, and `impact(table)` reaches the **code** that reads or writes it,
the §6.2 flagship realized ("change an RDS column/table → find what reads or writes
it"). Because the link is table-level, a column change reaches the dependent code
transitively (`impact(users.email)` → `users` via incoming `HasColumn` → the
readers/writers via incoming `Reads`/`Writes`), one hop beyond the table.

**`MapsTo` is a dependency edge** (M2b): `impact(table)` reverse-walks it, so it
reaches the ORM **model class** that maps to the table and, because the model node
also carries incoming `Calls`, transitively the code that instantiates/uses the model.
`context(table).mapped_by` lists the mapping models; a model's `context(model).maps_to`
is its table. The direction is model → table (the model depends on the table; a
column/table change breaks the model), kept distinct from `Reads`/`Writes` so a
table's read/write buckets stay query-only and the mapping has its own `mapped_by`
view.

## Corpus

One committed fixture estate under
`crates/strata-index/tests/fixtures/crossrepo_data/`:

- **`repo-a`**: a distilled Postgres `schema.sql`: `orgs`, `users`, and
  `memberships` tables with a small foreign-key graph (`users.org_id → orgs.id`;
  `memberships.user_id → users.id`; `memberships.org_id → orgs.id`), a table-level
  composite `PRIMARY KEY` and `FOREIGN KEY` constraints, a `CREATE INDEX` (skipped),
  and a cumulative `ALTER TABLE users ADD COLUMN last_login` (so `users` ends with
  four columns). It also carries the M2 code→table fixtures: `src/users.ts` with a
  `SELECT email FROM users` (1 read) and a `users JOIN orgs` (2 reads), and
  `writer.py` (asyncpg-style `$1` placeholders) with an `UPDATE users` and an
  `INSERT INTO memberships` (2 writes) plus a dynamic `DELETE FROM {table}` f-string
  that is **not** captured (the honest dynamic-SQL miss). It also carries the M2b ORM
  fixtures: `models.py` with a SQLAlchemy `class User(Base): __tablename__ = "users"`
  (1 model → `users`) and `src/org.entity.ts` with a TypeORM `@Entity("orgs") export
  class Org` (1 model → `orgs`), both naming declared tables, so 2 `MapsTo` edges.
- **`repo-b`**: **no database schema**, a pure TS code repo (`src/app.ts`). It
  proves the data plane is silent when no `.sql` schema is present (it contributes
  zero tables/columns/foreign keys and zero code→table links).

`node_modules` is **not** committed (`.gitignore` excludes it); the estate is
indexed hermetically with `ResolveMode::Off` (no Node/SCIP).

## Results

Measured over the committed `crossrepo_data` estate (aggregated across its repos;
`repo-b` contributes no schema).

| metric | value |
|---|---:|
| `schemas_detected` | **1** |
| `schemas_failed` | **0** |
| `statements_skipped` | **0** |
| `tables_total` | **3** |
| `columns_total` | **9** |
| `fks_total` | **3** |
| &nbsp;&nbsp;of which `fks_linked` | 3 |
| &nbsp;&nbsp;of which `fks_unresolved` | 0 |
| `reads_linked` | **3** |
| `reads_unresolved` | **0** |
| `writes_linked` | **2** |
| `writes_unresolved` | **0** |
| `orm_models_total` | **2** |
| &nbsp;&nbsp;of which `orm_models_linked` | 2 |
| &nbsp;&nbsp;of which `orm_models_unresolved` | 0 |

Reading the numbers:

- **1 schema, 3 tables, 9 columns:** repo-a's `schema.sql`, with `orgs` (2 cols),
  `users` (4 cols after the cumulative ALTER), and `memberships` (3 cols) as typed
  nodes. The composite `PRIMARY KEY (user_id, org_id)` flags both `memberships`
  columns; the interleaved `CREATE INDEX` is skipped, not errored.
- **3 foreign keys, all linked:** every FK target table+column is declared in the
  same schema, so all three resolve to a `Column` node at Extracted 0.95. This is
  the data-plane blast-radius foundation: `impact(orgs.id)` reaches `users.org_id`
  and `memberships.org_id` (the referencing columns) plus the owning `orgs` table.
- **3 reads, 2 writes, all linked (the §6.2 payoff):** repo-a's code touches the
  declared tables via raw SQL. `src/users.ts::getUserEmail` reads `users` (1);
  `src/users.ts::listUsersWithOrg` reads `users` and `orgs` via a JOIN (2);
  `writer.py::touch_last_login` writes `users` (UPDATE) and
  `writer.py::add_membership` writes `memberships` (INSERT) (2). Each is an Extracted
  0.95 fact, so `impact(users)` reaches the reading and writing code as a
  **will-break** verdict, and `explain` renders the `table —Reads/Writes→ code`
  chain. The dynamic `writer.py::delete_by_table` f-string is **not** captured (it is
  not a single literal), the honest dynamic-SQL miss, adding no edge.
- **2 ORM models, both linked (the M2b payoff):** `models.py::User` (SQLAlchemy
  `__tablename__ = "users"`) maps to `users` and `src/org.entity.ts::Org` (TypeORM
  `@Entity("orgs")`) maps to `orgs`, each a `User —MapsTo→ users` / `Org —MapsTo→ orgs`
  edge at Extracted 0.95. So `impact(users)` reaches the `User` model and
  `impact(orgs)` reaches the `Org` model (and transitively any code that instantiates
  them), and `context(users).mapped_by` lists `User`. No model names an undeclared
  table, so `orm_models_unresolved` is 0.
- **0 unresolved (reads/writes/FKs/ORM), 0 failed schemas, 0 skipped statements:** this
  estate is deliberately clean (every FK target and every code-referenced table is
  declared, the one schema parses, and it has no PL/pgSQL `DO`/`CREATE FUNCTION`
  bodies for the per-statement splitter to skip). The honesty cases (a query/FK
  against an undeclared table (no edge, counted), dynamic SQL (not captured), a
  malformed `.sql` (a `[data] FAILED …` diagnostic), and a mixed file that keeps its
  tables while skipping an unparseable `DO $$…$$` block (counted in
  `statements_skipped`, the file NOT failed)) are exercised by the unit tests in
  `data.rs`, `strata-data`, and the analyzers, not this estate's headline numbers.

## CI floors

`data_coverage_meets_documented_floors` gates: `schemas_detected ≥ 1`,
`tables_total ≥ 3`, `fks_linked ≥ 3`, `reads_linked ≥ 3`, `writes_linked ≥ 2`,
`orm_models_linked ≥ 2`, and a six-sided honesty pin `fks_unresolved == 0` /
`reads_unresolved == 0` / `writes_unresolved == 0` / `orm_models_unresolved == 0` /
`schemas_failed == 0` / `statements_skipped == 0`: a regression that silently
dropped a real link (inflating an `*_unresolved` counter), a whole schema (inflating
`schemas_failed`), or that started dropping statements from this clean estate
(inflating `statements_skipped`) fails the build. (The
`statements_skipped == 0` pin is specific to this deliberately PL/pgSQL-free fixture;
on a real repo a non-zero count is an honest informational signal, not a failure.)
Floors sit at the measured values (the fixture is deterministic); they are re-derived
from this report whenever the fixture changes.
