//! Estate link-pass tests (Slice 3, M3 — Definition of Done tests 3, 5, 6, 9,
//! 10). The headline cross-repo *impact* test (4) lives in
//! `tests/crossrepo_impact.rs`; the coverage report/CI-floor tests (7, 8) live
//! in `tests/openapi_linking.rs`.
//!
//! Every test copies a committed fixture estate to a tempdir, indexes each repo
//! (`index_estate`, `ResolveMode::Off` — no Node/SCIP needed), then calls
//! `link_estate`. `.strata/` dirs are only ever created inside the tempdir.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use strata_core::{Direction, EdgeKind, NodeKind, Provenance, Uid};
use strata_index::{index_estate, link_estate, ResolveMode, WorkspaceManifest};

// ── Fixture helpers ─────────────────────────────────────────────────────────

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
        let name = entry.file_name();
        if name == ".strata" {
            continue;
        }
        let src_path = entry.path();
        let dst_path = dst.join(&name);
        if entry.file_type()?.is_dir() {
            copy_dir_all(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

/// Copy `fixtures/<name>` to a tempdir, index it, and return (tempdir, manifest).
fn prep_estate(name: &str) -> (tempfile::TempDir, WorkspaceManifest) {
    let tmp = tempfile::tempdir().expect("tempdir");
    copy_dir_all(&fixture_dir(name), tmp.path()).expect("copy fixture");
    let manifest_path = tmp.path().join("strata.workspace.toml");
    let manifest = WorkspaceManifest::parse_file(&manifest_path).expect("manifest parses");
    index_estate(&manifest, &manifest_path, ResolveMode::Off);
    (tmp, manifest)
}

/// The canonical (estate-wide) `ApiOperation` UID for `key` in estate `name`,
/// owned by api `api_id`. The api-scoped identity (B6 fix) renders the
/// discriminator as `{api_id}/openapi`; the default `api_id` is the repo name of
/// the repo whose spec declares the operation.
fn canonical_op_uid(estate: &str, api_id: &str, key: &str) -> Uid {
    Uid::new("contract", estate, &format!("{api_id}/openapi"), key, "")
}

/// The code-plane function-symbol UID for `fqn` in `repo`/`path`.
fn fn_uid(repo: &str, path: &str, fqn: &str) -> Uid {
    Uid::new("ts", repo, path, fqn, "")
}

fn edges_of(g: &strata_core::Graph, src: &Uid, kind: EdgeKind) -> Vec<(Uid, Provenance, f32)> {
    g.neighbors(src, Direction::Outgoing, &[kind])
        .into_iter()
        .map(|(e, _)| (e.dst.clone(), e.provenance, e.confidence.value()))
        .collect()
}

/// Count `ApiOperation` nodes whose `fqn` (cross-repo key) equals `key`.
fn count_ops_with_key(g: &strata_core::Graph, key: &str) -> usize {
    g.nodes()
        .filter(|n| n.kind == NodeKind::ApiOperation && n.fqn == key)
        .count()
}

// ── Test 3: cross-repo linking — one canonical op, PRODUCES + CONSUMES ───────

#[test]
fn link_estate_links_producer_and_consumer_across_repos() {
    let (tmp, manifest) = prep_estate("crossrepo");
    let (g, _coverage, results) = link_estate(&manifest, tmp.path());

    assert!(
        results.iter().all(|r| r.ok),
        "both repos must load ok: {results:?}"
    );

    // Exactly ONE canonical getUser ApiOperation node, under the estate name.
    // The spec lives in repo-producer, which declares no api id → api_id defaults
    // to the repo name `repo-producer`.
    let canonical = canonical_op_uid("shop-estate", "repo-producer", "getUser");
    let node = g
        .get_node(&canonical)
        .unwrap_or_else(|| panic!("canonical getUser node missing"));
    assert_eq!(node.kind, NodeKind::ApiOperation);
    assert_eq!(node.name, "getUser");
    assert_eq!(
        count_ops_with_key(&g, "getUser"),
        1,
        "getUser must collapse to ONE canonical operation node"
    );

    // PRODUCES from repo-producer's getUser handler → the canonical operation.
    let producer_fn = fn_uid("repo-producer", "src/handlers.ts", "getUser");
    let produces = edges_of(&g, &producer_fn, EdgeKind::Produces);
    assert!(
        produces
            .iter()
            .any(|(dst, prov, _)| *dst == canonical && *prov == Provenance::Inferred),
        "producer handler must PRODUCES the canonical getUser (Inferred): {produces:?}"
    );

    // CONSUMES from repo-consumer's literal-URL call → the canonical operation.
    let literal_consumer = fn_uid("repo-consumer", "src/api.ts", "fetchUserProfile");
    let c_lit = edges_of(&g, &literal_consumer, EdgeKind::Consumes);
    assert!(
        c_lit.iter().any(|(dst, prov, conf)| *dst == canonical
            && *prov == Provenance::Inferred
            && *conf == 0.70),
        "literal-URL consumer must CONSUMES canonical getUser at 0.70: {c_lit:?}"
    );

    // CONSUMES from repo-consumer's operationId-name call → the canonical op.
    let name_consumer = fn_uid("repo-consumer", "src/api.ts", "getUserViaClient");
    let c_name = edges_of(&g, &name_consumer, EdgeKind::Consumes);
    assert!(
        c_name.iter().any(|(dst, prov, conf)| *dst == canonical
            && *prov == Provenance::Inferred
            && *conf == 0.75),
        "operationId-name consumer must CONSUMES canonical getUser at 0.75: {c_name:?}"
    );
}

// ── Test 5: no false cross-repo link ─────────────────────────────────────────

#[test]
fn link_estate_creates_no_edge_for_undeclared_endpoint() {
    let (tmp, manifest) = prep_estate("crossrepo");
    let (g, _coverage, _results) = link_estate(&manifest, tmp.path());

    // repo-consumer's fetchWidget calls /widgets/9 — no operation declares it.
    let widget_consumer = fn_uid("repo-consumer", "src/api.ts", "fetchWidget");
    let edges = edges_of(&g, &widget_consumer, EdgeKind::Consumes);
    assert!(
        edges.is_empty(),
        "a consumer of an undeclared endpoint must create NO CONSUMES edge, got {edges:?}"
    );

    // And no operation node was invented for /widgets/9 anywhere. (Even if one
    // were, it would be in repo-consumer's namespace — which has no spec at all.)
    let invented = canonical_op_uid("shop-estate", "repo-producer", "GET /widgets/{}");
    assert!(g.get_node(&invented).is_none(), "no invented widget op");
}

// ── Test 6: dedup — same operationId in two repos that DECLARE a shared api id
// collapses to one node (the opt-in MERGE, B6 fix) ───────────────────────────

#[test]
fn link_estate_dedups_same_operation_id_across_repos_when_api_declared() {
    // The `dedup` fixture's manifest now POSITIVELY declares the same api id
    // `user` in both repo-x and repo-y (the explicit opt-in). Only because of that
    // declaration does the shared `getUser` key collapse to ONE canonical node.
    let (tmp, manifest) = prep_estate("dedup");
    let (g, _coverage, results) = link_estate(&manifest, tmp.path());

    assert!(results.iter().all(|r| r.ok), "both repos load ok");

    // getUser declared in BOTH repos under the SAME api id `user` → ONE node.
    assert_eq!(
        count_ops_with_key(&g, "getUser"),
        1,
        "the same operationId in two repos that declare the same api id must \
         collapse to ONE canonical node (earned by the declaration)"
    );

    // Both producers point at that single canonical node, keyed by api id `user`.
    let canonical = canonical_op_uid("dedup-estate", "user", "getUser");
    let x_fn = fn_uid("repo-x", "src/x.ts", "getUser");
    let y_fn = fn_uid("repo-y", "src/y.ts", "getUser");
    assert!(
        edges_of(&g, &x_fn, EdgeKind::Produces)
            .iter()
            .any(|(d, _, _)| *d == canonical),
        "repo-x handler produces the canonical node"
    );
    assert!(
        edges_of(&g, &y_fn, EdgeKind::Produces)
            .iter()
            .any(|(d, _, _)| *d == canonical),
        "repo-y handler produces the SAME canonical node"
    );
}

// ── Test 6 (sibling): WITHOUT the api declaration, the same key namespaces apart
// into two nodes — repos never merge a shared key by default (B6 safe default) ─

#[test]
fn link_estate_does_not_merge_same_operation_id_without_api_declaration() {
    // The `dedup_undeclared` fixture is byte-identical to `dedup` but its v1
    // manifest declares NO api id. The default api_id is the repo name, so the two
    // `getUser` operations namespace apart — TWO canonical nodes, never a merge.
    let (tmp, manifest) = prep_estate("dedup_undeclared");
    let (g, _coverage, results) = link_estate(&manifest, tmp.path());
    assert!(results.iter().all(|r| r.ok), "both repos load ok");

    assert_eq!(
        count_ops_with_key(&g, "getUser"),
        2,
        "without a declared shared api id, the same key in two repos must stay as \
         TWO distinct canonical nodes (the safe default)"
    );

    // Each producer points at its OWN repo-scoped canonical node.
    let x_op = canonical_op_uid("dedup-undeclared-estate", "repo-x", "getUser");
    let y_op = canonical_op_uid("dedup-undeclared-estate", "repo-y", "getUser");
    assert_ne!(x_op, y_op, "the two api nodes must be distinct");
    let x_fn = fn_uid("repo-x", "src/x.ts", "getUser");
    let y_fn = fn_uid("repo-y", "src/y.ts", "getUser");
    assert!(
        edges_of(&g, &x_fn, EdgeKind::Produces)
            .iter()
            .any(|(d, _, _)| *d == x_op),
        "repo-x handler produces repo-x's own canonical node"
    );
    assert!(
        edges_of(&g, &y_fn, EdgeKind::Produces)
            .iter()
            .any(|(d, _, _)| *d == y_op),
        "repo-y handler produces repo-y's own (distinct) canonical node"
    );
}

// ── Test 9: determinism — link_estate twice → identical estate graph ─────────

#[test]
fn link_estate_is_deterministic() {
    let (tmp, manifest) = prep_estate("crossrepo");

    let (g1, cov1, _) = link_estate(&manifest, tmp.path());
    let (g2, cov2, _) = link_estate(&manifest, tmp.path());

    assert_eq!(g1.node_count(), g2.node_count(), "node counts match");
    assert_eq!(g1.edge_count(), g2.edge_count(), "edge counts match");
    assert_eq!(cov1, cov2, "coverage is identical across runs");

    let nodes1: BTreeSet<String> = g1.nodes().map(|n| n.uid.to_string()).collect();
    let nodes2: BTreeSet<String> = g2.nodes().map(|n| n.uid.to_string()).collect();
    assert_eq!(nodes1, nodes2, "node UID sets match");

    // Edge multiset (src|dst|kind) must match exactly.
    let edges = |g: &strata_core::Graph| -> BTreeSet<String> {
        let mut s = BTreeSet::new();
        for n in g.nodes() {
            for (e, _) in g.neighbors(&n.uid, Direction::Outgoing, &[]) {
                s.insert(format!(
                    "{}|{}|{:?}",
                    e.src.as_str(),
                    e.dst.as_str(),
                    e.kind
                ));
            }
        }
        s
    };
    assert_eq!(edges(&g1), edges(&g2), "edge sets match across runs");
}

// ── Test 10: R2 — a broken spec still contributes code + valid links ─────────

#[test]
fn link_estate_degrades_gracefully_on_broken_spec() {
    // repo-producer has a VALID openapi.yaml (getUser) AND a malformed broken.yaml
    // (getOrder). repo-consumer has no spec and calls getUser. link_estate must
    // NOT panic; the valid getUser op + its cross-repo consumer link survive; the
    // malformed spec's getOrder is never extracted.
    let (tmp, manifest) = prep_estate("crossrepo_r2");
    let (g, _coverage, results) = link_estate(&manifest, tmp.path());

    assert!(
        results.iter().all(|r| r.ok),
        "both repos load ok: {results:?}"
    );

    // The valid operation is present and canonical (spec in repo-producer → the
    // default api_id is `repo-producer`).
    let canonical = canonical_op_uid("r2-estate", "repo-producer", "getUser");
    assert!(
        g.get_node(&canonical).is_some(),
        "the valid getUser op must survive the broken sibling spec"
    );

    // The broken spec's getOrder was never extracted (no node by that key).
    assert_eq!(
        count_ops_with_key(&g, "getOrder"),
        0,
        "the malformed spec must be skipped, not partially extracted"
    );

    // The cross-repo consumer link still formed despite the broken spec.
    let consumer = fn_uid("repo-consumer", "src/api.ts", "loadUser");
    let edges = edges_of(&g, &consumer, EdgeKind::Consumes);
    assert!(
        edges.iter().any(|(d, _, _)| *d == canonical),
        "valid consumer link must be unaffected by the broken spec: {edges:?}"
    );

    // The producer code plane is intact: the handler node exists.
    let handler = fn_uid("repo-producer", "src/handlers.ts", "getUser");
    assert!(
        g.get_node(&handler).is_some(),
        "producer code plane must be intact"
    );
}
