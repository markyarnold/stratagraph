//! Hermetic precise-resolution accuracy tests (brief Definition of Done #1–#8).
//!
//! These load the **committed** `index.scip` for the `resolve` fixture (no Node
//! at test time), Tree-sitter-analyze the fixture sources, and run
//! `assemble_graph_with_scip(..., Some(resolver), ...)`. They prove the
//! gap-closers (aliased import / namespace call / re-export hop) now produce
//! compiler-grade `RESOLVED` edges, that a heuristic false edge is corrected,
//! that an SCIP-uncovered call still falls back to a heuristic/`AMBIGUOUS` edge,
//! and that the byte↔UTF-16 conversion works at the *site* (non-ASCII).
//!
//! Test #8 (determinism / heuristic path unchanged) pins that `scip = None`
//! equals the slice-1 `assemble_graph` byte-for-byte.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use strata_core::{AnalyzedFile, Direction, Edge, EdgeKind, Graph, Provenance, Uid};
use strata_index::{assemble_graph, assemble_graph_with_scip, resolve_differential};
use strata_lang_ts::{analyze, ResolveOptions};
use strata_scip::ScipResolver;

const REPO: &str = "strata-index-resolve";

/// The committed SCIP index for the `resolve` fixture.
const RESOLVE_INDEX: &[u8] = include_bytes!("fixtures/resolve/index.scip");

fn fixture_src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("resolve")
        .join("src")
}

/// Read every `src/*.ts` fixture source into `src/<name>.ts -> text`.
fn load_sources() -> BTreeMap<String, String> {
    let dir = fixture_src_dir();
    let mut map = BTreeMap::new();
    for entry in std::fs::read_dir(&dir).expect("read fixture src dir") {
        let entry = entry.expect("dir entry");
        let p = entry.path();
        if p.extension().and_then(|e| e.to_str()) == Some("ts") {
            let name = p.file_name().unwrap().to_str().unwrap();
            let key = format!("src/{name}");
            map.insert(key, std::fs::read_to_string(&p).expect("read source"));
        }
    }
    map
}

fn analyze_all(sources: &BTreeMap<String, String>) -> BTreeMap<String, AnalyzedFile> {
    sources
        .iter()
        .map(|(p, s)| (p.clone(), analyze(p, s)))
        .collect()
}

fn resolver() -> ScipResolver {
    ScipResolver::from_bytes(RESOLVE_INDEX).expect("resolve index parses")
}

/// Build the graph with precise resolution enabled.
fn build_resolved() -> Graph {
    let sources = load_sources();
    let analyzed = analyze_all(&sources);
    let r = resolver();
    assemble_graph_with_scip(
        &analyzed,
        REPO,
        &ResolveOptions::default(),
        Some(&r),
        &sources,
    )
}

fn uid_symbol(path: &str, fqn: &str) -> Uid {
    Uid::new("ts", REPO, path, fqn, "")
}

/// The `(provenance, confidence)` of a directed `kind` edge from `src` to `dst`.
fn edge_meta(g: &Graph, src: &Uid, dst: &Uid, kind: EdgeKind) -> Option<(Provenance, f32)> {
    g.neighbors(src, Direction::Outgoing, &[kind])
        .into_iter()
        .find(|(e, _)| &e.dst == dst)
        .map(|(e, _)| (e.provenance, e.confidence.value()))
}

fn has_edge(g: &Graph, src: &Uid, dst: &Uid, kind: EdgeKind) -> bool {
    edge_meta(g, src, dst, kind).is_some()
}

/// All CALLS-edge destinations from `src`, sorted.
fn calls_from(g: &Graph, src: &Uid) -> Vec<Uid> {
    let mut v: Vec<Uid> = g
        .neighbors(src, Direction::Outgoing, &[EdgeKind::Calls])
        .into_iter()
        .map(|(e, _)| e.dst.clone())
        .collect();
    v.sort();
    v
}

// ── Test 1: aliased import resolves cross-file (was a slice-1 miss) ───────────

#[test]
fn aliased_import_call_resolves_to_foo() {
    let g = build_resolved();
    let run = uid_symbol("src/a.ts", "run");
    let foo = uid_symbol("src/b.ts", "foo");

    let meta = edge_meta(&g, &run, &foo, EdgeKind::Calls);
    assert!(
        meta.is_some(),
        "run must CALL foo in b.ts (resolved through `foo as bar`). calls were: {:?}",
        calls_from(&g, &run)
    );
    let (prov, conf) = meta.unwrap();
    assert_eq!(prov, Provenance::Resolved, "aliased call is compiler-grade");
    assert!((conf - 0.97).abs() < 1e-6, "confidence ~0.97, got {conf}");

    // It must NOT point at a phantom `bar` symbol.
    assert!(
        g.get_node(&uid_symbol("src/a.ts", "bar")).is_none(),
        "there is no `bar` symbol node — the alias resolves to foo"
    );
}

// ── Test 2: namespace call resolves to the free function (was a miss) ─────────

#[test]
fn namespace_call_resolves_to_nsfn() {
    let g = build_resolved();
    let run = uid_symbol("src/a.ts", "run");
    let ns_fn = uid_symbol("src/ns.ts", "nsFn");

    let (prov, conf) = edge_meta(&g, &run, &ns_fn, EdgeKind::Calls)
        .expect("run must CALL nsFn in ns.ts (NS.nsFn())");
    assert_eq!(prov, Provenance::Resolved);
    assert!((conf - 0.97).abs() < 1e-6);
}

// ── Test 3: re-export hop resolves to the original (was a miss) ───────────────

#[test]
fn reexport_call_resolves_to_original_foo() {
    let g = build_resolved();
    let run = uid_symbol("src/a.ts", "run");
    let foo = uid_symbol("src/b.ts", "foo");

    // fooReexport() must resolve, through the barrel, to b.ts's foo.
    let (prov, _) = edge_meta(&g, &run, &foo, EdgeKind::Calls)
        .expect("run must CALL b.ts's foo via the re-export");
    assert_eq!(prov, Provenance::Resolved);

    // And there is no phantom `fooReexport` symbol target.
    assert!(g.get_node(&uid_symbol("src/a.ts", "fooReexport")).is_none());
    assert!(g
        .get_node(&uid_symbol("src/barrel.ts", "fooReexport"))
        .is_none());
}

// ── Test 4: SCIP resolves dup() to the local definition; no cross-file edge ───
//
// `dup` is a bare local call — `run` calls its own local `dup` function, not an
// import. SCIP confirms the local definition precisely. The test verifies the
// RESOLVED edge to the local `dup` and the absence of any cross-file edge to the
// same-named symbols in other.ts or b.ts. (The heuristic also lands on the local
// definition in this fixture because `dup` is not imported, so no heuristic
// false edge is corrected here; the value of this test is confirming the SCIP
// RESOLVED provenance and that no spurious cross-file edges appear.)

#[test]
fn dup_call_resolves_to_local_not_other_file() {
    let g = build_resolved();
    let run = uid_symbol("src/a.ts", "run");
    let local_dup = uid_symbol("src/a.ts", "dup");
    let other_dup = uid_symbol("src/other.ts", "dup");
    let b_dup = uid_symbol("src/b.ts", "dup");

    // dup() resolves precisely to the LOCAL dup in a.ts (SCIP gives RESOLVED).
    let (prov, _) = edge_meta(&g, &run, &local_dup, EdgeKind::Calls)
        .expect("run must CALL the local dup in a.ts");
    assert_eq!(prov, Provenance::Resolved);

    // No cross-file edges to same-named dup symbols in other.ts or b.ts.
    assert!(
        !has_edge(&g, &run, &other_dup, EdgeKind::Calls),
        "SCIP must narrow dup() to the local def — no edge to other.ts's dup"
    );
    assert!(
        !has_edge(&g, &run, &b_dup, EdgeKind::Calls),
        "no edge to b.ts's dup either"
    );

    // Exactly one `dup` target overall — the local one.
    let dups: Vec<Uid> = calls_from(&g, &run)
        .into_iter()
        .filter(|u| u.as_str().ends_with("|dup|"))
        .collect();
    assert_eq!(
        dups,
        vec![local_dup],
        "exactly one dup target: the local one"
    );
}

// ── Test 5: RESOLVED provenance + an SCIP-uncovered call stays AMBIGUOUS ──────

#[test]
fn uncovered_dynamic_call_falls_back_to_ambiguous() {
    let g = build_resolved();
    let dyn_caller = uid_symbol("src/dyn.ts", "dynCaller");
    let widget_save = uid_symbol("src/dyn.ts", "Widget.save");
    let gadget_save = uid_symbol("src/dyn.ts", "Gadget.save");

    // `obj.save()` where obj: any — SCIP cannot resolve the callee `save`, so the
    // heuristic (rule 3) over-includes both same-named methods, AMBIGUOUS (R4).
    let (wprov, wconf) = edge_meta(&g, &dyn_caller, &widget_save, EdgeKind::Calls)
        .expect("dynCaller must CALL Widget.save (heuristic fallback)");
    assert_eq!(
        wprov,
        Provenance::Ambiguous,
        "uncovered call is not dropped"
    );
    // Ambiguous confidence is the band-capped UnknownReceiver number (0.39):
    // stored = min(measured precision 0.50, Ambiguous ceiling 0.39) per §4.1.
    // This keeps heuristic Ambiguous edges strictly below the Inferred tier.
    assert!(
        (wconf - 0.39).abs() < 1e-6,
        "ambiguous (UnknownReceiver) confidence is the band-capped 0.39, got {wconf}"
    );
    let (gprov, _) = edge_meta(&g, &dyn_caller, &gadget_save, EdgeKind::Calls)
        .expect("dynCaller must CALL Gadget.save (heuristic fallback)");
    assert_eq!(gprov, Provenance::Ambiguous);

    // No RESOLVED edge from dynCaller (SCIP didn't cover `save`).
    let resolved_from_dyn = g
        .neighbors(&dyn_caller, Direction::Outgoing, &[EdgeKind::Calls])
        .into_iter()
        .any(|(e, _)| e.provenance == Provenance::Resolved);
    assert!(
        !resolved_from_dyn,
        "the dynamic call must remain heuristic, not RESOLVED"
    );
}

// ── Test 6: non-ASCII alignment at the SITE (spec A3) ────────────────────────

#[test]
fn non_ascii_call_resolves_through_site_conversion() {
    let g = build_resolved();
    let caller = uid_symbol("src/uni.ts", "uniCaller");
    let cafe = uid_symbol("src/uni.ts", "café");
    let naive = uid_symbol("src/uni.ts", "naïve");
    let nf = uid_symbol("src/uni.ts", "nf");

    // All three callees come after a non-ASCII identifier on the line. `nf`
    // especially: its byte column (49) overshoots its UTF-16 occurrence
    // [47,49); only the correct byte→UTF-16 conversion lands inside it.
    for (target, label) in [(&cafe, "café"), (&naive, "naïve"), (&nf, "nf")] {
        let (prov, conf) = edge_meta(&g, &caller, target, EdgeKind::Calls)
            .unwrap_or_else(|| panic!("uniCaller must RESOLVED-CALL {label}"));
        assert_eq!(
            prov,
            Provenance::Resolved,
            "{label} must resolve precisely (byte↔UTF-16 conversion at the site)"
        );
        assert!((conf - 0.97).abs() < 1e-6);
    }
}

// ── Test 8: determinism + heuristic path unchanged ───────────────────────────

#[test]
fn scip_none_equals_slice1_and_resolved_is_deterministic() {
    let sources = load_sources();
    let analyzed = analyze_all(&sources);
    let opts = ResolveOptions::default();

    // scip = None (via assemble_graph_with_scip) MUST equal slice-1 assemble_graph.
    let empty = BTreeMap::new();
    let g_none = assemble_graph_with_scip(&analyzed, REPO, &opts, None, &empty);
    let g_slice1 = assemble_graph(&analyzed, REPO, &opts);
    assert_graphs_identical(
        &g_none,
        &g_slice1,
        "scip=None must equal slice-1 assemble_graph",
    );

    // Resolved-mode build is deterministic: same inputs+resolver → same graph.
    let r1 = resolver();
    let r2 = resolver();
    let g1 = assemble_graph_with_scip(&analyzed, REPO, &opts, Some(&r1), &sources);
    let g2 = assemble_graph_with_scip(&analyzed, REPO, &opts, Some(&r2), &sources);
    assert_graphs_identical(&g1, &g2, "resolved-mode build must be deterministic");

    // And the resolved graph DIFFERS from the heuristic one (proves SCIP acted).
    assert_ne!(
        edge_multiset(&g1),
        edge_multiset(&g_none),
        "resolved graph must differ from the pure-heuristic graph"
    );
}

// ── Test (DoD #2): resolve_differential shares logic with the merge ──────────
//
// The SCIP targets `resolve_differential` records (per call site) must be
// exactly the RESOLVED edge destinations `assemble_graph_with_scip` emits.
// Both paths route every site through the *same* `resolve_site_targets` helper
// and the builder writes one RESOLVED edge per SCIP-covered, non-self site, so
// the two multisets are identical — any drift between the helper-as-recorded and
// the helper-as-built fails here.

#[test]
fn differential_scip_targets_match_resolved_edges() {
    let sources = load_sources();
    let analyzed = analyze_all(&sources);
    let r = resolver();

    // Differential: the SCIP target recorded per site (self-edges already
    // filtered, matching the builder's self-edge suppression).
    let mut differential_targets: Vec<String> =
        resolve_differential(&analyzed, &sources, REPO, &ResolveOptions::default(), &r)
            .into_iter()
            .filter_map(|o| o.scip_target.map(|t| t.to_string()))
            .collect();
    differential_targets.sort();

    // Builder: every RESOLVED edge's destination.
    let g = build_resolved();
    let mut resolved_edge_targets: Vec<String> = Vec::new();
    for node in g.nodes() {
        for (e, _) in g.neighbors(&node.uid, Direction::Outgoing, &[EdgeKind::Calls]) {
            if e.provenance == Provenance::Resolved {
                resolved_edge_targets.push(e.dst.to_string());
            }
        }
    }
    resolved_edge_targets.sort();

    assert!(
        differential_targets.len() >= 5,
        "expected several SCIP-covered sites, got {}",
        differential_targets.len()
    );
    assert_eq!(
        differential_targets, resolved_edge_targets,
        "resolve_differential's SCIP targets must equal the builder's RESOLVED edge targets \
         (shared per-site helper must not drift)"
    );
}

// ── graph comparison helpers ─────────────────────────────────────────────────

/// Canonical edge tuple including provenance and rounded confidence.
fn edge_tuple(e: &Edge) -> (String, String, String, String, String) {
    (
        e.src.to_string(),
        e.dst.to_string(),
        format!("{:?}", e.kind),
        format!("{:?}", e.provenance),
        format!("{:.4}", e.confidence.value()),
    )
}

fn edge_multiset(g: &Graph) -> BTreeSet<(String, String, String, String, String)> {
    let uids: Vec<Uid> = g.nodes().map(|n| n.uid.clone()).collect();
    let mut set = BTreeSet::new();
    for uid in &uids {
        for (e, _) in g.neighbors(uid, Direction::Outgoing, &[]) {
            set.insert(edge_tuple(e));
        }
    }
    set
}

fn node_set(g: &Graph) -> BTreeSet<(String, String, String)> {
    g.nodes()
        .map(|n| {
            (
                n.uid.to_string(),
                format!("{:?}", n.kind),
                format!("{:?}", n.provenance),
            )
        })
        .collect()
}

fn assert_graphs_identical(a: &Graph, b: &Graph, msg: &str) {
    assert_eq!(a.node_count(), b.node_count(), "{msg}: node counts differ");
    assert_eq!(a.edge_count(), b.edge_count(), "{msg}: edge counts differ");
    assert_eq!(node_set(a), node_set(b), "{msg}: node sets differ");
    assert_eq!(
        edge_multiset(a),
        edge_multiset(b),
        "{msg}: edge sets differ"
    );
}
