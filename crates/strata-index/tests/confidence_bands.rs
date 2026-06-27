//! Provenance-band invariant guardrail (design §4.1).
//!
//! Design §4.1 specifies that every edge's confidence must lie within the band
//! for its provenance:
//!
//!   Extracted  : 0.95 ≤ confidence ≤ 1.0
//!   Resolved   : 0.90 ≤ confidence ≤ 1.0
//!   Inferred   : 0.40 ≤ confidence ≤ 0.80
//!   Ambiguous  :        confidence <  0.40
//!
//! "An inference can never masquerade as a fact." Calibration of heuristic
//! constants must use min(measured_precision, band_ceiling) so that heuristic
//! edges never reach or exceed the RESOLVED or EXTRACTED tiers.
//!
//! This test iterates ALL edges produced by both the pure-heuristic build
//! (scip = None, covering the `resolve` fixture) and the resolved build
//! (scip = Some, same fixture) and asserts the band invariant holds for every
//! edge. It also directly asserts the CONF_* constants used in build.rs are
//! each within their band.
//!
//! If CONF_BARE_SINGLE or CONF_UNKNOWN_RECEIVER are ever raised above their
//! band ceilings (0.80 and <0.40 respectively) this test will fail, providing
//! the missing regression guard.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use strata_contract::OperationDef;
use strata_core::{Direction, EdgeKind, Provenance};
use strata_index::{
    assemble_graph,
    assemble_graph_with_contracts,
    assemble_graph_with_data,
    assemble_graph_with_infra,
    assemble_graph_with_scip,
    // The PRODUCTION §4.1 band constants — these tests assert the REAL values
    // (re-exported from strata-index), not drifting local literal copies (T1).
    CONF_BARE_MULTI,
    CONF_BARE_SINGLE,
    CONF_DATA_FACT,
    CONF_ORM_EXPLICIT,
    CONF_PRODUCES_EXTRACTED,
    CONF_PRODUCES_INFERRED,
    CONF_PRODUCES_MULTI,
    CONF_PRODUCES_SINGLE,
    CONF_REF_INFERRED,
    CONF_REF_RESOURCE,
    CONF_RUNS,
    CONF_THIS_METHOD,
    CONF_UNKNOWN_RECEIVER,
};
// The contract consumer-tier constants are already `pub` in strata-contract.
use strata_contract::{
    CONF_AMBIGUOUS, CONF_GRAPHQL_EXTRACTED, CONF_LITERAL_URL, CONF_OPERATION_ID, CONF_TEMPLATE_URL,
};
use strata_infra::{CfnSamAdapter, IacAdapter, InfraTemplate, TerraformAdapter};
use strata_lang_ts::{analyze, ResolveOptions};
use strata_scip::ScipResolver;

const REPO: &str = "strata-index-resolve";
const RESOLVE_INDEX: &[u8] = include_bytes!("fixtures/resolve/index.scip");

fn fixture_src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("resolve")
        .join("src")
}

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

fn analyze_all(sources: &BTreeMap<String, String>) -> BTreeMap<String, strata_core::AnalyzedFile> {
    sources
        .iter()
        .map(|(p, s)| (p.clone(), analyze(p, s)))
        .collect()
}

/// Assert every edge in `g` satisfies the §4.1 confidence band for its provenance.
fn assert_band_invariant(g: &strata_core::Graph, label: &str) {
    let uids: Vec<strata_core::Uid> = g.nodes().map(|n| n.uid.clone()).collect();
    let mut violations: Vec<String> = Vec::new();

    for uid in &uids {
        for (edge, _) in g.neighbors(uid, Direction::Outgoing, &[]) {
            let conf = edge.confidence.value();
            let ok = match edge.provenance {
                // Extracted: 0.95 ≤ conf ≤ 1.0 (§4.1)
                Provenance::Extracted => (0.95..=1.0).contains(&conf),
                // Resolved: 0.90 ≤ conf ≤ 1.0 (§4.1)
                Provenance::Resolved => (0.90..=1.0).contains(&conf),
                // Observed: 0.90 ≤ conf ≤ 1.0 (§4.1: runtime data, high trust)
                Provenance::Observed => (0.90..=1.0).contains(&conf),
                // Inferred: 0.40 ≤ conf ≤ 0.80 (§4.1)
                Provenance::Inferred => (0.40..=0.80).contains(&conf),
                // Ambiguous: conf < 0.40 (§4.1)
                Provenance::Ambiguous => conf < 0.40,
                // Model: knowledge-plane only; tagged separately, not gating impact
                // (§4.1 does not constrain a numeric band for Model; skip)
                Provenance::Model => true,
            };
            if !ok {
                violations.push(format!(
                    "  [{label}] {:?} provenance edge {}->{} has conf {:.4} (band violated)",
                    edge.provenance,
                    edge.src.as_str(),
                    edge.dst.as_str(),
                    conf,
                ));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "§4.1 band invariant violated in the {label} graph ({} violation(s)):\n{}",
        violations.len(),
        violations.join("\n"),
    );
}

// ── Test: band invariant holds in the heuristic (scip=None) graph ────────────

#[test]
fn heuristic_graph_satisfies_band_invariant() {
    let sources = load_sources();
    let analyzed = analyze_all(&sources);
    let empty: BTreeMap<String, String> = BTreeMap::new();
    let g = assemble_graph_with_scip(&analyzed, REPO, &ResolveOptions::default(), None, &empty);
    // Confirm the graph has edges to check.
    assert!(g.edge_count() > 0, "expected edges in the heuristic graph");
    assert_band_invariant(&g, "heuristic");
}

// ── Test: band invariant holds in the resolved (scip=Some) graph ─────────────

#[test]
fn resolved_graph_satisfies_band_invariant() {
    let sources = load_sources();
    let analyzed = analyze_all(&sources);
    let resolver = ScipResolver::from_bytes(RESOLVE_INDEX).expect("resolve index parses");
    let g = assemble_graph_with_scip(
        &analyzed,
        REPO,
        &ResolveOptions::default(),
        Some(&resolver),
        &sources,
    );
    assert!(g.edge_count() > 0, "expected edges in the resolved graph");
    assert_band_invariant(&g, "resolved");
}

// ── Test: also use assemble_graph (the slice-1 convenience path) ──────────────

#[test]
fn slice1_graph_satisfies_band_invariant() {
    let sources = load_sources();
    let analyzed = analyze_all(&sources);
    let g = assemble_graph(&analyzed, REPO, &ResolveOptions::default());
    assert!(g.edge_count() > 0, "expected edges in the slice-1 graph");
    assert_band_invariant(&g, "slice-1");
}

// ── Constant-level assertions: each CONF_* is within its band ─────────────────
//
// These are compile-time const-block assertions. A future edit to build.rs that
// raises a constant above its ceiling will fail here immediately with a named
// message rather than an opaque graph-invariant failure in the graph tests above.

#[test]
fn conf_constants_are_within_their_bands() {
    // Asserts the REAL build.rs heuristic-grade constants (re-exported from
    // strata-index), so the guard fails if production drifts out of band (T1 — no
    // more drifting literal copies).

    // CONF_BARE_SINGLE: Inferred band 0.40–0.80
    const {
        assert!(
            CONF_BARE_SINGLE >= 0.40 && CONF_BARE_SINGLE <= 0.80,
            "CONF_BARE_SINGLE is outside the Inferred band [0.40, 0.80]"
        )
    };

    // CONF_BARE_MULTI: Ambiguous band < 0.40
    const {
        assert!(
            CONF_BARE_MULTI < 0.40,
            "CONF_BARE_MULTI is >= 0.40 (Ambiguous ceiling)"
        )
    };

    // CONF_THIS_METHOD: Inferred band 0.40–0.80 (at ceiling; in-band)
    const {
        assert!(
            CONF_THIS_METHOD >= 0.40 && CONF_THIS_METHOD <= 0.80,
            "CONF_THIS_METHOD is outside the Inferred band [0.40, 0.80]"
        )
    };

    // CONF_UNKNOWN_RECEIVER: Ambiguous band < 0.40 (strictly below, positive)
    const {
        assert!(
            CONF_UNKNOWN_RECEIVER < 0.40,
            "CONF_UNKNOWN_RECEIVER is >= 0.40 (Ambiguous ceiling)"
        )
    };
    const {
        assert!(
            CONF_UNKNOWN_RECEIVER > 0.0,
            "CONF_UNKNOWN_RECEIVER must be > 0"
        )
    };
}

// ── Test 11: the band invariant extends to contract-plane `Produces` edges ───
//
// A producer link is a convention match (route method+path ⇒ operation), never a
// fact: a single match is `Inferred` (≤ 0.80), several matches are `Ambiguous`
// (< 0.40). This test builds a graph that contains BOTH a single-match (Inferred)
// and a multi-match (Ambiguous) `Produces` edge and runs the same §4.1 band
// invariant over it, then asserts the producer-confidence constants are in band.

/// Two routes in one module: `getUser` (matches exactly one operation →
/// Inferred) and `getThing` (matches two operations → Ambiguous).
fn routed_source() -> &'static str {
    concat!(
        "export function getUser(req, res) { res.end(); }\n",
        "app.get(\"/users/:id\", getUser);\n",
        "export function getThing(req, res) { res.end(); }\n",
        "app.get(\"/things/:id\", getThing);\n",
    )
}

/// The operations the routes match: one for `/users/{}` (single) and two for
/// `/things/{}` (forces Ambiguous), all GET.
fn routed_operations() -> Vec<OperationDef> {
    let op = |key: &str, path: &str, spec: &str| OperationDef {
        format: strata_contract::ContractFormat::OpenApi,
        key: key.to_string(),
        method: "GET".to_string(),
        path: path.to_string(),
        norm_path: path.replace("{id}", "{}"),
        operation_id: Some(key.to_string()),
        spec_path: spec.to_string(),
    };
    vec![
        op("getUser", "/users/{id}", "openapi.yaml"),
        op("getThingV1", "/things/{id}", "things-v1.yaml"),
        op("getThingV2", "/things/{id}", "things-v2.yaml"),
    ]
}

#[test]
fn produces_edges_satisfy_band_invariant() {
    let mut analyzed = BTreeMap::new();
    analyzed.insert(
        "src/routes.ts".to_string(),
        analyze("src/routes.ts", routed_source()),
    );
    let ops = routed_operations();

    let g = assemble_graph_with_contracts(&analyzed, REPO, &ResolveOptions::default(), &ops);

    // Confirm the graph actually contains Produces edges of BOTH provenances, so
    // the invariant below is exercising the contract plane, not vacuously true.
    let mut seen_inferred = false;
    let mut seen_ambiguous = false;
    for node in g.nodes() {
        for (edge, _) in g.neighbors(&node.uid, Direction::Outgoing, &[EdgeKind::Produces]) {
            match edge.provenance {
                Provenance::Inferred => seen_inferred = true,
                Provenance::Ambiguous => seen_ambiguous = true,
                other => panic!("unexpected provenance on a Produces edge: {other:?}"),
            }
        }
    }
    assert!(
        seen_inferred,
        "expected a single-match Inferred Produces edge in the graph"
    );
    assert!(
        seen_ambiguous,
        "expected a multi-match Ambiguous Produces edge in the graph"
    );

    // The §4.1 band invariant must hold for every edge, including Produces.
    assert_band_invariant(&g, "contract-producer");
}

#[test]
fn produces_conf_constants_are_within_their_bands() {
    // Asserts the REAL contract::CONF_PRODUCES_* (Inferred ceiling 0.80; Ambiguous
    // ceiling exclusive < 0.40), re-exported from strata-index. A future edit raising
    // either above its band fails here with a named message (T1).
    const {
        assert!(
            CONF_PRODUCES_SINGLE >= 0.40 && CONF_PRODUCES_SINGLE <= 0.80,
            "CONF_PRODUCES_SINGLE is outside the Inferred band [0.40, 0.80]"
        )
    };

    const {
        assert!(
            CONF_PRODUCES_MULTI < 0.40,
            "CONF_PRODUCES_MULTI is >= 0.40 (Ambiguous ceiling)"
        )
    };
}

// ── Test 7: the band invariant extends to contract-plane `Consumes` edges ────
//
// A consumer link is a name/URL convention match, never a fact: a unique match
// is `Inferred` (≤ 0.80, the tiered 0.75/0.70/0.60), several matches are
// `Ambiguous` (< 0.40). This builds a graph containing BOTH a single-match
// (Inferred) and a multi-match (Ambiguous) `Consumes` edge and runs the same
// §4.1 band invariant over it, then asserts the consumer-confidence constants.

/// A consumer module with two HTTP calls: a literal `/users/1` (matches exactly
/// one operation → Inferred) and a literal `/things/9` (matches two operations →
/// Ambiguous).
fn consumer_source() -> &'static str {
    concat!(
        "export async function loadUser() { return fetch(\"/users/1\"); }\n",
        "export async function loadThing() { return fetch(\"/things/9\"); }\n",
    )
}

/// Operations: one for `/users/{}` (single) and two for `/things/{}` (forces
/// Ambiguous), all GET.
fn consumer_operations() -> Vec<OperationDef> {
    let op = |key: &str, path: &str, spec: &str| OperationDef {
        format: strata_contract::ContractFormat::OpenApi,
        key: key.to_string(),
        method: "GET".to_string(),
        path: path.to_string(),
        norm_path: path.replace("{id}", "{}"),
        operation_id: Some(key.to_string()),
        spec_path: spec.to_string(),
    };
    vec![
        op("getUser", "/users/{id}", "openapi.yaml"),
        op("getThingV1", "/things/{id}", "things-v1.yaml"),
        op("getThingV2", "/things/{id}", "things-v2.yaml"),
    ]
}

#[test]
fn consumes_edges_satisfy_band_invariant() {
    let mut analyzed = BTreeMap::new();
    analyzed.insert(
        "src/client.ts".to_string(),
        analyze("src/client.ts", consumer_source()),
    );
    let ops = consumer_operations();

    let g = assemble_graph_with_contracts(&analyzed, REPO, &ResolveOptions::default(), &ops);

    // Confirm the graph contains Consumes edges of BOTH provenances, so the
    // invariant below exercises the consumer plane, not vacuously.
    let mut seen_inferred = false;
    let mut seen_ambiguous = false;
    for node in g.nodes() {
        for (edge, _) in g.neighbors(&node.uid, Direction::Outgoing, &[EdgeKind::Consumes]) {
            match edge.provenance {
                Provenance::Inferred => seen_inferred = true,
                Provenance::Ambiguous => seen_ambiguous = true,
                other => panic!("unexpected provenance on a Consumes edge: {other:?}"),
            }
        }
    }
    assert!(
        seen_inferred,
        "expected a single-match Inferred Consumes edge in the graph"
    );
    assert!(
        seen_ambiguous,
        "expected a multi-match Ambiguous Consumes edge in the graph"
    );

    // The §4.1 band invariant must hold for every edge, including Consumes.
    assert_band_invariant(&g, "contract-consumer");
}

#[test]
fn consumes_conf_constants_are_within_their_bands() {
    // Asserts the REAL strata_contract::CONF_* consumer tiers (Inferred ceiling 0.80;
    // Ambiguous ceiling exclusive < 0.40), imported directly from strata-contract.
    // A future edit raising any above its band fails here with a named message (T1).
    const {
        assert!(
            CONF_OPERATION_ID >= 0.40 && CONF_OPERATION_ID <= 0.80,
            "CONF_OPERATION_ID is outside the Inferred band [0.40, 0.80]"
        )
    };
    const {
        assert!(
            CONF_LITERAL_URL >= 0.40 && CONF_LITERAL_URL <= 0.80,
            "CONF_LITERAL_URL is outside the Inferred band [0.40, 0.80]"
        )
    };
    const {
        assert!(
            CONF_TEMPLATE_URL >= 0.40 && CONF_TEMPLATE_URL <= 0.80,
            "CONF_TEMPLATE_URL is outside the Inferred band [0.40, 0.80]"
        )
    };

    const {
        assert!(
            CONF_AMBIGUOUS < 0.40,
            "CONF_AMBIGUOUS is >= 0.40 (Ambiguous ceiling)"
        )
    };
}

// ── Test 5: the band invariant extends to GraphQL contract edges ─────────────
//
// A GraphQL resolver `PRODUCES` is a convention match → `Inferred` (0.80); a
// parsed `gql` document `CONSUMES` *names* the contract → `Extracted` (0.95, the
// EXTRACTED band floor). This builds a graph containing BOTH an Inferred PRODUCES
// and an Extracted CONSUMES (the GraphQL tier the OpenAPI suite never exercises)
// and runs the same §4.1 band invariant over it — non-vacuously.

/// A GraphQL-format operation `Query.<field>`.
fn gql_op(key: &str, method: &str, field: &str) -> OperationDef {
    OperationDef {
        format: strata_contract::ContractFormat::Graphql,
        key: key.to_string(),
        method: method.to_string(),
        path: field.to_string(),
        norm_path: field.to_string(),
        operation_id: None,
        spec_path: "schema.graphql".to_string(),
    }
}

/// A monolith GraphQL module: a `getUser` resolver (PRODUCES) and a `gql` query
/// that consumes `getUser` (CONSUMES Extracted 0.95).
fn graphql_source() -> &'static str {
    concat!(
        "export function getUser() { return {}; }\n",
        "export const resolvers = { Query: { getUser } };\n",
        "export function loadUser() {\n",
        "  return gql`query { getUser }`;\n",
        "}\n",
    )
}

#[test]
fn graphql_contract_edges_satisfy_band_invariant() {
    let mut analyzed = BTreeMap::new();
    analyzed.insert(
        "src/gql.ts".to_string(),
        analyze("src/gql.ts", graphql_source()),
    );
    let ops = vec![gql_op("Query.getUser", "QUERY", "getUser")];

    let g = assemble_graph_with_contracts(&analyzed, REPO, &ResolveOptions::default(), &ops);

    // Confirm the graph contains an Inferred PRODUCES AND an Extracted CONSUMES,
    // so the invariant below is exercising the GraphQL plane, not vacuously true.
    let mut seen_inferred_produces = false;
    let mut seen_extracted_consumes = false;
    for node in g.nodes() {
        for (edge, _) in g.neighbors(&node.uid, Direction::Outgoing, &[]) {
            match (edge.kind, edge.provenance) {
                (EdgeKind::Produces, Provenance::Inferred) => seen_inferred_produces = true,
                (EdgeKind::Consumes, Provenance::Extracted) => seen_extracted_consumes = true,
                _ => {}
            }
        }
    }
    assert!(
        seen_inferred_produces,
        "expected an Inferred GraphQL PRODUCES edge in the graph"
    );
    assert!(
        seen_extracted_consumes,
        "expected an Extracted GraphQL CONSUMES edge (0.95) in the graph"
    );

    // The §4.1 band invariant must hold for every edge, incl. the Extracted
    // CONSUMES (0.95–1.0 band) the OpenAPI consumer suite never produces.
    assert_band_invariant(&g, "graphql-contract");
}

#[test]
fn graphql_extracted_consumes_conf_is_within_band() {
    // Asserts the REAL strata_contract::CONF_GRAPHQL_EXTRACTED (Extracted band
    // 0.95–1.0), imported from strata-contract (T1).
    const {
        assert!(
            CONF_GRAPHQL_EXTRACTED >= 0.95 && CONF_GRAPHQL_EXTRACTED <= 1.0,
            "CONF_GRAPHQL_EXTRACTED is outside the Extracted band [0.95, 1.0]"
        )
    };
}

// ── Test 5 (bands): the band invariant extends to infra-plane edges ──────────
//
// The infra plane adds four edge kinds. Their bands (Slice 5, M2):
//   - `Assumes`/`Routes` from a `Resource` ref → Extracted 0.95; from a `Sub`/
//     `Join` `Inferred` ref → Inferred 0.70.
//   - `Runs` (Lambda → its handler module) → Extracted 0.95.
//   - money-link `PRODUCES` → Extracted 0.95 (chain fully `Resource`-graded) or
//     Inferred 0.70 (chain contains an `Inferred` hop).
// This builds a graph containing ALL of those at BOTH the Extracted and the
// Inferred tier (non-vacuously) and runs the same §4.1 band invariant over it.

/// A schema declaring the two root fields the two templates below resolve.
fn infra_schema() -> &'static str {
    concat!(
        "type Query { getUser(id: ID!): String }\n",
        "type Mutation { createUser(name: String!): String }\n",
    )
}

/// A handler module so the `Runs` bridge has a real `Module` to resolve to.
fn infra_handler_src() -> &'static str {
    "export function handler() { return 1; }\n"
}

/// Template A — a fully `Resource`-graded chain: the Lambda `PRODUCES`
/// `Query.getUser` (Extracted 0.95), `Assumes` its role (Extracted), `Routes`
/// resolve crisply (Extracted), the handler `Runs` to `src/a.ts`, and the API
/// `Contains` the datasource/resolver via their `ApiId` (Extracted 0.95, B3).
fn infra_template_resource() -> &'static str {
    concat!(
        "Resources:\n",
        "  AFn:\n",
        "    Type: AWS::Serverless::Function\n",
        "    Properties:\n",
        "      Handler: a.handler\n",
        "      CodeUri: src/\n",
        "      Role: !GetAtt ARole.Arn\n",
        "  ARole:\n",
        "    Type: AWS::IAM::Role\n",
        "    Properties: {}\n",
        "  Api:\n",
        "    Type: AWS::AppSync::GraphQLApi\n",
        "    Properties:\n",
        "      Name: a-api\n",
        "  ADS:\n",
        "    Type: AWS::AppSync::DataSource\n",
        "    Properties:\n",
        "      ApiId: !GetAtt Api.ApiId\n",
        "      LambdaConfig:\n",
        "        LambdaFunctionArn: !GetAtt AFn.Arn\n",
        "  AResolver:\n",
        "    Type: AWS::AppSync::Resolver\n",
        "    Properties:\n",
        "      ApiId: !GetAtt Api.ApiId\n",
        "      TypeName: Query\n",
        "      FieldName: getUser\n",
        "      DataSourceName: !GetAtt ADS.Name\n",
    )
}

/// Template B — an `Inferred`-graded chain via `Sub`: the Lambda `PRODUCES`
/// `Mutation.createUser` at Inferred 0.70 (the datasource→lambda hop is recovered
/// from a `Sub` interpolation), and `Assumes` its role via `Sub` (Inferred 0.70).
fn infra_template_inferred() -> &'static str {
    concat!(
        "Resources:\n",
        "  BFn:\n",
        "    Type: AWS::Serverless::Function\n",
        "    Properties:\n",
        "      Handler: b.handler\n",
        "      Role: !Sub \"${BRole.Arn}\"\n",
        "  BRole:\n",
        "    Type: AWS::IAM::Role\n",
        "    Properties: {}\n",
        "  BDS:\n",
        "    Type: AWS::AppSync::DataSource\n",
        "    Properties:\n",
        "      LambdaConfig:\n",
        "        LambdaFunctionArn: !Sub \"${BFn.Arn}\"\n",
        "  BResolver:\n",
        "    Type: AWS::AppSync::Resolver\n",
        "    Properties:\n",
        "      TypeName: Mutation\n",
        "      FieldName: createUser\n",
        "      DataSourceName: !GetAtt BDS.Name\n",
    )
}

fn extract_template(path: &str, content: &str) -> InfraTemplate {
    assert!(
        CfnSamAdapter.detects(path, content),
        "{path} must be detected as a CFN/SAM template"
    );
    CfnSamAdapter
        .extract(path, content)
        .unwrap_or_else(|e| panic!("{path} parses: {e}"))
}

#[test]
fn infra_edges_satisfy_band_invariant() {
    use strata_contract::{ContractAdapter, GraphqlAdapter};

    let mut analyzed = BTreeMap::new();
    analyzed.insert(
        "src/a.ts".to_string(),
        analyze("src/a.ts", infra_handler_src()),
    );
    let ops = GraphqlAdapter
        .extract("schema.graphql", infra_schema())
        .expect("schema parses");
    let templates = vec![
        extract_template("a.yaml", infra_template_resource()),
        extract_template("b.yaml", infra_template_inferred()),
    ];

    let (g, _cov) = assemble_graph_with_infra(
        &analyzed,
        REPO,
        &ResolveOptions::default(),
        &ops,
        &templates,
    );

    // Confirm the graph contains infra edges across all four kinds AND at BOTH
    // tiers, so the invariant below is exercising the infra plane, not vacuously.
    let mut seen_assumes_extracted = false;
    let mut seen_assumes_inferred = false;
    let mut seen_routes = false;
    let mut seen_runs = false;
    let mut seen_produces_extracted = false;
    let mut seen_produces_inferred = false;
    let mut seen_contains = false;
    for node in g.nodes() {
        for (edge, _) in g.neighbors(&node.uid, Direction::Outgoing, &[]) {
            match (edge.kind, edge.provenance) {
                (EdgeKind::Assumes, Provenance::Extracted) => seen_assumes_extracted = true,
                (EdgeKind::Assumes, Provenance::Inferred) => seen_assumes_inferred = true,
                (EdgeKind::Routes, _) => seen_routes = true,
                (EdgeKind::Runs, _) => seen_runs = true,
                (EdgeKind::Produces, Provenance::Extracted) => seen_produces_extracted = true,
                (EdgeKind::Produces, Provenance::Inferred) => seen_produces_inferred = true,
                (EdgeKind::Contains, _) => seen_contains = true,
                _ => {}
            }
        }
    }
    assert!(seen_assumes_extracted, "expected an Extracted Assumes edge");
    assert!(seen_assumes_inferred, "expected an Inferred Assumes edge");
    assert!(seen_routes, "expected a Routes edge");
    assert!(seen_runs, "expected a Runs edge");
    assert!(
        seen_produces_extracted,
        "expected an Extracted infra PRODUCES edge (fully Resource-graded chain)"
    );
    assert!(
        seen_produces_inferred,
        "expected an Inferred infra PRODUCES edge (Sub-graded chain hop)"
    );
    assert!(
        seen_contains,
        "expected an Api —Contains→ resolver/datasource edge (B3)"
    );

    // The §4.1 band invariant must hold for every edge, including the infra ones.
    assert_band_invariant(&g, "infra");
}

#[test]
fn infra_conf_constants_are_within_their_bands() {
    // Asserts the REAL infra.rs constants (re-exported from strata-index):
    // CONF_REF_RESOURCE / CONF_RUNS / CONF_PRODUCES_EXTRACTED in the Extracted band
    // 0.95–1.0, and CONF_REF_INFERRED / CONF_PRODUCES_INFERRED in the Inferred band
    // 0.40–0.80. A future edit raising one out of band fails here (T1).
    const {
        assert!(
            CONF_REF_RESOURCE >= 0.95 && CONF_REF_RESOURCE <= 1.0,
            "CONF_REF_RESOURCE is outside the Extracted band [0.95, 1.0]"
        )
    };
    const {
        assert!(
            CONF_RUNS >= 0.95 && CONF_RUNS <= 1.0,
            "CONF_RUNS is outside the Extracted band [0.95, 1.0]"
        )
    };
    const {
        assert!(
            CONF_PRODUCES_EXTRACTED >= 0.95 && CONF_PRODUCES_EXTRACTED <= 1.0,
            "CONF_PRODUCES_EXTRACTED is outside the Extracted band [0.95, 1.0]"
        )
    };
    const {
        assert!(
            CONF_REF_INFERRED >= 0.40 && CONF_REF_INFERRED <= 0.80,
            "CONF_REF_INFERRED is outside the Inferred band [0.40, 0.80]"
        )
    };
    const {
        assert!(
            CONF_PRODUCES_INFERRED >= 0.40 && CONF_PRODUCES_INFERRED <= 0.80,
            "CONF_PRODUCES_INFERRED is outside the Inferred band [0.40, 0.80]"
        )
    };
}

// ── Slice 14 (Track D1): the band invariant extends to Terraform infra edges ──
//
// A `.tf` config flows through the SAME `build_infra_plane` as CFN, so its edges
// must satisfy the SAME §4.1 bands. The grades come from the TerraformAdapter's
// `RefValue`s: a same-file resource reference → Extracted 0.95; an interpolation
// that recovers a same-file resource (`"…/${aws_iam_role.r.name}"`) → Inferred
// 0.70. This builds a graph containing BOTH tiers (non-vacuously) and runs the
// same invariant over it — proving nothing TF invents masquerades as a fact.

/// A Terraform config with BOTH an Extracted same-file `Assumes` (`role =
/// aws_iam_role.crisp.arn`) AND an Inferred one (an interpolated ARN embedding a
/// same-file role), plus the resolver→datasource→lambda money chain.
fn tf_band_config() -> &'static str {
    concat!(
        "resource \"aws_iam_role\" \"crisp\" {\n",
        "  name = \"crisp\"\n",
        "}\n",
        "resource \"aws_lambda_function\" \"fn\" {\n",
        "  function_name = \"fn\"\n",
        "  role          = aws_iam_role.crisp.arn\n",
        "  handler       = \"i.h\"\n",
        "}\n",
        // An Inferred Assumes: the role is recovered from a `${…}` interpolation.
        "resource \"aws_lambda_function\" \"fn2\" {\n",
        "  function_name = \"fn2\"\n",
        "  role          = \"arn:aws:iam::123:role/${aws_iam_role.crisp.name}\"\n",
        "  handler       = \"i.h\"\n",
        "}\n",
        "resource \"aws_appsync_graphql_api\" \"api\" {\n",
        "  name = \"api\"\n",
        "}\n",
        "resource \"aws_appsync_datasource\" \"ds\" {\n",
        "  api_id = aws_appsync_graphql_api.api.id\n",
        "  name   = \"ds\"\n",
        "  lambda_config {\n",
        "    function_arn = aws_lambda_function.fn.arn\n",
        "  }\n",
        "}\n",
        "resource \"aws_appsync_resolver\" \"r\" {\n",
        "  api_id      = aws_appsync_graphql_api.api.id\n",
        "  type        = \"Query\"\n",
        "  field       = \"getUser\"\n",
        "  data_source = aws_appsync_datasource.ds.name\n",
        "}\n",
    )
}

#[test]
fn terraform_infra_edges_satisfy_band_invariant() {
    use strata_contract::{ContractAdapter, GraphqlAdapter};

    let analyzed = BTreeMap::new();
    let ops = GraphqlAdapter
        .extract("schema.graphql", infra_schema())
        .expect("schema parses");
    assert!(TerraformAdapter.detects("main.tf", tf_band_config()));
    let templates = vec![TerraformAdapter
        .extract("main.tf", tf_band_config())
        .expect("tf config parses")];

    let (g, _cov) = assemble_graph_with_infra(
        &analyzed,
        REPO,
        &ResolveOptions::default(),
        &ops,
        &templates,
    );

    // Confirm the graph contains TF infra edges at BOTH tiers, so the invariant is
    // non-vacuous over the Terraform plane.
    let mut seen_assumes_extracted = false;
    let mut seen_assumes_inferred = false;
    let mut seen_routes = false;
    let mut seen_produces = false;
    let mut seen_contains = false;
    for node in g.nodes() {
        for (edge, _) in g.neighbors(&node.uid, Direction::Outgoing, &[]) {
            match (edge.kind, edge.provenance) {
                (EdgeKind::Assumes, Provenance::Extracted) => seen_assumes_extracted = true,
                (EdgeKind::Assumes, Provenance::Inferred) => seen_assumes_inferred = true,
                (EdgeKind::Routes, _) => seen_routes = true,
                (EdgeKind::Produces, _) => seen_produces = true,
                (EdgeKind::Contains, _) => seen_contains = true,
                _ => {}
            }
        }
    }
    assert!(
        seen_assumes_extracted,
        "expected an Extracted TF Assumes edge"
    );
    assert!(
        seen_assumes_inferred,
        "expected an Inferred TF Assumes edge (interpolation-recovered role)"
    );
    assert!(seen_routes, "expected a TF Routes edge");
    assert!(seen_produces, "expected a TF money-link PRODUCES edge");
    assert!(seen_contains, "expected a TF Api —Contains→ member edge");

    // The §4.1 band invariant must hold for every edge, including the TF ones.
    assert_band_invariant(&g, "terraform-infra");
}

// ── Slice 16 (Track D3): the band invariant extends to data-plane edges ──────
//
// The data plane adds two edge kinds, both pure-DDL facts → Extracted 0.95 (M1
// infers nothing): `HasColumn` (a Table → each Column) and `ForeignKey` (a Column →
// the Column it references). This builds a graph containing BOTH (non-vacuously)
// and runs the same §4.1 band invariant over it — proving every data edge is in the
// EXTRACTED band, never masquerading at a tier it did not earn.

/// A two-table schema with an explicit foreign key: `accounts.org_id → orgs.id`.
fn data_schema() -> strata_data::SchemaModel {
    use strata_data::{ColumnDef, ForeignKey, SchemaModel, TableDef};
    let col = |name: &str, ty: &str, nullable: bool, pk: bool| ColumnDef {
        name: name.into(),
        sql_type: ty.into(),
        nullable,
        primary_key: pk,
    };
    SchemaModel {
        path: "schema.sql".into(),
        tables: vec![
            TableDef {
                name: "orgs".into(),
                columns: vec![col("id", "BIGINT", false, true)],
                foreign_keys: vec![],
            },
            TableDef {
                name: "accounts".into(),
                columns: vec![
                    col("id", "BIGINT", false, true),
                    col("org_id", "BIGINT", false, false),
                ],
                foreign_keys: vec![ForeignKey {
                    column: "org_id".into(),
                    ref_table: "orgs".into(),
                    ref_column: "id".into(),
                }],
            },
        ],
        ..Default::default()
    }
}

#[test]
fn data_edges_satisfy_band_invariant() {
    let (g, _cov) = assemble_graph_with_data(REPO, &[data_schema()]);

    // Confirm the graph contains BOTH data edge kinds, so the invariant is
    // non-vacuous over the data plane.
    let mut seen_has_column = false;
    let mut seen_foreign_key = false;
    for node in g.nodes() {
        for (edge, _) in g.neighbors(&node.uid, Direction::Outgoing, &[]) {
            match edge.kind {
                EdgeKind::HasColumn => seen_has_column = true,
                EdgeKind::ForeignKey => seen_foreign_key = true,
                _ => {}
            }
        }
    }
    assert!(
        seen_has_column,
        "expected a HasColumn edge in the data graph"
    );
    assert!(
        seen_foreign_key,
        "expected a ForeignKey edge in the data graph"
    );

    // The §4.1 band invariant must hold for every edge, including the data ones.
    assert_band_invariant(&g, "data");
}

#[test]
fn data_code_to_table_reads_writes_satisfy_band_invariant_non_vacuously() {
    // The band guardrail extended to the M2 code→table edges: build the data plane
    // WITH code SQL candidates so it emits both `Reads` and `Writes` edges, then
    // assert (a) both kinds are present (non-vacuous) and (b) every edge sits in the
    // EXTRACTED band — the §4.1 invariant over Reads/Writes specifically.
    use strata_core::{Confidence, Graph, Node, NodeKind, Span, SqlCandidate, Uid};
    use strata_index::{build_data_plane, CodeSqlFile};

    let schemas = [data_schema()];
    let mut g = Graph::new();
    // Seed the code symbol nodes the Reads/Writes edges originate from.
    let seed = |g: &mut Graph, lang: &str, path: &str, fqn: &str| {
        g.add_node(Node {
            uid: Uid::new(lang, REPO, path, fqn, ""),
            kind: NodeKind::Function,
            name: fqn.into(),
            fqn: fqn.into(),
            path: path.into(),
            span: Span::default(),
            provenance: Provenance::Extracted,
            confidence: Confidence::new(1.0),
        });
    };
    seed(&mut g, "ts", "src/r.ts", "readOrgs");
    seed(&mut g, "ts", "src/w.ts", "writeAccount");
    let read_c = [SqlCandidate {
        text: "SELECT name FROM orgs".into(),
        enclosing_fqn: "readOrgs".into(),
        span: Span::default(),
    }];
    let write_c = [SqlCandidate {
        text: "INSERT INTO accounts (id) VALUES (1)".into(),
        enclosing_fqn: "writeAccount".into(),
        span: Span::default(),
    }];
    let code = [
        CodeSqlFile {
            lang: "ts",
            path: "src/r.ts",
            candidates: &read_c,
        },
        CodeSqlFile {
            lang: "ts",
            path: "src/w.ts",
            candidates: &write_c,
        },
    ];
    let cov = build_data_plane(&mut g, REPO, &schemas, &code, &[]);
    assert_eq!(cov.reads_linked, 1, "one Reads edge to a declared table");
    assert_eq!(cov.writes_linked, 1, "one Writes edge to a declared table");

    let mut seen_reads = false;
    let mut seen_writes = false;
    for node in g.nodes() {
        for (edge, _) in g.neighbors(&node.uid, Direction::Outgoing, &[]) {
            match edge.kind {
                EdgeKind::Reads => seen_reads = true,
                EdgeKind::Writes => seen_writes = true,
                _ => {}
            }
        }
    }
    assert!(seen_reads, "expected a Reads edge in the data graph");
    assert!(seen_writes, "expected a Writes edge in the data graph");

    // The §4.1 band invariant must hold for every edge, including Reads/Writes.
    assert_band_invariant(&g, "data-code-links");
}

#[test]
fn data_conf_constant_is_within_its_band() {
    // Asserts the REAL data.rs CONF_DATA_FACT (Extracted band 0.95–1.0), re-exported
    // from strata-index. A future edit dropping it below the floor fails here (T1).
    const {
        assert!(
            CONF_DATA_FACT >= 0.95 && CONF_DATA_FACT <= 1.0,
            "CONF_DATA_FACT is outside the Extracted band [0.95, 1.0]"
        )
    };
}

#[test]
fn data_orm_mapsto_satisfies_band_invariant_non_vacuously() {
    // The band guardrail extended to the M2b ORM model→table edge: build the data
    // plane WITH an ORM hint so it emits a `MapsTo` edge, then assert (a) the MapsTo
    // edge is present (non-vacuous) and (b) every edge sits in the EXTRACTED band.
    use strata_core::{Confidence, Graph, Node, NodeKind, OrmFramework, OrmModelHint, Span, Uid};
    use strata_index::{build_data_plane, CodeOrmFile};

    let schemas = [data_schema()];
    let mut g = Graph::new();
    // Seed the model class node the MapsTo edge originates from (data_schema declares
    // `orgs`).
    g.add_node(Node {
        uid: Uid::new("py", REPO, "models.py", "Org", ""),
        kind: NodeKind::Class,
        name: "Org".into(),
        fqn: "Org".into(),
        path: "models.py".into(),
        span: Span::default(),
        provenance: Provenance::Extracted,
        confidence: Confidence::new(1.0),
    });
    let hints = [OrmModelHint {
        model_fqn: "Org".into(),
        table_name: "orgs".into(),
        framework: OrmFramework::SqlAlchemy,
        span: Span::default(),
    }];
    let orm = [CodeOrmFile {
        lang: "py",
        path: "models.py",
        hints: &hints,
    }];
    let cov = build_data_plane(&mut g, REPO, &schemas, &[], &orm);
    assert_eq!(
        cov.orm_models_linked, 1,
        "one MapsTo edge to a declared table"
    );

    let mut seen_mapsto = false;
    for node in g.nodes() {
        for (edge, _) in g.neighbors(&node.uid, Direction::Outgoing, &[]) {
            if edge.kind == EdgeKind::MapsTo {
                seen_mapsto = true;
            }
        }
    }
    assert!(seen_mapsto, "expected a MapsTo edge in the data graph");

    // The §4.1 band invariant must hold for every edge, including MapsTo.
    assert_band_invariant(&g, "data-orm-mapsto");
}

#[test]
fn data_orm_conf_constant_is_within_its_band() {
    // Asserts the REAL data.rs CONF_ORM_EXPLICIT (Extracted band 0.95–1.0), re-exported
    // from strata-index: an explicit literal table name matching a declared table is a
    // fact (T1).
    const {
        assert!(
            CONF_ORM_EXPLICIT >= 0.95 && CONF_ORM_EXPLICIT <= 1.0,
            "CONF_ORM_EXPLICIT is outside the Extracted band [0.95, 1.0]"
        )
    };
}
