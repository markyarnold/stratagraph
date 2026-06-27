//! Integration tests for the hot-reload change *stamp* the indexer writes.
//!
//! After a successful index, `index_repo` writes `<repo>/.strata/index.stamp`
//! atomically. The stamp is the cheap, race-free signal the MCP server keys off
//! to decide whether the on-disk graph changed since it last loaded — it is
//! written LAST (after the graph/file-hash/parse-cache saves return) and read
//! without ever opening the duckdb file.

use std::fs;

use strata_index::{index_repo, IndexStamp};
use strata_store::DuckGraphStore;

/// A minimal one-file TS repo.
fn write_one_file(root: &std::path::Path) {
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/a.ts"), "export function foo() {}\n").unwrap();
}

/// Add a second exported symbol so a re-index changes the graph (node count up).
fn add_second_symbol(root: &std::path::Path) {
    fs::write(
        root.join("src/a.ts"),
        "export function foo() {}\nexport function bar() {}\n",
    )
    .unwrap();
}

#[test]
fn index_repo_writes_index_stamp() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_one_file(root);

    // The store lives in the repo's .strata dir (the canonical location), so the
    // indexer can place the stamp next to it.
    let strata = root.join(".strata");
    fs::create_dir_all(&strata).unwrap();
    let mut store = DuckGraphStore::open(&strata.join("graph.duckdb")).unwrap();
    index_repo(root, &mut store).unwrap();

    let stamp_path = strata.join("index.stamp");
    assert!(
        stamp_path.exists(),
        "index_repo must write {} after a successful index",
        stamp_path.display()
    );

    // The read helper returns the raw bytes (the change signal) and never opens
    // the duckdb file.
    let bytes = IndexStamp::read(&strata).expect("stamp should be readable");
    assert!(!bytes.is_empty(), "stamp must carry bytes");
    // The committed format is a JSON one-liner carrying the engine id and counts.
    let text = String::from_utf8(bytes).unwrap();
    assert!(
        text.contains(strata_core::ENGINE_ID),
        "stamp must record the engine id; got {text}"
    );
}

#[test]
fn stamp_signal_changes_after_a_graph_changing_reindex() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_one_file(root);

    let strata = root.join(".strata");
    fs::create_dir_all(&strata).unwrap();
    let db = strata.join("graph.duckdb");

    {
        let mut store = DuckGraphStore::open(&db).unwrap();
        index_repo(root, &mut store).unwrap();
    }
    let first = IndexStamp::read(&strata).expect("first stamp");

    // Re-index after a graph-changing edit: the signal must differ.
    add_second_symbol(root);
    {
        let mut store = DuckGraphStore::open(&db).unwrap();
        index_repo(root, &mut store).unwrap();
    }
    let second = IndexStamp::read(&strata).expect("second stamp");

    assert_ne!(
        first, second,
        "the stamp signal must change after a graph-changing re-index"
    );
}

#[test]
fn stamp_write_is_atomic_no_partial_file() {
    // The stamp is written via a temp sibling + rename, so a reader never sees a
    // partially-written stamp. We prove the *mechanism*: after the write, no
    // temp sibling is left behind, and the stamp parses as a complete JSON line.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_one_file(root);

    let strata = root.join(".strata");
    fs::create_dir_all(&strata).unwrap();
    let mut store = DuckGraphStore::open(&strata.join("graph.duckdb")).unwrap();
    index_repo(root, &mut store).unwrap();

    // No leftover temp sibling (the rename target is the only stamp artifact).
    let leftovers: Vec<_> = fs::read_dir(&strata)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("index.stamp") && n != "index.stamp")
        .collect();
    assert!(
        leftovers.is_empty(),
        "atomic write must leave no temp sibling; found {leftovers:?}"
    );

    // The stamp is a complete, parseable JSON object (never a truncated prefix).
    let bytes = IndexStamp::read(&strata).expect("stamp readable");
    let v: serde_json::Value = serde_json::from_slice(&bytes).expect("stamp is complete JSON");
    assert!(v.get("engine_id").is_some(), "stamp has engine_id");
    assert!(v.get("nodes").is_some(), "stamp has node count");
    assert!(v.get("edges").is_some(), "stamp has edge count");
}
