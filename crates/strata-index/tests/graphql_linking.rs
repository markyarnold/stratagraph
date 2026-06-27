//! GraphQL contract-plane per-repo linking test (Slice 4, M2 — Definition of
//! Done test 4). The monolith fixture: GraphqlField nodes exist (correct kind),
//! a resolver `PRODUCES` Inferred 0.80 from the named handler node, and a `gql`
//! consumer `CONSUMES` Extracted 0.95 from the enclosing node.
//!
//! The estate tests (6–11) live in `tests/graphql_estate.rs`; the headline impact
//! test (6) in `tests/graphql_crossrepo_impact.rs`; the band-invariant extension
//! (5) in `tests/confidence_bands.rs`; the coverage tests (12) in
//! `tests/graphql_coverage.rs`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use strata_contract::{ContractAdapter, GraphqlAdapter, OperationDef};
use strata_core::{
    impact, AnalyzedFile, Direction, EdgeKind, ImpactOptions, NodeKind, Provenance, Uid,
};
use strata_index::assemble_graph_with_contracts;
use strata_lang_ts::{analyze, ResolveOptions};

const REPO: &str = "monolith-graphql";

fn fixture_dir(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

/// Read a file under `fixtures/<name>/`.
fn read_fixture(name: &str, rel: &str) -> String {
    std::fs::read_to_string(fixture_dir(name).join(rel))
        .unwrap_or_else(|e| panic!("read fixture {name}/{rel}: {e}"))
}

/// Analyze the monolith_graphql TS sources into the `path → AnalyzedFile` map.
fn monolith_analyzed() -> BTreeMap<String, AnalyzedFile> {
    let mut m = BTreeMap::new();
    for rel in ["src/resolvers.ts", "src/app.ts"] {
        let src = read_fixture("monolith_graphql", rel);
        m.insert(rel.to_string(), analyze(rel, &src));
    }
    m
}

/// Extract the GraphQL operations from the monolith schema.
fn monolith_ops() -> Vec<OperationDef> {
    let schema = read_fixture("monolith_graphql", "schema.graphql");
    assert!(
        GraphqlAdapter.detects("schema.graphql", &schema),
        "the schema fixture must be detected as a GraphQL schema"
    );
    GraphqlAdapter
        .extract("schema.graphql", &schema)
        .expect("schema fixture parses")
}

fn op_uid(repo: &str, spec_path: &str, key: &str) -> Uid {
    Uid::new("contract", repo, spec_path, key, "")
}

fn fn_uid(repo: &str, path: &str, fqn: &str) -> Uid {
    Uid::new("ts", repo, path, fqn, "")
}

fn module_uid(repo: &str, path: &str) -> Uid {
    Uid::new("ts", repo, path, "<module>", "")
}

fn edges_of(g: &strata_core::Graph, src: &Uid, kind: EdgeKind) -> Vec<(Uid, Provenance, f32)> {
    g.neighbors(src, Direction::Outgoing, &[kind])
        .into_iter()
        .map(|(e, _)| (e.dst.clone(), e.provenance, e.confidence.value()))
        .collect()
}

// ── Test 4: per-repo GraphqlField nodes + PRODUCES + CONSUMES ────────────────

#[test]
fn monolith_graphql_field_nodes_and_producer_consumer_links() {
    let analyzed = monolith_analyzed();
    let ops = monolith_ops();
    let g = assemble_graph_with_contracts(&analyzed, REPO, &ResolveOptions::default(), &ops);

    // GraphqlField nodes exist with the right kind (NOT ApiOperation).
    let get_user_op = op_uid(REPO, "schema.graphql", "Query.getUser");
    let node = g
        .get_node(&get_user_op)
        .unwrap_or_else(|| panic!("Query.getUser GraphqlField node missing"));
    assert_eq!(
        node.kind,
        NodeKind::GraphqlField,
        "kind must be GraphqlField"
    );
    assert_eq!(node.fqn, "Query.getUser");

    // All three root fields became GraphqlField nodes; no ApiOperation leaked in.
    let gql_fields = g
        .nodes()
        .filter(|n| n.kind == NodeKind::GraphqlField)
        .count();
    assert_eq!(
        gql_fields, 3,
        "Query.getUser/listUsers + Mutation.createUser"
    );
    assert_eq!(
        g.nodes()
            .filter(|n| n.kind == NodeKind::ApiOperation)
            .count(),
        0,
        "a GraphQL-only repo must have NO ApiOperation nodes"
    );

    // PRODUCES: the `getUser` named handler → Query.getUser at Inferred 0.80.
    let get_user_handler = fn_uid(REPO, "src/resolvers.ts", "getUser");
    let produces = edges_of(&g, &get_user_handler, EdgeKind::Produces);
    assert!(
        produces.iter().any(|(dst, prov, conf)| *dst == get_user_op
            && *prov == Provenance::Inferred
            && (*conf - 0.80).abs() < 1e-6),
        "getUser handler must PRODUCES Query.getUser (Inferred 0.80): {produces:?}"
    );

    // PRODUCES: the inline `listUsers` arrow attaches to the MODULE node (no
    // named handler), still Inferred 0.80 to Query.listUsers.
    let list_users_op = op_uid(REPO, "schema.graphql", "Query.listUsers");
    let module = module_uid(REPO, "src/resolvers.ts");
    let module_produces = edges_of(&g, &module, EdgeKind::Produces);
    assert!(
        module_produces
            .iter()
            .any(|(dst, prov, _)| *dst == list_users_op && *prov == Provenance::Inferred),
        "inline listUsers arrow PRODUCES Query.listUsers from the module: {module_produces:?}"
    );

    // CONSUMES: the `loadUser` gql query → Query.getUser at Extracted 0.95.
    let consumer = fn_uid(REPO, "src/app.ts", "loadUser");
    let consumes = edges_of(&g, &consumer, EdgeKind::Consumes);
    assert!(
        consumes.iter().any(|(dst, prov, conf)| *dst == get_user_op
            && *prov == Provenance::Extracted
            && (*conf - 0.95).abs() < 1e-6),
        "loadUser gql query must CONSUMES Query.getUser (Extracted 0.95): {consumes:?}"
    );

    // No false edge: the consumer reads ONLY getUser, so it must not CONSUMES
    // listUsers or createUser.
    let create_user_op = op_uid(REPO, "schema.graphql", "Mutation.createUser");
    assert!(
        !consumes
            .iter()
            .any(|(dst, _, _)| *dst == list_users_op || *dst == create_user_op),
        "the gql query consumes only getUser, no spurious field edges: {consumes:?}"
    );
}

// ── dogfood fix 1 — test 2 (linking): untagged consumer links identically ────
//
// The UNTAGGED `listAll` template constant (`const LIST_USERS = `query …``) in
// app.ts reads Query.listUsers. Once `parse_operations` succeeds it is
// evidence-identical to a tagged doc, so it links Extracted 0.95 — exactly like
// the tagged `loadUser` query. The non-GraphQL `BUTTON_CSS` constant in the same
// file produces no edge (the prefilter kept it out of the pipeline).

#[test]
fn monolith_untagged_template_constant_consumes_identically_to_tagged() {
    let analyzed = monolith_analyzed();
    let ops = monolith_ops();
    let g = assemble_graph_with_contracts(&analyzed, REPO, &ResolveOptions::default(), &ops);

    let list_users_op = op_uid(REPO, "schema.graphql", "Query.listUsers");

    // CONSUMES: the untagged `listAll` constant → Query.listUsers at Extracted
    // 0.95, the same tier the tagged `loadUser` query gets.
    let untagged_consumer = fn_uid(REPO, "src/app.ts", "listAll");
    let consumes = edges_of(&g, &untagged_consumer, EdgeKind::Consumes);
    assert!(
        consumes
            .iter()
            .any(|(dst, prov, conf)| *dst == list_users_op
                && *prov == Provenance::Extracted
                && (*conf - 0.95).abs() < 1e-6),
        "untagged listAll constant must CONSUMES Query.listUsers (Extracted 0.95): {consumes:?}"
    );

    // The non-GraphQL `BUTTON_CSS` constant lives at module top level; it must NOT
    // produce any CONSUMES edge from the module node.
    let module = module_uid(REPO, "src/app.ts");
    let module_consumes = edges_of(&g, &module, EdgeKind::Consumes);
    assert!(
        module_consumes.is_empty(),
        "the css constant must produce no consumer edge: {module_consumes:?}"
    );
}

// ── dogfood fix 2 (integration): impact ON a GraphqlField target ──────────────
//
// "I'm changing this schema field — who breaks?" The impact TARGET is the
// `Query.getUser` GraphqlField node itself (a contract node with NO outgoing
// PRODUCES). Over the real monolith graph, `impact(Query.getUser)` must now reach
// both the gql CONSUMER (`loadUser`, via the incoming CONSUMES Extracted 0.95) and
// the implementing PRODUCER handler (`getUser`, via the incoming PRODUCES Inferred
// 0.80). With the contract hop off it reaches neither (proving the reach is the
// contract plane, not a code edge).

#[test]
fn impact_on_graphql_field_target_reaches_consumer_and_producer() {
    let analyzed = monolith_analyzed();
    let ops = monolith_ops();
    let g = assemble_graph_with_contracts(&analyzed, REPO, &ResolveOptions::default(), &ops);

    let get_user_op = op_uid(REPO, "schema.graphql", "Query.getUser");
    assert_eq!(
        g.get_node(&get_user_op).map(|n| n.kind),
        Some(NodeKind::GraphqlField),
        "the impact target must be the Query.getUser GraphqlField node"
    );

    let r = impact(&g, &get_user_op, &ImpactOptions::default());

    // The CONSUMER `loadUser` is reached at the CONSUMES confidence (Extracted 0.95).
    let consumer = fn_uid(REPO, "src/app.ts", "loadUser");
    let c = r
        .affected
        .iter()
        .find(|a| a.uid == consumer)
        .unwrap_or_else(|| {
            panic!(
                "impact(Query.getUser) must include the gql consumer loadUser: {:?}",
                r.affected
                    .iter()
                    .map(|a| a.uid.as_str())
                    .collect::<Vec<_>>()
            )
        });
    assert!(
        (c.confidence - 0.95).abs() < 1e-5,
        "consumer reach conf = CONSUMES Extracted 0.95, got {}",
        c.confidence
    );

    // The implementing PRODUCER handler `getUser` is reached at the PRODUCES
    // confidence (Inferred 0.80) — changing the field forces the handler to change.
    let producer = fn_uid(REPO, "src/resolvers.ts", "getUser");
    let p = r
        .affected
        .iter()
        .find(|a| a.uid == producer)
        .unwrap_or_else(|| {
            panic!(
                "impact(Query.getUser) must include the producer handler getUser: {:?}",
                r.affected
                    .iter()
                    .map(|a| a.uid.as_str())
                    .collect::<Vec<_>>()
            )
        });
    assert!(
        (p.confidence - 0.80).abs() < 1e-5,
        "producer reach conf = PRODUCES Inferred 0.80, got {}",
        p.confidence
    );

    // Sanity: with the contract hop off, the GraphqlField target reaches neither
    // (it has no incoming CALLS) — proving the reach is via the contract plane.
    let code_only = impact(
        &g,
        &get_user_op,
        &ImpactOptions {
            include_contracts: false,
            ..ImpactOptions::default()
        },
    );
    assert!(
        !code_only.affected.iter().any(|a| a.uid == consumer)
            && !code_only.affected.iter().any(|a| a.uid == producer),
        "without the contract hop a GraphqlField target reaches no consumer/producer"
    );
}
