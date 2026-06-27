//! THE B6 collision regression (Slice 8 — api-scoped canonical identity).
//!
//! Audit 2026-06-11 §14.2 B6 (CONFIRMED Critical): estate dedup by `(format,
//! key)` falsely merges two UNRELATED APIs that share an operation key. Proven in
//! the field: a user-service field's impact confidently included a billing-only
//! frontend at 0.76, unflagged — the one failure this product must never produce.
//!
//! The fix makes canonical contract identity `(api_id, format, key)`, where the
//! default `api_id` is the repo name. Two repos NEVER merge a shared key unless
//! they positively declare the same api id (see `dedup_graphql` for that opt-in).
//!
//! This suite is the red-first regression. Against the pre-fix `(format, key)`
//! dedup it FAILS: the two `Query.getUser` schemas (and the two `GET /health`
//! specs) collapse to ONE node each, and the shared consumer links once,
//! confidently. Post-fix it asserts: TWO canonical nodes per collision; impact
//! from one api's producer never reaches the OTHER api's operation node; and a
//! consumer matching both apis fans out to N `Ambiguous` 0.35 edges
//! (`ambiguous: true`) — never a silent confident pick.

use std::path::{Path, PathBuf};

use strata_core::{impact, Direction, EdgeKind, ImpactOptions, NodeKind, Provenance, Uid};
use strata_index::{index_estate, link_estate, ResolveMode, WorkspaceManifest};

const ESTATE: &str = "collision-estate";

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

fn prep() -> (tempfile::TempDir, WorkspaceManifest) {
    let tmp = tempfile::tempdir().expect("tempdir");
    copy_dir_all(&fixture_dir("estate_api_collision"), tmp.path()).expect("copy fixture");
    let manifest_path = tmp.path().join("strata.workspace.toml");
    let manifest = WorkspaceManifest::parse_file(&manifest_path).expect("manifest parses");
    index_estate(&manifest, &manifest_path, ResolveMode::Off);
    (tmp, manifest)
}

/// The canonical (estate-wide) operation UID under the api-scoped identity:
/// `contract | estate | {api_id}/{format} | key |`.
fn canonical_uid(api_id: &str, format: &str, key: &str) -> Uid {
    Uid::new("contract", ESTATE, &format!("{api_id}/{format}"), key, "")
}

fn fn_uid(repo: &str, path: &str, fqn: &str) -> Uid {
    Uid::new("ts", repo, path, fqn, "")
}

fn count_nodes_with_fqn(g: &strata_core::Graph, kind: NodeKind, fqn: &str) -> usize {
    g.nodes().filter(|n| n.kind == kind && n.fqn == fqn).count()
}

fn consumes_of(g: &strata_core::Graph, src: &Uid) -> Vec<(Uid, Provenance, f32)> {
    g.neighbors(src, Direction::Outgoing, &[EdgeKind::Consumes])
        .into_iter()
        .map(|(e, _)| (e.dst.clone(), e.provenance, e.confidence.value()))
        .collect()
}

// ── GraphQL collision: two unrelated `Query.getUser` schemas ─────────────────

#[test]
fn graphql_unrelated_same_key_apis_do_not_merge() {
    let (tmp, manifest) = prep();
    let (g, _cov, results) = link_estate(&manifest, tmp.path());
    assert!(
        results.iter().all(|r| r.ok),
        "all repos load ok: {results:?}"
    );

    // TWO canonical GraphqlField nodes for Query.getUser — one per api (repo).
    assert_eq!(
        count_nodes_with_fqn(&g, NodeKind::GraphqlField, "Query.getUser"),
        2,
        "two UNRELATED APIs declaring Query.getUser must yield TWO canonical nodes \
         (api-scoped identity), not one merged node"
    );

    let user_op = canonical_uid("repo-user", "graphql", "Query.getUser");
    let billing_op = canonical_uid("repo-billing", "graphql", "Query.getUser");
    assert!(
        g.get_node(&user_op).is_some(),
        "the user service's canonical Query.getUser node must exist: {user_op:?}"
    );
    assert!(
        g.get_node(&billing_op).is_some(),
        "the billing service's canonical Query.getUser node must exist: {billing_op:?}"
    );
    assert_ne!(user_op, billing_op, "the two api nodes must be distinct");
}

#[test]
fn graphql_impact_never_crosses_unrelated_apis() {
    let (tmp, manifest) = prep();
    let (g, _cov, _results) = link_estate(&manifest, tmp.path());

    let user_resolver = fn_uid("repo-user", "src/resolvers.ts", "getUser");
    let user_op = canonical_uid("repo-user", "graphql", "Query.getUser");
    let billing_op = canonical_uid("repo-billing", "graphql", "Query.getUser");

    let r = impact(&g, &user_resolver, &ImpactOptions::default());

    // The user resolver reaches ITS OWN operation node…
    assert!(
        r.affected.iter().any(|a| a.uid == user_op),
        "impact(user resolver) must reach the user service's own Query.getUser op"
    );
    // …and NEVER the billing service's unrelated operation node. Pre-fix the two
    // collapsed to one node, so this reach happened — the false merge.
    assert!(
        !r.affected.iter().any(|a| a.uid == billing_op),
        "impact(user resolver) must NOT reach the billing service's unrelated \
         Query.getUser op (no false cross-api merge)"
    );
    // The billing PRODUCER node itself must never be in a user-side blast radius.
    let billing_resolver = fn_uid("repo-billing", "src/resolvers.ts", "getUser");
    assert!(
        !r.affected.iter().any(|a| a.uid == billing_resolver),
        "impact(user resolver) must NOT reach the billing resolver"
    );
}

#[test]
fn graphql_shared_consumer_fans_out_ambiguously() {
    let (tmp, manifest) = prep();
    let (g, _cov, _results) = link_estate(&manifest, tmp.path());

    // repo-frontend's gql consumer matches Query.getUser, owned by BOTH apis →
    // exactly TWO Ambiguous 0.35 edges (one per api node), never one confident
    // Extracted 0.95 pick.
    let consumer = fn_uid("repo-frontend", "src/queries.ts", "loadUser");
    let user_op = canonical_uid("repo-user", "graphql", "Query.getUser");
    let billing_op = canonical_uid("repo-billing", "graphql", "Query.getUser");

    let edges = consumes_of(&g, &consumer);
    let fanned: Vec<&(Uid, Provenance, f32)> = edges
        .iter()
        .filter(|(dst, _, _)| *dst == user_op || *dst == billing_op)
        .collect();
    assert_eq!(
        fanned.len(),
        2,
        "the shared consumer must fan out to BOTH api nodes (2 edges), got: {edges:?}"
    );
    for (_dst, prov, conf) in &fanned {
        assert_eq!(
            *prov,
            Provenance::Ambiguous,
            "a fan-out edge must be Ambiguous, never a confident pick"
        );
        assert!(
            *conf < 0.40 && (*conf - 0.35).abs() < 1e-6,
            "a fan-out edge must sit at the Ambiguous 0.35 tier (< 0.40), got {conf}"
        );
    }

    // The impact view of the fan-out is honestly flagged ambiguous (never the
    // confident 0.76 = 0.80 × 0.95 the false merge produced).
    let user_resolver = fn_uid("repo-user", "src/resolvers.ts", "getUser");
    let r = impact(&g, &user_resolver, &ImpactOptions::default());
    if let Some(a) = r.affected.iter().find(|a| a.uid == consumer) {
        assert!(
            a.ambiguous,
            "the cross-api consumer reach must be flagged ambiguous"
        );
        assert!(
            a.confidence < 0.40,
            "the cross-api consumer reach confidence must be < 0.40 (Ambiguous \
             band), never the confident 0.76 the false merge produced; got {}",
            a.confidence
        );
    }
}

// ── OpenAPI collision: two unrelated `GET /health` specs ─────────────────────

#[test]
fn openapi_unrelated_same_key_apis_do_not_merge() {
    let (tmp, manifest) = prep();
    let (g, _cov, results) = link_estate(&manifest, tmp.path());
    assert!(
        results.iter().all(|r| r.ok),
        "all repos load ok: {results:?}"
    );

    // TWO canonical ApiOperation nodes for the `health` key — one per service.
    assert_eq!(
        count_nodes_with_fqn(&g, NodeKind::ApiOperation, "health"),
        2,
        "two UNRELATED services declaring GET /health must yield TWO canonical \
         operation nodes, not one merged node"
    );

    let a_op = canonical_uid("repo-svc-a", "openapi", "health");
    let b_op = canonical_uid("repo-svc-b", "openapi", "health");
    assert!(g.get_node(&a_op).is_some(), "svc-a health op must exist");
    assert!(g.get_node(&b_op).is_some(), "svc-b health op must exist");
    assert_ne!(a_op, b_op, "the two health ops must be distinct nodes");
}

#[test]
fn openapi_shared_consumer_fans_out_ambiguously() {
    let (tmp, manifest) = prep();
    let (g, _cov, _results) = link_estate(&manifest, tmp.path());

    // repo-uptime's literal fetch("/health") matches GET /health, owned by BOTH
    // services → exactly TWO Ambiguous 0.35 edges, never one confident pick.
    let probe = fn_uid("repo-uptime", "src/probe.ts", "probeHealth");
    let a_op = canonical_uid("repo-svc-a", "openapi", "health");
    let b_op = canonical_uid("repo-svc-b", "openapi", "health");

    let edges = consumes_of(&g, &probe);
    let fanned: Vec<&(Uid, Provenance, f32)> = edges
        .iter()
        .filter(|(dst, _, _)| *dst == a_op || *dst == b_op)
        .collect();
    assert_eq!(
        fanned.len(),
        2,
        "the uptime probe must fan out to BOTH service health ops (2 edges), got: {edges:?}"
    );
    for (_dst, prov, conf) in &fanned {
        assert_eq!(*prov, Provenance::Ambiguous, "fan-out must be Ambiguous");
        assert!(
            *conf < 0.40 && (*conf - 0.35).abs() < 1e-6,
            "fan-out edge must be at the Ambiguous 0.35 tier (< 0.40), got {conf}"
        );
    }

    // Cross-service impact: svc-a's health handler must NOT reach svc-b's op.
    let a_handler = fn_uid("repo-svc-a", "src/server.ts", "health");
    let r = impact(&g, &a_handler, &ImpactOptions::default());
    assert!(
        r.affected.iter().any(|a| a.uid == a_op),
        "impact(svc-a health handler) must reach svc-a's own health op"
    );
    assert!(
        !r.affected.iter().any(|a| a.uid == b_op),
        "impact(svc-a health handler) must NOT reach svc-b's unrelated health op"
    );
}

// ── Band guardrail: the api fan-out obeys the §4.1 Ambiguous band (< 0.40) ───
//
// The fan-out is the only place `link_estate` mints Ambiguous CONSUMES edges, so
// this is the non-vacuous extension of the band invariant to api-scoped identity:
// it confirms fan-out edges EXIST in this estate, then asserts every edge in the
// linked graph respects its provenance band — Ambiguous strictly below 0.40.

#[test]
fn fanout_edges_respect_the_ambiguous_band_non_vacuously() {
    let (tmp, manifest) = prep();
    let (g, cov, _results) = link_estate(&manifest, tmp.path());

    // Non-vacuous: this estate must actually contain api fan-out CONSUMES edges
    // (the GraphQL frontend consumer + the OpenAPI uptime probe, 2 each = 4).
    assert!(
        cov.consumers_ambiguous >= 4,
        "expected ≥ 4 Ambiguous fan-out CONSUMES edges (gql consumer + uptime probe \
         × 2 apis each), got {}",
        cov.consumers_ambiguous
    );

    // Every edge in the linked estate respects its §4.1 band; in particular every
    // Ambiguous fan-out edge is strictly < 0.40.
    let mut seen_ambiguous_consumes = false;
    let uids: Vec<Uid> = g.nodes().map(|n| n.uid.clone()).collect();
    for uid in &uids {
        for (edge, _) in g.neighbors(uid, Direction::Outgoing, &[]) {
            let conf = edge.confidence.value();
            let ok = match edge.provenance {
                Provenance::Extracted => (0.95..=1.0).contains(&conf),
                Provenance::Resolved | Provenance::Observed => (0.90..=1.0).contains(&conf),
                Provenance::Inferred => (0.40..=0.80).contains(&conf),
                Provenance::Ambiguous => conf < 0.40,
                Provenance::Model => true,
            };
            assert!(
                ok,
                "§4.1 band violated: {:?} edge {}->{} has conf {conf:.4}",
                edge.provenance,
                edge.src.as_str(),
                edge.dst.as_str()
            );
            if edge.kind == EdgeKind::Consumes && edge.provenance == Provenance::Ambiguous {
                seen_ambiguous_consumes = true;
            }
        }
    }
    assert!(
        seen_ambiguous_consumes,
        "expected at least one Ambiguous CONSUMES (fan-out) edge in the estate graph"
    );
}
