//! Integration tests for the CLI's single-db [`strata_mcp::GraphReloader`]
//! implementation — the concrete reload source the `strata mcp` server uses to
//! pick up an on-disk reindex without a restart (Track E3).
//!
//! The reloader's change signal is the `.strata/index.stamp` bytes when present
//! (written by the indexer after a successful index), falling back to the
//! graph.duckdb `(mtime, len)` so a db produced before this feature still
//! hot-reloads. `reload()` re-opens the db and loads the graph; a missing/corrupt
//! db yields `Err` and the caller keeps the graph it already had (degrade-safe).

use std::fs;
use std::path::Path;

use strata_cli::{SingleDbReloader, WorkspaceReloader};
use strata_core::{Confidence, Graph, Node, NodeKind, Provenance, Span, Uid};
use strata_index::{
    index_estate_with_options, index_repo, IndexStamp, ResolveMode, WorkspaceManifest,
};
use strata_mcp::GraphReloader;
use strata_store::{DuckGraphStore, GraphStore};

/// Index a one-file TS repo into `<root>/.strata/graph.duckdb` and return the db
/// path. After this, the stamp exists and the reloader's signal is established.
fn index_one_file(root: &Path) -> std::path::PathBuf {
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/a.ts"), "export function foo() {}\n").unwrap();
    let db = root.join(".strata").join("graph.duckdb");
    fs::create_dir_all(db.parent().unwrap()).unwrap();
    let mut store = DuckGraphStore::open(&db).unwrap();
    index_repo(root, &mut store).unwrap();
    db
}

/// Inject a node into the on-disk db and bump the stamp — what a real reindex
/// does, minus the parse. Proves the reloader notices and serves the new graph.
fn rewrite_db_add_node(db: &Path, strata_dir: &Path, marker_uid: &str) {
    // Load the persisted graph, add a node, save it back.
    let mut graph = {
        let store = DuckGraphStore::open(db).unwrap();
        store.load_graph().unwrap()
    };
    graph.add_node(Node {
        uid: Uid(marker_uid.into()),
        kind: NodeKind::Function,
        name: marker_uid.into(),
        fqn: marker_uid.into(),
        path: "src/injected.ts".into(),
        span: Span::default(),
        provenance: Provenance::Extracted,
        confidence: Confidence::new(1.0),
    });
    let (nodes, edges) = (graph.node_count(), graph.edge_count());
    {
        let mut store = DuckGraphStore::open(db).unwrap();
        store.save_graph(&graph).unwrap();
    }
    // Bump the stamp last (mirrors the indexer ordering).
    IndexStamp::new(nodes, edges).write(strata_dir).unwrap();
}

#[test]
fn single_db_reloader_picks_up_an_external_reindex() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let db = index_one_file(root);
    let strata = root.join(".strata");

    // The reloader is built against the just-indexed db: its baseline signal is
    // the current stamp, so nothing looks changed yet.
    let mut reloader = SingleDbReloader::new(&db);
    assert!(
        !reloader.changed(),
        "freshly-built reloader must not report a change before any reindex"
    );

    // Externally reindex (add a node + bump stamp).
    rewrite_db_add_node(&db, &strata, "INJECTED_MARKER");

    assert!(
        reloader.changed(),
        "reloader must detect the bumped stamp after an external reindex"
    );
    let g: Graph = reloader.reload().expect("reload should succeed");
    assert!(
        g.get_node(&Uid("INJECTED_MARKER".into())).is_some(),
        "the reloaded graph must contain the externally-injected node"
    );

    // A second check with no further write must report no change (no needless
    // reload) — the successful reload advanced the baseline signal.
    assert!(
        !reloader.changed(),
        "after a successful reload with no new write, changed() must be false"
    );
}

#[test]
fn single_db_reloader_degrades_safely_on_corrupt_db() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let db = index_one_file(root);
    let strata = root.join(".strata");

    let mut reloader = SingleDbReloader::new(&db);

    // Simulate a reindex-in-progress / corruption: bump the stamp (so changed()
    // fires) but make the db unopenable/unloadable.
    fs::write(&db, b"not a valid duckdb file").unwrap();
    IndexStamp::new(999, 999).write(&strata).unwrap();

    assert!(
        reloader.changed(),
        "the bumped stamp must register as changed"
    );
    let err = reloader.reload();
    assert!(
        err.is_err(),
        "reload of a corrupt db must be Err (degrade-safe), got {err:?}"
    );

    // The signal must NOT have advanced: a subsequent changed() still reports the
    // pending change, so the server will retry the reload on the next request.
    assert!(
        reloader.changed(),
        "a failed reload must not advance the signal — changed() stays true so it retries"
    );
}

/// Write a single-repo workspace manifest and the repo's one TS file, then index
/// the estate so each repo gets its `.strata/graph.duckdb` + stamp.
fn build_estate(root: &Path) -> std::path::PathBuf {
    let repo = root.join("repo_a");
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(repo.join("src/a.ts"), "export function foo() {}\n").unwrap();
    let manifest = root.join("strata.workspace.toml");
    fs::write(
        &manifest,
        "[workspace]\nname = \"estate\"\n\n[[repos]]\nname = \"repo_a\"\npath = \"repo_a\"\n",
    )
    .unwrap();

    let parsed = WorkspaceManifest::parse_file(&manifest).unwrap();
    index_estate_with_options(&parsed, &manifest, ResolveMode::Off, false);
    manifest
}

#[test]
fn workspace_reloader_picks_up_a_repo_reindex() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let manifest = build_estate(root);

    // Baselined to the freshly-indexed estate: nothing looks changed yet.
    let mut reloader = WorkspaceReloader::new(&manifest);
    assert!(
        !reloader.changed(),
        "freshly-built workspace reloader must not report a change"
    );

    // Reindex repo_a after adding a symbol: the repo's stamp bumps → changed().
    fs::write(
        root.join("repo_a/src/a.ts"),
        "export function foo() {}\nexport function bar() {}\n",
    )
    .unwrap();
    let parsed = WorkspaceManifest::parse_file(&manifest).unwrap();
    index_estate_with_options(&parsed, &manifest, ResolveMode::Off, false);

    assert!(
        reloader.changed(),
        "a repo reindex must flip the estate change signal"
    );
    let g: Graph = reloader.reload().expect("estate reload should succeed");
    let bar = Uid::new("ts", "repo_a", "src/a.ts", "bar", "");
    assert!(
        g.get_node(&bar).is_some(),
        "the re-linked estate must contain the newly-indexed symbol"
    );
    assert!(
        !reloader.changed(),
        "after a successful estate reload with no new write, changed() must be false"
    );
}
