//! `rename` over real indexed tempdir repositories.
//!
//! Each test builds a tiny repo, indexes it into an in-memory DuckDB store, then
//! runs [`rename`] against the loaded graph. The headline guarantees:
//!
//! * **the adversarial test** — a same-named identifier in a file the graph does
//!   NOT implicate (no edge to the target) is never touched;
//! * per-language happy paths (ts / py / cs): def + caller (+ importer) edited;
//! * dry-run lists, then `apply` writes, then a re-index + re-run is a no-op
//!   (the old name is gone);
//! * the collision guard (+ `force`), ambiguous candidates, the non-code error;
//! * edits carry the implicating edge's confidence;
//! * applied files re-parse clean (the analyzer finds the new name).

use std::path::Path;

use strata_core::Graph;
use strata_index::{
    index_repo, rename, RenameError, RenameOptions, RenameOutcome, DEF_SITE_CONFIDENCE,
};
use strata_store::{DuckGraphStore, GraphStore};
use tempfile::TempDir;

/// Write `content` to `<dir>/<rel>`, creating parent dirs.
fn write(dir: &Path, rel: &str, content: &str) {
    let path = dir.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir");
    }
    std::fs::write(path, content).expect("write file");
}

/// Index the repo at `dir` (resolution off — these fixtures rely on the
/// heuristic so no `scip-typescript` install is needed) and return the graph.
fn index(dir: &Path) -> Graph {
    let mut store = DuckGraphStore::open_in_memory().expect("open store");
    index_repo(dir, &mut store).expect("index repo");
    store.load_graph().expect("load graph")
}

/// Read `<dir>/<rel>` to a string.
fn read(dir: &Path, rel: &str) -> String {
    std::fs::read_to_string(dir.join(rel)).expect("read file")
}

/// The `Plan` variant's edits, panicking on a `Candidates` outcome.
fn plan_edits(outcome: &RenameOutcome) -> &[strata_index::Edit] {
    match outcome {
        RenameOutcome::Plan { edits, .. } => edits,
        RenameOutcome::Candidates { .. } => panic!("expected a Plan, got Candidates"),
    }
}

// ── THE MANDATORY ADVERSARIAL TEST ───────────────────────────────────────────────
//
// A same-named identifier in a NON-implicated file must NEVER be touched. Two
// independent `helper` functions live in two files with no edge between them; only
// the one the caller actually calls (same file) is implicated. Renaming via the
// caller's graph node must leave the unrelated `helper` in the other file intact.

#[test]
fn adversarial_same_name_in_non_implicated_file_is_never_touched() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    // a.ts: `helper` + `caller` that calls it (same-file Calls edge → a.ts is the
    // ONLY implicated file). b.ts: a DIFFERENT, unrelated `helper` — no edge to
    // a.ts's helper, so b.ts is not implicated.
    write(
        dir,
        "src/a.ts",
        "export function helper() { return 1; }\nexport function caller() { return helper(); }\n",
    );
    write(
        dir,
        "src/b.ts",
        "export function helper() { return 99; }\nexport function other() { return helper(); }\n",
    );
    let graph = index(dir);

    // Pin a.ts's helper by uid so resolution is unambiguous (two `helper` nodes).
    let target_uid = graph
        .nodes()
        .find(|n| n.name == "helper" && n.path == "src/a.ts")
        .expect("a.ts helper node")
        .uid
        .0
        .clone();

    let outcome = rename(
        &graph,
        dir,
        "helper",
        "assist",
        &RenameOptions {
            apply: true,
            uid: Some(target_uid),
            force: false,
        },
    )
    .expect("rename");

    // The edits and implicated files mention ONLY a.ts — never b.ts.
    match &outcome {
        RenameOutcome::Plan {
            implicated_files,
            edits,
            ..
        } => {
            assert!(
                implicated_files.iter().all(|f| f == "src/a.ts"),
                "only a.ts may be implicated; got {implicated_files:?}"
            );
            assert!(
                edits.iter().all(|e| e.file == "src/a.ts"),
                "no edit may target b.ts; got {edits:?}"
            );
        }
        other => panic!("expected Plan, got {other:?}"),
    }

    // On disk: a.ts's helper is renamed; b.ts is byte-identical (untouched).
    let a_after = read(dir, "src/a.ts");
    assert!(
        a_after.contains("function assist()") && a_after.contains("return assist()"),
        "a.ts's helper must be renamed to assist: {a_after}"
    );
    assert_eq!(
        read(dir, "src/b.ts"),
        "export function helper() { return 99; }\nexport function other() { return helper(); }\n",
        "the unrelated helper in the NON-implicated b.ts must be untouched"
    );
}

// ── per-language happy paths: def + caller (+ importer) edited ────────────────────

#[test]
fn ts_happy_path_def_caller_and_importer_edited() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    // b.ts defines `foo`; a.ts imports and calls it across files.
    write(dir, "src/b.ts", "export function foo() { return 1; }\n");
    write(
        dir,
        "src/a.ts",
        "import { foo } from \"./b\";\nexport function run() { return foo(); }\n",
    );
    let graph = index(dir);

    let outcome = rename(&graph, dir, "foo", "bar", &RenameOptions::default()).expect("rename");
    let edits = plan_edits(&outcome);
    // The def (b.ts) and the importer/caller (a.ts) are both edited.
    assert!(
        edits.iter().any(|e| e.file == "src/b.ts"),
        "the definition in b.ts must be edited: {edits:?}"
    );
    assert!(
        edits.iter().any(|e| e.file == "src/a.ts"),
        "the importer/caller in a.ts must be edited: {edits:?}"
    );
}

#[test]
fn py_happy_path_def_and_cross_module_caller_edited() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    // service.py defines make_user; client.py imports + calls it cross-module.
    write(dir, "pkg/service.py", "def make_user():\n    return 1\n");
    write(
        dir,
        "pkg/client.py",
        "from .service import make_user\n\n\ndef load():\n    return make_user()\n",
    );
    let graph = index(dir);

    let outcome = rename(
        &graph,
        dir,
        "make_user",
        "create_user",
        &RenameOptions::default(),
    )
    .expect("rename");
    let edits = plan_edits(&outcome);
    assert!(
        edits.iter().any(|e| e.file == "pkg/service.py"),
        "the def in service.py must be edited: {edits:?}"
    );
    assert!(
        edits.iter().any(|e| e.file == "pkg/client.py"),
        "the cross-module caller in client.py must be edited: {edits:?}"
    );
}

#[test]
fn cs_happy_path_def_and_cross_file_caller_edited() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    // Service.cs defines MakeUser; Client.cs calls it cross-file (unique name).
    write(
        dir,
        "src/Service.cs",
        concat!(
            "namespace App\n{\n",
            "    public class Service\n    {\n",
            "        public int MakeUser() { return 1; }\n",
            "    }\n}\n",
        ),
    );
    write(
        dir,
        "src/Client.cs",
        concat!(
            "namespace App\n{\n",
            "    public class Client\n    {\n",
            "        public int Load() { return new Service().MakeUser(); }\n",
            "    }\n}\n",
        ),
    );
    let graph = index(dir);

    let outcome = rename(
        &graph,
        dir,
        "MakeUser",
        "BuildUser",
        &RenameOptions::default(),
    )
    .expect("rename");
    let edits = plan_edits(&outcome);
    assert!(
        edits.iter().any(|e| e.file == "src/Service.cs"),
        "the def in Service.cs must be edited: {edits:?}"
    );
    assert!(
        edits.iter().any(|e| e.file == "src/Client.cs"),
        "the cross-file caller in Client.cs must be edited: {edits:?}"
    );
}

// ── dry-run lists, apply writes, re-index + re-run is a no-op ─────────────────────

#[test]
fn dry_run_lists_then_apply_writes_then_rerun_is_noop() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    write(
        dir,
        "src/a.ts",
        "export function helper() { return 1; }\nexport function caller() { return helper(); }\n",
    );
    let graph = index(dir);

    // Dry run (default): edits computed, nothing written.
    let dry = rename(&graph, dir, "helper", "assist", &RenameOptions::default()).expect("dry");
    match &dry {
        RenameOutcome::Plan { applied, edits, .. } => {
            assert!(!applied, "dry run must not write");
            assert!(!edits.is_empty(), "dry run must list edits");
        }
        other => panic!("expected Plan, got {other:?}"),
    }
    // Disk is unchanged after a dry run.
    assert!(
        read(dir, "src/a.ts").contains("function helper()"),
        "dry run must leave the file unchanged"
    );

    // Apply: writes the edits.
    let applied = rename(
        &graph,
        dir,
        "helper",
        "assist",
        &RenameOptions {
            apply: true,
            ..Default::default()
        },
    )
    .expect("apply");
    match &applied {
        RenameOutcome::Plan {
            applied,
            reindex_recommended,
            ..
        } => {
            assert!(applied, "apply must report applied");
            assert!(reindex_recommended, "apply must recommend a reindex");
        }
        other => panic!("expected Plan, got {other:?}"),
    }
    let after = read(dir, "src/a.ts");
    assert!(
        after.contains("function assist()") && after.contains("return assist()"),
        "apply must rewrite both occurrences: {after}"
    );
    assert!(!after.contains("helper"), "no `helper` may remain: {after}");

    // Re-index the now-renamed repo and re-run: `helper` no longer resolves → the
    // rename is a no-op (NotFound). This proves the apply was complete + reindex-safe.
    let graph2 = index(dir);
    let rerun = rename(&graph2, dir, "helper", "assist", &RenameOptions::default());
    assert!(
        matches!(rerun, Err(RenameError::NotFound(_))),
        "after apply + reindex, the old name must be gone: {rerun:?}"
    );
}

// ── collision guard + force ───────────────────────────────────────────────────────

#[test]
fn collision_guard_refuses_then_force_proceeds() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    // `helper` (target) and a pre-existing `taken` in the same repo. Renaming
    // helper → taken collides.
    write(
        dir,
        "src/a.ts",
        "export function helper() { return 1; }\nexport function caller() { return helper(); }\n",
    );
    write(dir, "src/c.ts", "export function taken() { return 0; }\n");
    let graph = index(dir);

    // Without force: a collision error naming `taken`.
    let err = rename(&graph, dir, "helper", "taken", &RenameOptions::default()).unwrap_err();
    match err {
        RenameError::Collision {
            new_name, count, ..
        } => {
            assert_eq!(new_name, "taken");
            assert!(count >= 1, "at least the existing `taken` collides");
        }
        other => panic!("expected Collision, got {other:?}"),
    }

    // With force: it proceeds (dry-run plan still, since apply is false).
    let forced = rename(
        &graph,
        dir,
        "helper",
        "taken",
        &RenameOptions {
            force: true,
            ..Default::default()
        },
    )
    .expect("force must bypass the collision guard");
    assert!(
        !plan_edits(&forced).is_empty(),
        "forced rename still plans edits"
    );
}

// ── ambiguous target → candidates ────────────────────────────────────────────────

#[test]
fn ambiguous_target_returns_candidates() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    // Two `helper` functions in two files, no edge between them → ambiguous.
    write(dir, "src/a.ts", "export function helper() { return 1; }\n");
    write(dir, "src/b.ts", "export function helper() { return 2; }\n");
    let graph = index(dir);

    let outcome =
        rename(&graph, dir, "helper", "assist", &RenameOptions::default()).expect("rename");
    match outcome {
        RenameOutcome::Candidates { candidates, .. } => {
            assert_eq!(candidates.len(), 2, "both helper nodes are candidates");
        }
        RenameOutcome::Plan { .. } => panic!("ambiguous target must yield Candidates"),
    }
}

// ── non-code target → clear queued error ─────────────────────────────────────────

#[test]
fn non_code_target_is_a_clear_queued_error() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    // A GraphQL schema field is a contract-plane node — not renameable yet.
    write(dir, "schema.graphql", "type Query {\n  getStats: Int\n}\n");
    let graph = index(dir);

    // The field's fqn is `Query.getStats` (its name is the display form
    // `QUERY getStats`); resolve by the fqn so the non-code node is hit.
    let err = rename(
        &graph,
        dir,
        "Query.getStats",
        "Query.getMetrics",
        &RenameOptions::default(),
    )
    .unwrap_err();
    match err {
        RenameError::NotCodeSymbol(name, kind) => {
            assert!(name.contains("getStats"), "error names the symbol: {name}");
            assert_eq!(kind, "GraphqlField", "the kind is reported: {kind}");
        }
        other => panic!("expected NotCodeSymbol, got {other:?}"),
    }
}

// ── edits carry the implicating edge's confidence ────────────────────────────────

#[test]
fn edits_carry_edge_confidences() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    write(
        dir,
        "src/a.ts",
        "export function helper() { return 1; }\nexport function caller() { return helper(); }\n",
    );
    let graph = index(dir);

    let outcome =
        rename(&graph, dir, "helper", "assist", &RenameOptions::default()).expect("rename");
    let edits = plan_edits(&outcome);
    // The definition-site edit (in helper's own file, on its own line) carries the
    // def-site confidence; every edit has a confidence in (0, 1].
    assert!(
        edits
            .iter()
            .any(|e| (e.confidence - DEF_SITE_CONFIDENCE).abs() < f32::EPSILON),
        "at least the def-site edit must carry DEF_SITE_CONFIDENCE; got {edits:?}"
    );
    assert!(
        edits
            .iter()
            .all(|e| e.confidence > 0.0 && e.confidence <= 1.0),
        "every edit confidence is a valid band value; got {edits:?}"
    );
}

// ── applied files re-parse clean (the analyzer finds the new name) ────────────────

#[test]
fn applied_files_reparse_clean() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    write(
        dir,
        "src/a.ts",
        "export function helper() { return 1; }\nexport function caller() { return helper(); }\n",
    );
    let graph = index(dir);

    rename(
        &graph,
        dir,
        "helper",
        "assist",
        &RenameOptions {
            apply: true,
            ..Default::default()
        },
    )
    .expect("apply");

    // Re-index the applied repo: the renamed symbol must be a real graph node
    // (the file parsed cleanly), and the old name must be absent.
    let graph2 = index(dir);
    assert!(
        graph2.nodes().any(|n| n.name == "assist"),
        "the renamed symbol `assist` must be a node after re-index"
    );
    assert!(
        !graph2.nodes().any(|n| n.name == "helper"),
        "the old name `helper` must not survive as a node"
    );
}
