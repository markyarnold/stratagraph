//! Mode + degradation + coverage tests (brief Definition of Done #9–#12) plus
//! the gated live integration test.
//!
//! Tests 9–12 are hermetic (no Node): the mode handler degrades/hard-fails on a
//! plain temp repo, and coverage is asserted against the committed fixture index
//! via `assemble_with_coverage`. The live test (gated behind `STRATA_SCIP_LIVE=1`)
//! copies the committed fixture to a tempdir and runs `index_repo` end-to-end in
//! `Auto` mode with installs permitted, proving the same `RESOLVED` edges as the
//! hermetic accuracy tests.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use strata_core::{Direction, EdgeKind, Provenance, Uid};
use strata_index::{
    assemble_with_coverage, index_repo_with_options, IndexError, IndexOptions, ResolveMode,
};
use strata_lang_ts::{analyze, ResolveOptions};
use strata_scip::ScipResolver;
use strata_store::{DuckGraphStore, GraphStore};

const REPO: &str = "strata-index-resolve";
const RESOLVE_INDEX: &[u8] = include_bytes!("fixtures/resolve/index.scip");

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("resolve")
}

/// A minimal on-disk TS repo (no node_modules) in a fresh tempdir.
fn write_simple_repo(root: &Path) {
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("src/a.ts"),
        "import { foo } from \"./b\";\nexport function run() { foo(); }\n",
    )
    .unwrap();
    fs::write(root.join("src/b.ts"), "export function foo() {}\n").unwrap();
}

// ── Test 9: Off → no SCIP, heuristic only (sites_resolved == 0) ──────────────

#[test]
fn off_mode_is_heuristic_only() {
    let dir = tempfile::tempdir().unwrap();
    write_simple_repo(dir.path());
    let mut store = DuckGraphStore::open_in_memory().unwrap();

    let opts = IndexOptions {
        resolve_mode: ResolveMode::Off,
        allow_install: false,
        include_vendored: false,
    };
    let stats = index_repo_with_options(dir.path(), &mut store, &opts).expect("off succeeds");

    assert_eq!(stats.resolution_mode, ResolveMode::Off);
    assert!(!stats.degraded, "off is not a degradation");
    assert_eq!(stats.sites_resolved, 0, "off must not resolve via SCIP");
    assert!(stats.sites_total >= 1, "the foo() call is a site");

    // The cross-file call is the slice-1 heuristic edge (Inferred, not Resolved).
    let g = store.load_graph().unwrap();
    let repo = dir.path().file_name().unwrap().to_str().unwrap();
    let run = Uid::new("ts", repo, "src/a.ts", "run", "");
    let foo = Uid::new("ts", repo, "src/b.ts", "foo", "");
    let prov = g
        .neighbors(&run, Direction::Outgoing, &[EdgeKind::Calls])
        .into_iter()
        .find(|(e, _)| e.dst == foo)
        .map(|(e, _)| e.provenance);
    assert_eq!(
        prov,
        Some(Provenance::Inferred),
        "heuristic edge in off mode"
    );
}

// ── Test 10: Auto with SCIP unavailable → succeeds, degraded ─────────────────

#[test]
fn auto_mode_degrades_when_scip_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    write_simple_repo(dir.path()); // no node_modules ⇒ scip not runnable
    let mut store = DuckGraphStore::open_in_memory().unwrap();

    let opts = IndexOptions {
        resolve_mode: ResolveMode::Auto,
        allow_install: false,
        include_vendored: false,
    };
    // R1: indexing SUCCEEDS even though SCIP could not run.
    let stats = index_repo_with_options(dir.path(), &mut store, &opts)
        .expect("auto must succeed in degraded mode");

    assert_eq!(stats.resolution_mode, ResolveMode::Auto);
    assert!(stats.degraded, "auto records that it degraded to heuristic");
    assert_eq!(stats.sites_resolved, 0, "no SCIP resolutions when degraded");

    // The graph is still produced (heuristic edges present).
    let g = store.load_graph().unwrap();
    assert!(g.node_count() > 0, "a graph is still produced");
}

// ── Test 11: On with SCIP unavailable → hard error (R5) ──────────────────────

#[test]
fn on_mode_hard_fails_when_scip_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    write_simple_repo(dir.path()); // no node_modules ⇒ scip not runnable
    let mut store = DuckGraphStore::open_in_memory().unwrap();

    let opts = IndexOptions {
        resolve_mode: ResolveMode::On,
        allow_install: false,
        include_vendored: false,
    };
    let err = match index_repo_with_options(dir.path(), &mut store, &opts) {
        Err(e) => e,
        Ok(_) => panic!("on mode must hard-fail when scip-typescript cannot run"),
    };
    match err {
        IndexError::Scip(inner) => {
            let msg = inner.to_string();
            assert!(
                msg.contains("on") && msg.contains("typescript"),
                "error must explain the `on` prerequisite, got: {msg}"
            );
        }
        other => panic!("expected IndexError::Scip, got {other:?}"),
    }
}

// ── Test 12: coverage metric consistent with the fixture (hermetic) ──────────

#[test]
fn coverage_counts_match_fixture() {
    // Analyze the committed fixture sources and resolve against the committed
    // index — no Node — and assert the exact coverage tally.
    let sources = load_fixture_sources();
    let analyzed: BTreeMap<_, _> = sources
        .iter()
        .map(|(p, s)| (p.clone(), analyze(p, s)))
        .collect();
    let resolver = ScipResolver::from_bytes(RESOLVE_INDEX).expect("index parses");

    let (_g, cov) = assemble_with_coverage(
        &analyzed,
        REPO,
        &ResolveOptions::default(),
        Some(&resolver),
        &sources,
    );

    // 8 call sites: a.ts {bar, NS.nsFn, fooReexport, dup} = 4 resolved;
    // uni.ts {café, naïve, nf} = 3 resolved; dyn.ts {obj.save} = 1 ambiguous.
    assert_eq!(cov.sites_total, 8, "8 call sites across the fixture");
    assert_eq!(
        cov.sites_resolved, 7,
        "alias/namespace/re-export/local/unicode"
    );
    assert_eq!(cov.sites_ambiguous, 1, "the any-receiver obj.save() call");
    assert_eq!(
        cov.sites_heuristic, 0,
        "no non-ambiguous heuristic fallback here"
    );
    // The buckets account for every site here (no unresolvable-without-edge site).
    assert_eq!(
        cov.sites_resolved + cov.sites_heuristic + cov.sites_ambiguous,
        cov.sites_total
    );
}

// ── Gated live integration test ──────────────────────────────────────────────

/// Recursively copy `src` into `dst`, skipping `node_modules`, the committed
/// `index.scip`, and the lockfile (the live run produces its own).
fn copy_project(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).expect("create dst");
    for entry in fs::read_dir(src).expect("read_dir") {
        let entry = entry.expect("dir entry");
        let name = entry.file_name();
        if name == "node_modules" || name == "index.scip" || name == "package-lock.json" {
            continue;
        }
        let from = entry.path();
        let to = dst.join(&name);
        if from.is_dir() {
            copy_project(&from, &to);
        } else {
            fs::copy(&from, &to).expect("copy file");
        }
    }
}

// Enable with `STRATA_SCIP_LIVE=1 cargo test -p strata-index`.
#[test]
fn live_index_repo_auto_produces_resolved_edges() {
    if std::env::var("STRATA_SCIP_LIVE").as_deref() != Ok("1") {
        eprintln!("skipping live index_repo test (set STRATA_SCIP_LIVE=1 to run)");
        return;
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    let project = tmp.path().join("resolve");
    copy_project(&fixture_dir(), &project);

    let mut store = DuckGraphStore::open_in_memory().unwrap();
    let opts = IndexOptions {
        resolve_mode: ResolveMode::Auto,
        allow_install: true, // permit the npm install + scip-typescript run
        include_vendored: false,
    };
    let stats = index_repo_with_options(&project, &mut store, &opts)
        .expect("live auto index_repo succeeds");

    assert_eq!(stats.resolution_mode, ResolveMode::Auto);
    assert!(
        !stats.degraded,
        "scip-typescript should have run, not degraded"
    );
    assert_eq!(
        stats.sites_resolved, 7,
        "same coverage as the hermetic fixture"
    );
    assert_eq!(stats.sites_ambiguous, 1);

    // The same headline RESOLVED edges as the hermetic accuracy tests.
    let g = store.load_graph().unwrap();
    let repo = project.file_name().unwrap().to_str().unwrap();
    let run = Uid::new("ts", repo, "src/a.ts", "run", "");
    let foo = Uid::new("ts", repo, "src/b.ts", "foo", "");
    let ns_fn = Uid::new("ts", repo, "src/ns.ts", "nsFn", "");

    for (target, label) in [(&foo, "foo (alias+reexport)"), (&ns_fn, "nsFn (namespace)")] {
        let prov = g
            .neighbors(&run, Direction::Outgoing, &[EdgeKind::Calls])
            .into_iter()
            .find(|(e, _)| &e.dst == target)
            .map(|(e, _)| e.provenance);
        assert_eq!(
            prov,
            Some(Provenance::Resolved),
            "run must RESOLVED-CALL {label} in the live index"
        );
    }
}

// ── Gated live test: incremental == full in RESOLVED mode (§7, DoD #6) ───────
//
// Proves the SCIP cache + merge are incrementally correct: an incremental
// re-index (same store, parse cache reused for unchanged files, SCIP re-run for
// the changed source set) yields a byte-identical graph — nodes AND edges incl.
// provenance + confidence — to a fresh full resolved index of the same repo.
// And the resolved edges are actually present (so the test is comparing real
// precise resolution, not two degraded heuristic graphs).

/// The `methods` accuracy fixture (richer call sites than the simple repo).
fn methods_fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("accuracy")
        .join("methods")
}

// Enable with `STRATA_SCIP_LIVE=1 cargo test -p strata-index`.
#[test]
fn live_resolved_incremental_equals_full() {
    if std::env::var("STRATA_SCIP_LIVE").as_deref() != Ok("1") {
        eprintln!("skipping live resolved incremental==full test (set STRATA_SCIP_LIVE=1 to run)");
        return;
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    let project = tmp.path().join("methods");
    copy_project(&methods_fixture_dir(), &project);

    let opts = || IndexOptions {
        resolve_mode: ResolveMode::On, // REQUIRE precise resolution (no silent degrade)
        allow_install: true,
        include_vendored: false,
    };

    // ── G1: first full resolved index into the incremental store ──
    let mut store_inc = DuckGraphStore::open_in_memory().unwrap();
    let stats1 = index_repo_with_options(&project, &mut store_inc, &opts())
        .expect("first resolved index succeeds");
    assert!(!stats1.degraded, "On mode must not degrade");
    assert!(
        stats1.sites_resolved >= 1,
        "the first resolved index must have RESOLVED sites"
    );

    // ── Mutate one file on disk: add a new typed-receiver call in callers.ts ──
    let callers = project.join("src/callers.ts");
    let mut text = fs::read_to_string(&callers).expect("read callers.ts");
    text.push_str("\nexport function drawAgain() {\n  const c = new Circle();\n  c.render();\n}\n");
    fs::write(&callers, &text).expect("write mutated callers.ts");

    // ── G2: incremental re-index into the SAME store (SCIP re-runs) ──
    let stats2 = index_repo_with_options(&project, &mut store_inc, &opts())
        .expect("incremental resolved re-index succeeds");
    assert!(!stats2.degraded, "On mode must not degrade on re-index");
    assert!(
        stats2.files_reused >= 1,
        "unchanged files must be reused from the parse cache, got reused={}",
        stats2.files_reused
    );
    assert!(
        stats2.files_parsed >= 1,
        "the mutated file must be re-parsed, got parsed={}",
        stats2.files_parsed
    );
    let g_inc = store_inc.load_graph().unwrap();

    // ── G_full: fresh full resolved index of the mutated repo ──
    let mut store_full = DuckGraphStore::open_in_memory().unwrap();
    let stats_full = index_repo_with_options(&project, &mut store_full, &opts())
        .expect("full resolved index of mutated repo succeeds");
    assert!(!stats_full.degraded);
    let g_full = store_full.load_graph().unwrap();

    // ── G2 == G_full (order-independent nodes + edges incl. prov + confidence) ──
    assert_eq!(
        node_keys(&g_inc),
        node_keys(&g_full),
        "resolved incremental node set must equal full rebuild"
    );
    assert_eq!(
        edge_keys(&g_inc),
        edge_keys(&g_full),
        "resolved incremental edge set (incl. provenance + confidence) must equal full rebuild"
    );

    // ── And resolved edges are present in both (real precise resolution) ──
    assert!(
        edge_keys(&g_inc).iter().any(|e| e.contains("Resolved")),
        "the resolved graph must contain RESOLVED edges"
    );
    assert!(
        stats_full.sites_resolved >= stats1.sites_resolved,
        "the mutated repo adds a resolvable call, so resolved-site count must not drop"
    );
}

// ── graph comparison helpers (order-independent, incl. prov + confidence) ────

/// Canonical per-node string incl. provenance + 4-dp confidence.
fn node_keys(g: &strata_core::Graph) -> std::collections::BTreeSet<String> {
    g.nodes()
        .map(|n| {
            format!(
                "{}|{:?}|{}|{:?}|{:.4}",
                n.uid,
                n.kind,
                n.path,
                n.provenance,
                n.confidence.value()
            )
        })
        .collect()
}

/// Canonical per-edge string incl. provenance + 4-dp confidence.
fn edge_keys(g: &strata_core::Graph) -> std::collections::BTreeSet<String> {
    let uids: Vec<Uid> = g.nodes().map(|n| n.uid.clone()).collect();
    let mut set = std::collections::BTreeSet::new();
    for uid in &uids {
        for (e, _) in g.neighbors(uid, Direction::Outgoing, &[]) {
            set.insert(format!(
                "{}->{}|{:?}|{:?}|{:.4}",
                e.src,
                e.dst,
                e.kind,
                e.provenance,
                e.confidence.value()
            ));
        }
    }
    set
}

// ── helpers ──────────────────────────────────────────────────────────────────

fn load_fixture_sources() -> BTreeMap<String, String> {
    let dir = fixture_dir().join("src");
    let mut map = BTreeMap::new();
    for entry in fs::read_dir(&dir).expect("read fixture src") {
        let p = entry.expect("entry").path();
        if p.extension().and_then(|e| e.to_str()) == Some("ts") {
            let name = p.file_name().unwrap().to_str().unwrap();
            map.insert(
                format!("src/{name}"),
                fs::read_to_string(&p).expect("read source"),
            );
        }
    }
    map
}
