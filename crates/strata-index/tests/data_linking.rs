//! Data-plane linking tests (Slice 16, D3).
//!
//! The `crossrepo_data/repo-a/schema.sql` fixture declares a small multi-table FK
//! graph (orgs / users / memberships) plus a cumulative `ALTER`. These tests prove
//! the schema substrate flows into the graph and that `context`/`impact` work over
//! tables and columns:
//!
//! - `context(table)` lists the table's columns (the `HasColumn` membership edge,
//!   surfaced through the shared `members` bucket — the one dispatch).
//! - `impact(column)` reaches its owning table and the columns that reference it
//!   via a `ForeignKey` edge (the data-plane dependency edge the reverse walk
//!   traverses).
//! - the band guardrail: every data edge is an Extracted fact (0.95–1.0).

use std::path::{Path, PathBuf};

use strata_core::{
    context, explain, impact, Direction, EdgeKind, ImpactOptions, NodeKind, Provenance, Uid,
};
use strata_data::{SchemaAdapter, SqlSchemaAdapter};
use strata_index::{
    assemble_graph_with_data, build_data_plane, index_repo_with_options, IndexOptions, ResolveMode,
};
use strata_store::{DuckGraphStore, GraphStore};

const REPO: &str = "repo-a";

fn fixture_schema() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("crossrepo_data")
        .join("repo-a")
        .join("schema.sql");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The data-plane graph built from the fixture schema (path `schema.sql`).
fn build() -> strata_core::Graph {
    let model = SqlSchemaAdapter
        .extract("schema.sql", &fixture_schema())
        .expect("fixture schema parses");
    let (g, _cov) = assemble_graph_with_data(REPO, &[model]);
    g
}

/// The UID of a table node in the fixture.
fn table_uid(table: &str) -> Uid {
    Uid::new("data", REPO, "schema.sql", table, "")
}

/// The UID of a column node in the fixture (`table.column` fqn).
fn column_uid(table: &str, column: &str) -> Uid {
    Uid::new("data", REPO, "schema.sql", &format!("{table}.{column}"), "")
}

// ── Test 1: context(table) lists its columns. ────────────────────────────────

#[test]
fn context_table_lists_its_columns_in_members() {
    let g = build();
    let ctx = context(&g, &table_uid("users")).expect("users table is in the graph");
    assert_eq!(ctx.node.kind, NodeKind::Table);

    // The `users` table's columns surface through `members` (HasColumn). After the
    // cumulative ALTER, `last_login` is present too.
    let members: Vec<&str> = ctx.members.iter().map(|n| n.name.as_str()).collect();
    for expected in ["id", "email", "org_id", "last_login"] {
        assert!(
            members.contains(&expected),
            "context(users).members must list column {expected}; got {members:?}"
        );
    }
    // Every member is a Column node.
    assert!(
        ctx.members.iter().all(|n| n.kind == NodeKind::Column),
        "every member of a table is a Column"
    );
}

// ── Test 2: impact(column) reaches its table AND FK-referencing columns. ──────

#[test]
fn impact_referenced_column_reaches_its_table_and_referencing_columns() {
    let g = build();
    // `orgs.id` is referenced by users.org_id and memberships.org_id (both via a
    // ForeignKey edge), and is owned by the orgs table (HasColumn, incoming).
    let r = impact(&g, &column_uid("orgs", "id"), &ImpactOptions::default());
    let reached: Vec<&str> = r.affected.iter().map(|a| a.uid.as_str()).collect();

    // The referencing columns are in the blast radius (the FK edge is traversed).
    assert!(
        reached.contains(&column_uid("users", "org_id").as_str()),
        "impact(orgs.id) must reach users.org_id via the FK edge; got {reached:?}"
    );
    assert!(
        reached.contains(&column_uid("memberships", "org_id").as_str()),
        "impact(orgs.id) must reach memberships.org_id via the FK edge; got {reached:?}"
    );
    // The owning table reaches the column via its HasColumn edge (incoming), so
    // changing the column surfaces the table as affected too.
    assert!(
        reached.contains(&table_uid("orgs").as_str()),
        "impact(orgs.id) must reach the owning orgs table; got {reached:?}"
    );
}

#[test]
fn impact_referenced_column_is_will_break_via_extracted_fk() {
    // A ForeignKey edge is an Extracted fact (0.95) → a clean, above-floor reach,
    // so the referencing column is labelled will_break (not "may affect").
    let g = build();
    let r = impact(&g, &column_uid("users", "id"), &ImpactOptions::default());
    let mem_user = r
        .affected
        .iter()
        .find(|a| a.uid == column_uid("memberships", "user_id"))
        .expect("memberships.user_id references users.id");
    assert!(
        mem_user.will_break,
        "an Extracted FK edge (0.95) yields a will-break verdict, got {mem_user:?}"
    );
    assert!(!mem_user.ambiguous, "an FK fact is never ambiguous");
}

// ── Test 3: the band guardrail is non-vacuous over data edges. ───────────────

#[test]
fn data_edges_satisfy_the_extracted_band_non_vacuously() {
    let g = build();

    // Confirm the graph actually contains BOTH a HasColumn and a ForeignKey edge,
    // so the invariant below exercises the data plane, not vacuously.
    let mut seen_has_column = false;
    let mut seen_foreign_key = false;
    let uids: Vec<Uid> = g.nodes().map(|n| n.uid.clone()).collect();
    for uid in &uids {
        for (edge, _) in g.neighbors(uid, Direction::Outgoing, &[]) {
            match edge.kind {
                EdgeKind::HasColumn => seen_has_column = true,
                EdgeKind::ForeignKey => seen_foreign_key = true,
                _ => {}
            }
            // Every data-plane edge is an Extracted fact in the EXTRACTED band.
            assert_eq!(
                edge.provenance,
                Provenance::Extracted,
                "data edge {:?} {}→{} must be Extracted",
                edge.kind,
                edge.src.as_str(),
                edge.dst.as_str()
            );
            let conf = edge.confidence.value();
            assert!(
                (0.95..=1.0).contains(&conf),
                "data edge {:?} conf {conf} must be in the EXTRACTED band [0.95, 1.0]",
                edge.kind
            );
        }
    }
    assert!(
        seen_has_column,
        "expected a HasColumn edge in the data graph"
    );
    assert!(
        seen_foreign_key,
        "expected a ForeignKey edge in the data graph"
    );
}

// ── Test 4: pure build_data_plane idempotency over the fixture. ──────────────

#[test]
fn build_data_plane_over_fixture_node_idempotent_and_coverage_pure() {
    let model = SqlSchemaAdapter
        .extract("schema.sql", &fixture_schema())
        .expect("fixture schema parses");
    let mut g = strata_core::Graph::new();
    let models = std::slice::from_ref(&model);
    let cov1 = build_data_plane(&mut g, REPO, models, &[], &[]);
    let n1 = g.node_count();
    let cov2 = build_data_plane(&mut g, REPO, models, &[], &[]);
    // Nodes are keyed by UID — a re-build adds none. (Edges accumulate via
    // `Graph::add_edge`, but the production path always builds a fresh graph.)
    assert_eq!(g.node_count(), n1, "nodes are idempotent by UID");
    // Coverage is a pure function of the input — identical both times.
    assert_eq!(cov1, cov2);
    // The fixture's headline numbers (cross-checked by data_coverage.rs).
    assert_eq!(cov1.tables_total, 3);
    assert_eq!(cov1.columns_total, 9);
    assert_eq!(cov1.fks_total, 3);
    assert_eq!(cov1.fks_linked, 3);
    assert_eq!(cov1.fks_unresolved, 0);
}

// ── The §6.2 demo: index repo-a end-to-end; impact(table) reaches the code. ──
//
// The flagship deliverable: change an RDS column/table → find the services/
// functions that read or write it. We index the committed `crossrepo_data/repo-a`
// fixture (schema.sql + src/users.ts that SELECTs + writer.py that UPDATE/INSERTs)
// through the REAL indexer (`index_repo_with_options`, the same path `strata index`
// uses), then prove `impact(users)` reaches the reading code AND the writing code,
// and that `explain` renders the code→table chain. Dynamic SQL stays unlinked.

fn fixture_dir(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_all(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// Index `crossrepo_data/repo-a` (ResolveMode::Off, hermetic) and return the loaded
/// graph plus the repo_name its UIDs use (the temp dir basename).
fn index_repo_a() -> (strata_core::Graph, String, tempfile::TempDir) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dst = tmp.path().join("repo-a");
    copy_dir_all(&fixture_dir("crossrepo_data").join("repo-a"), &dst).expect("copy repo-a");
    let repo_name = "repo-a".to_string();
    let mut store = DuckGraphStore::open_in_memory().expect("store");
    let options = IndexOptions {
        resolve_mode: ResolveMode::Off,
        allow_install: false,
        include_vendored: false,
    };
    index_repo_with_options(&dst, &mut store, &options).expect("index repo-a");
    let g = store.load_graph().expect("load graph");
    (g, repo_name, tmp)
}

/// A data-plane table UID under the indexed repo.
fn indexed_table_uid(repo: &str, table: &str) -> Uid {
    Uid::new("data", repo, "schema.sql", table, "")
}

#[test]
fn s6_2_demo_impact_users_reaches_reading_and_writing_code() {
    let (g, repo, _tmp) = index_repo_a();

    // The code nodes the data plane should have linked (ts SELECT + py UPDATE).
    let reader = Uid::new("ts", &repo, "src/users.ts", "getUserEmail", "");
    let writer = Uid::new("py", &repo, "writer.py", "touch_last_login", "");
    assert!(
        g.get_node(&reader).is_some(),
        "the reading TS function node exists"
    );
    assert!(
        g.get_node(&writer).is_some(),
        "the writing Python function node exists"
    );

    // impact(users) reverse-walks the incoming Reads/Writes edges → reaches BOTH.
    let users = indexed_table_uid(&repo, "users");
    let r = impact(&g, &users, &ImpactOptions::default());
    let reached: Vec<&str> = r.affected.iter().map(|a| a.uid.as_str()).collect();
    assert!(
        reached.contains(&reader.as_str()),
        "impact(users) must reach the code that SELECTs from it (getUserEmail); got {reached:?}"
    );
    assert!(
        reached.contains(&writer.as_str()),
        "impact(users) must reach the code that UPDATEs it (touch_last_login); got {reached:?}"
    );

    // Both reach via an Extracted fact → labelled will-break, non-ambiguous.
    let reader_aff = r.affected.iter().find(|a| a.uid == reader).unwrap();
    assert!(
        reader_aff.will_break && !reader_aff.ambiguous,
        "the SELECT reader reaches via an Extracted Reads edge ⇒ will-break: {reader_aff:?}"
    );

    // explain renders the code→table chain: a one-hop Reads edge users→getUserEmail.
    let e = explain(&g, &users, &reader, &ImpactOptions::default())
        .expect("the reader is reachable, so it explains");
    assert_eq!(
        e.hops.len(),
        1,
        "a direct code→table read is one hop: {e:?}"
    );
    assert_eq!(e.hops[0].edge_kind, EdgeKind::Reads);
    assert_eq!(e.hops[0].from.as_str(), users.as_str());
    assert_eq!(e.hops[0].to.as_str(), reader.as_str());
    assert_eq!(e.hops[0].provenance, Provenance::Extracted);

    // And the write chain: a one-hop Writes edge users→touch_last_login.
    let ew = explain(&g, &users, &writer, &ImpactOptions::default()).expect("writer reachable");
    assert_eq!(ew.hops.len(), 1);
    assert_eq!(ew.hops[0].edge_kind, EdgeKind::Writes);
}

#[test]
fn s6_2_demo_join_read_reaches_both_tables_and_dynamic_write_is_unlinked() {
    let (g, repo, _tmp) = index_repo_a();

    // The JOIN read (`SELECT … FROM users JOIN orgs`) links to BOTH tables, so
    // impact(orgs) reaches `listUsersWithOrg` via a Reads edge.
    let join_fn = Uid::new("ts", &repo, "src/users.ts", "listUsersWithOrg", "");
    let orgs = indexed_table_uid(&repo, "orgs");
    let r_orgs = impact(&g, &orgs, &ImpactOptions::default());
    assert!(
        r_orgs.affected.iter().any(|a| a.uid == join_fn),
        "impact(orgs) reaches the JOIN reader listUsersWithOrg via a Reads edge"
    );

    // impact(memberships) reaches the INSERT writer `add_membership`.
    let add_fn = Uid::new("py", &repo, "writer.py", "add_membership", "");
    let memberships = indexed_table_uid(&repo, "memberships");
    let r_mem = impact(&g, &memberships, &ImpactOptions::default());
    assert!(
        r_mem.affected.iter().any(|a| a.uid == add_fn),
        "impact(memberships) reaches the INSERT writer add_membership"
    );

    // The dynamic f-string write (`delete_by_table`) is NOT a single literal → it is
    // honestly unlinked. No table-targeted Writes edge originates from it (the
    // never-invent rule over a dynamic query).
    let dynamic_fn = Uid::new("py", &repo, "writer.py", "delete_by_table", "");
    let writes_out: Vec<EdgeKind> = g
        .neighbors(&dynamic_fn, Direction::Outgoing, &[EdgeKind::Writes])
        .into_iter()
        .map(|(e, _)| e.kind)
        .collect();
    assert!(
        writes_out.is_empty(),
        "an interpolated DELETE (dynamic SQL) must produce NO Writes edge; got {writes_out:?}"
    );
}

#[test]
fn s6_2_demo_impact_column_reaches_code_transitively_through_its_table() {
    // The column-level §6.2: change `users.email` → find the code that touches it.
    // The code→table link is table-level (M2 documented bound: a `SELECT email
    // FROM users` is a Read of `users`, not `users.email`), so a column change
    // reaches the reading/writing code TRANSITIVELY: impact(users.email) reverse-
    // walks `users.email ←HasColumn— users ←Reads/Writes— code`. So a column change
    // still surfaces its dependent code (one hop further out than the table itself).
    let (g, repo, _tmp) = index_repo_a();
    let email = Uid::new("data", &repo, "schema.sql", "users.email", "");
    let r = impact(&g, &email, &ImpactOptions::default());
    let reached: Vec<&str> = r.affected.iter().map(|a| a.uid.as_str()).collect();

    // The owning table is reached at depth 1 (incoming HasColumn).
    let users = indexed_table_uid(&repo, "users");
    assert!(
        reached.contains(&users.as_str()),
        "impact(users.email) reaches its owning users table; got {reached:?}"
    );
    // The reading code is reached transitively (table-level link, one hop deeper).
    let reader = Uid::new("ts", &repo, "src/users.ts", "getUserEmail", "");
    let reader_aff = r
        .affected
        .iter()
        .find(|a| a.uid == reader)
        .expect("impact(users.email) reaches the code that reads users (transitively)");
    assert_eq!(
        reader_aff.depth, 2,
        "the reading code is one hop beyond the table (column→table→code)"
    );
    // Still an above-floor, clean reach (0.95 × 0.95) → will-break.
    assert!(
        reader_aff.will_break && !reader_aff.ambiguous,
        "the transitive column→table→code reach is a clean Extracted path ⇒ will-break"
    );
}

// ── M2b mixed-lang: SQL DDL + a py SQLAlchemy model + a ts TypeORM model. ─────
//
// Index a temp repo carrying a `schema.sql`, a Python SQLAlchemy model
// (`__tablename__`), and a TS TypeORM model (`@Entity`), all mapping to declared
// tables, through the REAL indexer. Both `MapsTo` edges must land at Extracted 0.95,
// impact(table) must reach each model, and the existing planes (Table/Column nodes,
// the code symbols) must be intact.

/// Write a small mixed-language repo (schema.sql + models.py + user.entity.ts) into
/// `dir` and index it hermetically (ResolveMode::Off). Returns the graph + repo_name.
fn index_mixed_orm_repo() -> (strata_core::Graph, String, tempfile::TempDir) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("orm-repo");
    std::fs::create_dir_all(root.join("src")).expect("mkdir src");

    // Two declared tables: `users` and `orgs`.
    std::fs::write(
        root.join("schema.sql"),
        "CREATE TABLE users (id BIGINT PRIMARY KEY, email TEXT);\n\
         CREATE TABLE orgs (id BIGINT PRIMARY KEY, name TEXT);\n",
    )
    .expect("write schema.sql");
    // Python SQLAlchemy model mapping User -> users.
    std::fs::write(
        root.join("models.py"),
        "class User(Base):\n    __tablename__ = \"users\"\n    id = Column(Integer)\n",
    )
    .expect("write models.py");
    // TS TypeORM model mapping Org -> orgs.
    std::fs::write(
        root.join("src").join("org.entity.ts"),
        "@Entity(\"orgs\")\nexport class Org {\n  id: number;\n}\n",
    )
    .expect("write org.entity.ts");

    let mut store = DuckGraphStore::open_in_memory().expect("store");
    let options = IndexOptions {
        resolve_mode: ResolveMode::Off,
        allow_install: false,
        include_vendored: false,
    };
    index_repo_with_options(&root, &mut store, &options).expect("index orm-repo");
    let g = store.load_graph().expect("load graph");
    let repo_name = root.file_name().unwrap().to_string_lossy().to_string();
    (g, repo_name, tmp)
}

#[test]
fn mixed_orm_repo_links_py_and_ts_models_to_tables_planes_intact() {
    let (g, repo, _tmp) = index_mixed_orm_repo();

    // The two declared tables exist (data plane intact).
    let users = indexed_table_uid(&repo, "users");
    let orgs = indexed_table_uid(&repo, "orgs");
    assert!(g.get_node(&users).is_some(), "users Table node exists");
    assert!(g.get_node(&orgs).is_some(), "orgs Table node exists");
    // The code symbols exist (code plane intact).
    let user_model = Uid::new("py", &repo, "models.py", "User", "");
    let org_model = Uid::new("ts", &repo, "src/org.entity.ts", "Org", "");
    assert!(
        g.get_node(&user_model).is_some(),
        "the py User class node exists"
    );
    assert!(
        g.get_node(&org_model).is_some(),
        "the ts Org class node exists"
    );

    // The py SQLAlchemy model maps to users (MapsTo, Extracted 0.95).
    let py_targets: Vec<(&str, Provenance, f32)> = g
        .neighbors(&user_model, Direction::Outgoing, &[EdgeKind::MapsTo])
        .into_iter()
        .map(|(e, n)| (n.name.as_str(), e.provenance, e.confidence.value()))
        .collect();
    assert_eq!(py_targets.len(), 1, "py User has one MapsTo edge");
    assert_eq!(py_targets[0].0, "users");
    assert_eq!(py_targets[0].1, Provenance::Extracted);
    assert!(py_targets[0].2 >= 0.95);

    // The ts TypeORM model maps to orgs (MapsTo, Extracted 0.95).
    let ts_targets: Vec<&str> = g
        .neighbors(&org_model, Direction::Outgoing, &[EdgeKind::MapsTo])
        .into_iter()
        .map(|(_, n)| n.name.as_str())
        .collect();
    assert_eq!(ts_targets, vec!["orgs"], "ts Org maps to orgs");

    // impact(users) reaches the py model; impact(orgs) reaches the ts model.
    let ru = impact(&g, &users, &ImpactOptions::default());
    assert!(
        ru.affected.iter().any(|a| a.uid == user_model),
        "impact(users) reaches the py User model via MapsTo"
    );
    let ro = impact(&g, &orgs, &ImpactOptions::default());
    assert!(
        ro.affected.iter().any(|a| a.uid == org_model),
        "impact(orgs) reaches the ts Org model via MapsTo"
    );
}
