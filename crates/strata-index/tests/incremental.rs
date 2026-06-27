//! Incremental indexing tests — the `incremental == full` invariant.
//!
//! Every test that touches the invariant:
//!   1. Mutates real files on disk in a tempdir.
//!   2. Re-indexes into the SAME store (incremental path).
//!   3. Full-indexes the same post-mutation repo into a FRESH store.
//!   4. Compares the two graphs as order-independent sets of nodes AND edges
//!      (including provenance and confidence).
//!   5. Asserts files_parsed / files_reused counts to prove parsing was
//!      actually skipped for unchanged files.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use strata_core::{graph::Direction, AnalyzedFile, Edge, EdgeKind, Graph, Node, Uid};
use strata_index::{assemble_graph, build_graph, index_repo};
use strata_lang_ts::ResolveOptions;
use strata_store::{DuckGraphStore, GraphStore};

// ── Graph comparison helpers ──────────────────────────────────────────────────

/// A canonical, order-independent representation of one node.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone)]
struct NodeKey {
    uid: String,
    kind: String,
    name: String,
    fqn: String,
    path: String,
    provenance: String,
    // Confidence rounded to 4 decimal places to tolerate f32 round-trip noise.
    confidence_r4: String,
}

impl NodeKey {
    fn from(n: &Node) -> Self {
        NodeKey {
            uid: n.uid.to_string(),
            kind: format!("{:?}", n.kind),
            name: n.name.clone(),
            fqn: n.fqn.clone(),
            path: n.path.clone(),
            provenance: format!("{:?}", n.provenance),
            confidence_r4: format!("{:.4}", n.confidence.value()),
        }
    }
}

/// A canonical, order-independent representation of one edge.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone)]
struct EdgeKey {
    src: String,
    dst: String,
    kind: String,
    provenance: String,
    confidence_r4: String,
}

impl EdgeKey {
    fn from(e: &Edge) -> Self {
        EdgeKey {
            src: e.src.to_string(),
            dst: e.dst.to_string(),
            kind: format!("{:?}", e.kind),
            provenance: format!("{:?}", e.provenance),
            confidence_r4: format!("{:.4}", e.confidence.value()),
        }
    }
}

fn node_set(g: &Graph) -> BTreeSet<NodeKey> {
    g.nodes().map(NodeKey::from).collect()
}

fn edge_set(g: &Graph) -> BTreeSet<EdgeKey> {
    let uids: Vec<Uid> = g.nodes().map(|n| n.uid.clone()).collect();
    let mut set = BTreeSet::new();
    for uid in &uids {
        for (edge, _) in g.neighbors(uid, Direction::Outgoing, &[]) {
            set.insert(EdgeKey::from(edge));
        }
    }
    set
}

fn graphs_equal(a: &Graph, b: &Graph) -> Result<(), String> {
    let an = node_set(a);
    let bn = node_set(b);
    if an != bn {
        let only_a: Vec<_> = an.difference(&bn).cloned().collect();
        let only_b: Vec<_> = bn.difference(&an).cloned().collect();
        return Err(format!(
            "node sets differ.\n  only in incremental: {only_a:?}\n  only in full: {only_b:?}"
        ));
    }
    let ae = edge_set(a);
    let be = edge_set(b);
    if ae != be {
        let only_a: Vec<_> = ae.difference(&be).cloned().collect();
        let only_b: Vec<_> = be.difference(&ae).cloned().collect();
        return Err(format!(
            "edge sets differ.\n  only in incremental: {only_a:?}\n  only in full: {only_b:?}"
        ));
    }
    Ok(())
}

// ── Fixture helpers ────────────────────────────────────────────────────────────

/// Write the baseline 3-file fixture repo:
///   src/a.ts  — imports foo from b, calls foo() inside run()
///   src/b.ts  — exports foo (calls helper())
///   src/c.ts  — standalone module, exports baz()
fn write_base_fixture(root: &Path) {
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("src/a.ts"),
        "import { foo } from \"./b\";\nexport function run() { foo(); }\n",
    )
    .unwrap();
    fs::write(
        root.join("src/b.ts"),
        "function helper() {}\nexport function foo() { helper(); }\n",
    )
    .unwrap();
    fs::write(
        root.join("src/c.ts"),
        "export function baz() { return 42; }\n",
    )
    .unwrap();
}

// ── Test 1: incremental == full after modifying one file ──────────────────────

#[test]
fn incremental_equals_full_after_modify() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_base_fixture(root);

    // ── First full index into store_inc ──
    let mut store_inc = DuckGraphStore::open_in_memory().unwrap();
    let stats1 = index_repo(root, &mut store_inc).unwrap();
    // First index: no cache, everything parsed.
    assert_eq!(
        stats1.files_reused, 0,
        "first index must have files_reused == 0"
    );
    assert_eq!(stats1.files_parsed, 3, "first index must parse all 3 files");

    // ── Mutate ONE file on disk ──
    fs::write(
        root.join("src/b.ts"),
        // Added a new exported function newFn() calling helper()
        "function helper() {}\nexport function foo() { helper(); }\nexport function newFn() { helper(); }\n",
    )
    .unwrap();

    // ── Incremental re-index into the SAME store ──
    let stats2 = index_repo(root, &mut store_inc).unwrap();
    assert!(
        stats2.files_reused >= 1,
        "second index must reuse at least 1 unchanged file, got files_reused={}",
        stats2.files_reused
    );
    assert!(
        stats2.files_parsed >= 1,
        "second index must re-parse at least 1 changed file, got files_parsed={}",
        stats2.files_parsed
    );
    // Exactly: a.ts and c.ts are unchanged (reused=2), b.ts changed (parsed=1).
    assert_eq!(
        stats2.files_reused, 2,
        "a.ts and c.ts unchanged => reused=2"
    );
    assert_eq!(stats2.files_parsed, 1, "b.ts changed => parsed=1");

    let g_inc = store_inc.load_graph().unwrap();

    // ── Full index of the modified repo into a FRESH store ──
    let mut store_full = DuckGraphStore::open_in_memory().unwrap();
    index_repo(root, &mut store_full).unwrap();
    let g_full = store_full.load_graph().unwrap();

    // ── Compare: must be identical ──
    graphs_equal(&g_inc, &g_full).expect("incremental graph must equal full rebuild after modify");
}

// ── Test 2: incremental == full after adding a new file ───────────────────────

#[test]
fn incremental_equals_full_after_add() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_base_fixture(root);

    // First index.
    let mut store_inc = DuckGraphStore::open_in_memory().unwrap();
    index_repo(root, &mut store_inc).unwrap();

    // ── Add a new file that imports from a.ts ──
    fs::write(
        root.join("src/d.ts"),
        "import { run } from \"./a\";\nexport function main() { run(); }\n",
    )
    .unwrap();

    // ── Incremental re-index ──
    let stats = index_repo(root, &mut store_inc).unwrap();
    assert_eq!(stats.files_indexed, 4, "4 files after add");
    // The 3 original files are unchanged → reused; the new file is parsed.
    assert_eq!(stats.files_reused, 3, "original 3 files reused");
    assert_eq!(stats.files_parsed, 1, "new d.ts parsed");

    let g_inc = store_inc.load_graph().unwrap();

    // ── Full index of the 4-file repo into a fresh store ──
    let mut store_full = DuckGraphStore::open_in_memory().unwrap();
    index_repo(root, &mut store_full).unwrap();
    let g_full = store_full.load_graph().unwrap();

    graphs_equal(&g_inc, &g_full)
        .expect("incremental graph must equal full rebuild after file add");

    // The new cross-file CALLS edge must be present.
    let repo_name = root.file_name().unwrap().to_str().unwrap();
    let main_uid = Uid::new("ts", repo_name, "src/d.ts", "main", "");
    let run_uid = Uid::new("ts", repo_name, "src/a.ts", "run", "");
    let calls: Vec<Uid> = g_inc
        .neighbors(&main_uid, Direction::Outgoing, &[EdgeKind::Calls])
        .into_iter()
        .map(|(e, _)| e.dst.clone())
        .collect();
    assert!(
        calls.contains(&run_uid),
        "main (d.ts) must CALL run (a.ts) after add. got: {:?}",
        calls.iter().map(|u| u.as_str()).collect::<Vec<_>>()
    );
}

// ── Test 3: incremental == full after deleting a file ─────────────────────────

#[test]
fn incremental_equals_full_after_delete() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_base_fixture(root);

    // First index.
    let mut store_inc = DuckGraphStore::open_in_memory().unwrap();
    index_repo(root, &mut store_inc).unwrap();

    // ── Delete c.ts from disk ──
    fs::remove_file(root.join("src/c.ts")).unwrap();

    // ── Incremental re-index ──
    let stats = index_repo(root, &mut store_inc).unwrap();
    assert_eq!(stats.files_indexed, 2, "2 files after delete");
    // a.ts and b.ts unchanged → reused; c.ts gone → not counted at all.
    assert_eq!(stats.files_reused, 2, "a.ts and b.ts reused");
    assert_eq!(stats.files_parsed, 0, "no file re-parsed");

    let g_inc = store_inc.load_graph().unwrap();

    // ── Full index of the 2-file repo into a fresh store ──
    let mut store_full = DuckGraphStore::open_in_memory().unwrap();
    index_repo(root, &mut store_full).unwrap();
    let g_full = store_full.load_graph().unwrap();

    graphs_equal(&g_inc, &g_full)
        .expect("incremental graph must equal full rebuild after file delete");

    // c.ts nodes must be absent.
    let repo_name = root.file_name().unwrap().to_str().unwrap();
    let baz_uid = Uid::new("ts", repo_name, "src/c.ts", "baz", "");
    assert!(
        g_inc.get_node(&baz_uid).is_none(),
        "baz node from deleted c.ts must not appear in incremental graph"
    );

    // Verify the parse cache no longer has c.ts.
    let cache = store_inc.load_parse_cache().unwrap();
    assert!(
        !cache.contains_key("src/c.ts"),
        "parse cache must not retain entry for deleted c.ts"
    );
    let hashes = store_inc.load_file_hashes().unwrap();
    assert!(
        !hashes.contains_key("src/c.ts"),
        "file hashes must not retain entry for deleted c.ts"
    );
}

// ── Test 4: no-op re-index reuses everything ──────────────────────────────────

#[test]
fn no_op_reindex_reuses_everything() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_base_fixture(root);

    let mut store = DuckGraphStore::open_in_memory().unwrap();

    // First index.
    let stats1 = index_repo(root, &mut store).unwrap();
    let g1 = store.load_graph().unwrap();

    // Second index (no changes).
    let stats2 = index_repo(root, &mut store).unwrap();

    assert_eq!(
        stats2.files_parsed, 0,
        "no-op re-index must not parse any file"
    );
    assert_eq!(
        stats2.files_reused, stats1.files_indexed,
        "no-op re-index must reuse every file"
    );

    let g2 = store.load_graph().unwrap();
    graphs_equal(&g1, &g2).expect("graph must be unchanged after no-op re-index");
}

// ── Test 5: assemble_graph determinism + equivalence with build_graph ─────────

#[test]
fn assemble_graph_determinism_and_equivalence() {
    let files: BTreeMap<String, String> = [
        (
            "src/a.ts",
            "import { foo } from \"./b\"; export function run() { foo(); }",
        ),
        (
            "src/b.ts",
            "export function foo() {} export class C { m() { this.m(); } }",
        ),
        ("src/c.ts", "export function baz() {}"),
    ]
    .iter()
    .map(|(p, s)| (p.to_string(), s.to_string()))
    .collect();

    let opts = ResolveOptions::default();
    let repo = "myrepo";

    // build_graph is analyze-all + assemble_graph; call assemble directly too.
    let analyzed: BTreeMap<String, AnalyzedFile> = files
        .iter()
        .map(|(p, s)| (p.clone(), strata_lang_ts::analyze(p, s)))
        .collect();

    let g_build = build_graph(&files, repo, &opts);
    let g_assemble1 = assemble_graph(&analyzed, repo, &opts);
    let g_assemble2 = assemble_graph(&analyzed, repo, &opts);

    // Determinism: two calls to assemble_graph with the same input.
    graphs_equal(&g_assemble1, &g_assemble2).expect("assemble_graph must be deterministic");

    // Equivalence: build_graph == assemble_graph(analyze_all).
    graphs_equal(&g_build, &g_assemble1)
        .expect("build_graph must equal assemble_graph(analyze_all(files))");
}
