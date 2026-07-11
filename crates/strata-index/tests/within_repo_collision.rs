//! WITHIN-REPO api-collision honesty (the monorepo counterpart of the B6 estate
//! fix). Two schemas in ONE repo declaring the same `(format, key)` are two
//! distinct operation nodes (keyed by spec path) — so a consumer of that key
//! cannot honestly be linked to just one of them. The consumer pass must emit an
//! `Ambiguous` fan-out (one edge per owning schema, 0.35, `ambiguous: true`),
//! exactly like the estate pass does across repos — never a silent, confident,
//! first-wins pick.
//!
//! The unique case must stay byte-identical: with a single declaring schema the
//! consumer keeps its `Extracted` 0.95 tier.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use strata_contract::{ContractAdapter, GraphqlAdapter, OperationDef};
use strata_core::{AnalyzedFile, Direction, EdgeKind, NodeKind, Provenance, Uid};
use strata_index::assemble_graph_with_contracts;
use strata_lang_ts::{analyze, ResolveOptions};

const REPO: &str = "monorepo-collision";

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("within_repo_collision")
}

fn read_fixture(rel: &str) -> String {
    std::fs::read_to_string(fixture_dir().join(rel))
        .unwrap_or_else(|e| panic!("read fixture within_repo_collision/{rel}: {e}"))
}

fn analyzed() -> BTreeMap<String, AnalyzedFile> {
    let mut m = BTreeMap::new();
    let src = read_fixture("src/app.ts");
    m.insert("src/app.ts".to_string(), analyze("src/app.ts", &src));
    m
}

/// Extract operations from the given schema files, each under its own spec path
/// (exactly how the indexer's spec collection feeds multiple specs of one repo).
fn ops_of(schemas: &[&str]) -> Vec<OperationDef> {
    let mut ops = Vec::new();
    for rel in schemas {
        let text = read_fixture(rel);
        ops.extend(
            GraphqlAdapter
                .extract(rel, &text)
                .unwrap_or_else(|e| panic!("schema {rel} parses: {e}")),
        );
    }
    ops
}

fn op_uid(spec_path: &str, key: &str) -> Uid {
    Uid::new("contract", REPO, spec_path, key, "")
}

fn fn_uid(path: &str, fqn: &str) -> Uid {
    Uid::new("ts", REPO, path, fqn, "")
}

fn consumes_of(g: &strata_core::Graph, src: &Uid) -> Vec<(Uid, Provenance, f32)> {
    g.neighbors(src, Direction::Outgoing, &[EdgeKind::Consumes])
        .into_iter()
        .map(|(e, _)| (e.dst.clone(), e.provenance, e.confidence.value()))
        .collect()
}

#[test]
fn two_same_key_schemas_in_one_repo_stay_distinct_nodes() {
    let g = assemble_graph_with_contracts(
        &analyzed(),
        REPO,
        &ResolveOptions::default(),
        &ops_of(&["users.graphql", "billing.graphql"]),
    );
    let users_op = op_uid("users.graphql", "Query.getUser");
    let billing_op = op_uid("billing.graphql", "Query.getUser");
    assert!(g.get_node(&users_op).is_some(), "users.graphql node exists");
    assert!(
        g.get_node(&billing_op).is_some(),
        "billing.graphql node exists"
    );
    assert_eq!(
        g.nodes()
            .filter(|n| n.kind == NodeKind::GraphqlField && n.fqn == "Query.getUser")
            .count(),
        2,
        "the colliding key is two distinct nodes, never merged"
    );
}

#[test]
fn consumer_of_within_repo_colliding_key_fans_out_ambiguously() {
    // THE M3 FIX: with two schemas owning `Query.getUser`, the consumer must get
    // one Ambiguous 0.35 edge per owning schema — not a silent confident pick of
    // whichever schema sorts first.
    let g = assemble_graph_with_contracts(
        &analyzed(),
        REPO,
        &ResolveOptions::default(),
        &ops_of(&["users.graphql", "billing.graphql"]),
    );
    let consumer = fn_uid("src/app.ts", "loadUser");
    let mut edges = consumes_of(&g, &consumer);
    edges.sort_by_key(|e| e.0.to_string());

    assert_eq!(
        edges.len(),
        2,
        "one CONSUMES edge per owning schema (fan-out), got: {edges:?}"
    );
    let expected: Vec<Uid> = {
        let mut v = vec![
            op_uid("users.graphql", "Query.getUser"),
            op_uid("billing.graphql", "Query.getUser"),
        ];
        v.sort_by_key(|a| a.to_string());
        v
    };
    for ((dst, prov, conf), want) in edges.iter().zip(expected.iter()) {
        assert_eq!(dst, want, "fan-out reaches each owning schema's node");
        assert_eq!(
            *prov,
            Provenance::Ambiguous,
            "a within-repo collision is Ambiguous, never a confident pick"
        );
        assert!(
            *conf < 0.40,
            "fan-out confidence must sit in the Ambiguous band, got {conf}"
        );
        assert!(
            (*conf - 0.35).abs() < 1e-6,
            "fan-out uses the shared 0.35 tier, got {conf}"
        );
    }
}

#[test]
fn unique_key_consumer_keeps_extracted_tier() {
    // Non-vacuity guard: with a SINGLE declaring schema the unique case is
    // unchanged — Extracted 0.95, exactly one edge.
    let g = assemble_graph_with_contracts(
        &analyzed(),
        REPO,
        &ResolveOptions::default(),
        &ops_of(&["users.graphql"]),
    );
    let consumer = fn_uid("src/app.ts", "loadUser");
    let edges = consumes_of(&g, &consumer);
    assert_eq!(edges.len(), 1, "unique owner: exactly one edge");
    let (dst, prov, conf) = &edges[0];
    assert_eq!(dst, &op_uid("users.graphql", "Query.getUser"));
    assert_eq!(*prov, Provenance::Extracted, "unique GraphQL doc match");
    assert!((*conf - 0.95).abs() < 1e-6, "Extracted floor holds");
}
