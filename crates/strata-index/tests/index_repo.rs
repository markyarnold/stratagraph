//! Integration test for the `index_repo` IO shell against a temp fixture repo.
//!
//! Proves: gitignore-aware walking (a `node_modules/` file is skipped), the
//! cross-file CALLS edge survives a save/load round-trip through DuckDB, and a
//! file-hash entry exists per indexed file.

use std::fs;

use strata_core::{Direction, EdgeKind, NodeKind, Uid};
use strata_index::index_repo;
use strata_store::{DuckGraphStore, GraphStore};

/// Create a fixture repo on disk: `src/a.ts` imports and calls `foo` from
/// `src/lib/b.ts`; `node_modules/dep.ts` is gitignored and must be skipped.
fn write_fixture(root: &std::path::Path) {
    fs::create_dir_all(root.join("src/lib")).unwrap();
    fs::create_dir_all(root.join("node_modules")).unwrap();

    fs::write(root.join(".gitignore"), "node_modules/\n").unwrap();
    fs::write(
        root.join("src/a.ts"),
        "import { foo } from \"./lib/b\";\nexport function run() { foo(); }\n",
    )
    .unwrap();
    fs::write(root.join("src/lib/b.ts"), "export function foo() {}\n").unwrap();
    // A README that is not TS/JS and must be ignored by extension filtering.
    fs::write(root.join("README.md"), "# fixture\n").unwrap();
    // Gitignored TS file: present on disk, must NOT be indexed.
    fs::write(
        root.join("node_modules/dep.ts"),
        "export function shouldNotAppear() {}\n",
    )
    .unwrap();
}

#[test]
fn index_repo_round_trips_cross_file_call_and_respects_gitignore() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_fixture(root);

    let repo_name = root.file_name().unwrap().to_str().unwrap().to_string();

    let mut store = DuckGraphStore::open_in_memory().unwrap();
    let stats = index_repo(root, &mut store).unwrap();

    // Two TS files indexed (a.ts, lib/b.ts); README + node_modules excluded.
    assert_eq!(stats.files_indexed, 2, "only the two TS files are indexed");

    let g = store.load_graph().unwrap();

    let run = Uid::new("ts", &repo_name, "src/a.ts", "run", "");
    let foo = Uid::new("ts", &repo_name, "src/lib/b.ts", "foo", "");

    // Cross-file CALLS edge survives the round-trip.
    let calls: Vec<Uid> = g
        .neighbors(&run, Direction::Outgoing, &[EdgeKind::Calls])
        .into_iter()
        .map(|(e, _)| e.dst.clone())
        .collect();
    assert!(
        calls.contains(&foo),
        "run must CALL foo across files after load. got: {:?}",
        calls.iter().map(|u| u.as_str()).collect::<Vec<_>>()
    );

    // The gitignored file produced no nodes.
    let ignored = Uid::new(
        "ts",
        &repo_name,
        "node_modules/dep.ts",
        "shouldNotAppear",
        "",
    );
    assert!(
        g.get_node(&ignored).is_none(),
        "gitignored node_modules file must not be in the graph"
    );
    assert!(
        g.nodes().all(|n| !n.path.contains("node_modules")),
        "no node may originate from node_modules"
    );
    // And the non-TS README produced no Module node.
    assert!(
        g.nodes()
            .all(|n| !(n.kind == NodeKind::Module && n.path.ends_with("README.md"))),
        "README.md must not become a Module"
    );

    // A file-hash entry exists per indexed file (and only those).
    let hashes = store.load_file_hashes().unwrap();
    assert_eq!(hashes.len(), 2, "one hash per indexed file");
    assert!(hashes.contains_key("src/a.ts"));
    assert!(hashes.contains_key("src/lib/b.ts"));
    assert!(!hashes.contains_key("node_modules/dep.ts"));
    // Hashes are blake3 hex (64 chars).
    for hash in hashes.values() {
        assert_eq!(hash.len(), 64, "blake3 hex is 64 chars");
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }
}

// ── Visible degradation: a broken template is surfaced, not silently skipped ──
//
// THE dogfood regression: a real 8,324-line SAM template was silently dropped.
// This pins the fix's other half — when a file that looks like a CFN template
// fails to parse, it is counted (`templates_failed`) and its (path, error) is
// recorded in `infra_diagnostics`, while a GOOD template in the same repo still
// extracts. A silently skipped template must never happen again.
#[test]
fn broken_template_is_counted_and_diagnosed_while_good_one_extracts() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("infra")).unwrap();

    // A good, well-formed SAM template: one Serverless function.
    fs::write(
        root.join("infra/good.yaml"),
        "AWSTemplateFormatVersion: \"2010-09-09\"\n\
         Resources:\n\
         \x20\x20GoodFn:\n\
         \x20\x20\x20\x20Type: AWS::Serverless::Function\n\
         \x20\x20\x20\x20Properties:\n\
         \x20\x20\x20\x20\x20\x20Handler: app.handler\n",
    )
    .unwrap();

    // A broken template: it carries the CFN textual signal (`Resources` + `AWS::`)
    // so it is unmistakably an *attempted* template, but a TAB makes it invalid
    // YAML (tabs are illegal for indentation). It must be reported, not dropped.
    fs::write(
        root.join("infra/broken.yaml"),
        "Resources:\n\tBrokenFn:\n\t\tType: AWS::Serverless::Function\n",
    )
    .unwrap();

    let mut store = DuckGraphStore::open_in_memory().unwrap();
    let stats = index_repo(root, &mut store).unwrap();

    // The good template extracted (one detected template, its resource present).
    assert_eq!(
        stats.infra_link.templates_detected, 1,
        "the good template must still extract despite a broken sibling"
    );
    assert!(
        stats.infra_link.resources_total >= 1,
        "the good template's resource(s) must be extracted"
    );

    // The broken template is COUNTED, not silently skipped.
    assert_eq!(
        stats.infra_link.templates_failed, 1,
        "the broken template must be counted as a failure"
    );

    // … and DIAGNOSED: a diagnostic string names the broken path so the CLI can
    // print it. The good path must NOT appear (it did not fail).
    assert_eq!(
        stats.infra_diagnostics.len(),
        1,
        "exactly one failure diagnostic, got {:?}",
        stats.infra_diagnostics
    );
    let diag = &stats.infra_diagnostics[0];
    assert!(
        diag.contains("infra/broken.yaml"),
        "the diagnostic must name the broken template path, got {diag:?}"
    );
    assert!(
        !diag.contains("good.yaml"),
        "the good template must not appear in failure diagnostics, got {diag:?}"
    );
}
