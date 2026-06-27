//! Infrastructure-plane graph-integration tests (Slice 5, M2 — Definition of
//! Done tests 1–6, 9). The `infra_appsync` fixture: a SAM + AppSync template wired
//! into the code and contract planes.
//!
//! - Test 1: typed nodes + `Assumes`/`Routes` wiring.
//! - Test 2: the money link (`PRODUCES` Lambda → `GraphqlField`).
//! - Test 3: THE PROOF — `impact(Query.getUser)` reaches the Lambda AND the
//!   frontend consumer (infra → contract → frontend), and reaches neither with
//!   `include_contracts=false`.
//! - Test 4: `Runs` (Lambda → code `Module`) for BOTH the TS and the Python
//!   handler (the EARNED Slice-9 flip — the Python handler now resolves).
//! - Test 5: honesty — the `ghostField` resolver is unlinked.
//! - Test 6: no cross-plane contamination (buildspec not detected).
//! - Test 9: determinism (building twice → identical graphs).
//!
//! The estate test (7) lives in `tests/graphql_estate.rs`; the coverage report
//! tests (8) in `tests/infra_coverage.rs`; the band-invariant extension (5, bands)
//! in `tests/confidence_bands.rs`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use strata_contract::{ContractAdapter, GraphqlAdapter, OperationDef};
use strata_core::{
    impact, AnalyzedFile, Direction, EdgeKind, Graph, ImpactOptions, NodeKind, Provenance, Uid,
};
use strata_index::{
    assemble_csharp, assemble_graph_with_contracts, assemble_python, assemble_rust,
    build_infra_plane, InfraLinkCoverage,
};
use strata_infra::{CfnSamAdapter, IacAdapter, InfraTemplate};
use strata_lang_cs::analyze as analyze_cs;
use strata_lang_py::analyze as analyze_py;
use strata_lang_rust::analyze as analyze_rust;
use strata_lang_ts::{analyze, ResolveOptions};

const REPO: &str = "infra-appsync";
const FIXTURE: &str = "infra_appsync";

fn fixture_dir(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn read_fixture(rel: &str) -> String {
    std::fs::read_to_string(fixture_dir(FIXTURE).join(rel))
        .unwrap_or_else(|e| panic!("read fixture {FIXTURE}/{rel}: {e}"))
}

/// Analyze the fixture's TS sources into the `path → AnalyzedFile` map.
fn analyzed() -> BTreeMap<String, AnalyzedFile> {
    let mut m = BTreeMap::new();
    for rel in ["src/handlers/user.ts", "src/client.ts"] {
        m.insert(rel.to_string(), analyze(rel, &read_fixture(rel)));
    }
    m
}

/// Analyze the fixture's Python sources into the `path → AnalyzedFile` map. With
/// the Python plane (Slice 9) the `PyFunction` handler `functions/py-op/app.py`
/// is indexed, so the `Runs` bridge resolves to its Python `Module` node.
fn analyzed_py() -> BTreeMap<String, AnalyzedFile> {
    let mut m = BTreeMap::new();
    let rel = "functions/py-op/app.py";
    m.insert(rel.to_string(), analyze_py(rel, &read_fixture(rel)));
    m
}

/// Extract the GraphQL operations from the fixture schema.
fn operations() -> Vec<OperationDef> {
    let schema = read_fixture("schema.graphql");
    assert!(
        GraphqlAdapter.detects("schema.graphql", &schema),
        "schema.graphql must be detected as a GraphQL schema"
    );
    GraphqlAdapter
        .extract("schema.graphql", &schema)
        .expect("schema fixture parses")
}

/// Extract the CFN/SAM templates from the fixture.
fn templates() -> Vec<InfraTemplate> {
    let content = read_fixture("template.yaml");
    assert!(
        CfnSamAdapter.detects("template.yaml", &content),
        "template.yaml must be detected as a CFN/SAM template"
    );
    vec![CfnSamAdapter
        .extract("template.yaml", &content)
        .expect("template fixture parses")]
}

/// The full graph for the fixture, built in the exact order `index_impl` uses:
/// code (TS) + contract → Python plane → infra plane (the infra `Runs` match sees
/// the COMBINED TS+Python file set, so the Python handler resolves). Returns the
/// graph and the [`InfraLinkCoverage`].
fn build_with_cov() -> (Graph, InfraLinkCoverage) {
    let ts = analyzed();
    let py = analyzed_py();
    // Code + contract plane (TS only — Python contributes no contract surface).
    let mut g = assemble_graph_with_contracts(&ts, REPO, &ResolveOptions::default(), &operations());
    // Python code plane added to the same graph.
    assemble_python(&mut g, REPO, &py);
    // Infra plane over the combined file set (the `Runs` existence oracle).
    let combined: BTreeMap<String, AnalyzedFile> = ts.into_iter().chain(py).collect();
    let cov = build_infra_plane(&mut g, REPO, &templates(), &combined);
    (g, cov)
}

/// The full graph for the fixture (coverage discarded).
fn build() -> Graph {
    build_with_cov().0
}

fn infra_uid(logical_id: &str) -> Uid {
    Uid::new("infra", REPO, "template.yaml", logical_id, "")
}

fn gql_uid(key: &str) -> Uid {
    Uid::new("contract", REPO, "schema.graphql", key, "")
}

fn module_uid(path: &str) -> Uid {
    Uid::new("ts", REPO, path, "<module>", "")
}

/// A `py`-tagged code `Module` UID (the Python plane's node identity).
fn py_module_uid(path: &str) -> Uid {
    Uid::new("py", REPO, path, "<module>", "")
}

fn edges_of(g: &Graph, src: &Uid, kind: EdgeKind) -> Vec<(Uid, Provenance, f32)> {
    g.neighbors(src, Direction::Outgoing, &[kind])
        .into_iter()
        .map(|(e, _)| (e.dst.clone(), e.provenance, e.confidence.value()))
        .collect()
}

fn has_edge(edges: &[(Uid, Provenance, f32)], dst: &Uid, prov: Provenance, conf: f32) -> bool {
    edges
        .iter()
        .any(|(d, p, c)| d == dst && *p == prov && (*c - conf).abs() < 1e-6)
}

// ── Test 1: typed nodes + Assumes/Routes wiring ──────────────────────────────

#[test]
fn infra_nodes_and_wiring_edges() {
    let g = build();

    // The six typed nodes + the Generic queue, each at the right kind.
    let kinds = [
        ("UserFunction", NodeKind::LambdaFn),
        ("UserRole", NodeKind::IamRole),
        ("Api", NodeKind::AppSyncApi),
        ("UserDS", NodeKind::AppSyncDataSource),
        ("GetUserResolver", NodeKind::AppSyncResolver),
        ("EventQueue", NodeKind::CloudResource),
    ];
    for (id, kind) in kinds {
        let node = g
            .get_node(&infra_uid(id))
            .unwrap_or_else(|| panic!("infra node {id} missing"));
        assert_eq!(node.kind, kind, "{id} kind");
        assert_eq!(node.name, id, "{id} name = logical id");
        assert_eq!(node.fqn, id, "{id} fqn = logical id");
        assert_eq!(node.path, "template.yaml", "{id} path = template path");
        assert_eq!(node.provenance, Provenance::Extracted);
        assert!((node.confidence.value() - 1.0).abs() < 1e-6);
    }

    // UserFunction —Assumes→ UserRole (Extracted 0.95, the same-template GetAtt).
    let assumes = edges_of(&g, &infra_uid("UserFunction"), EdgeKind::Assumes);
    assert!(
        has_edge(
            &assumes,
            &infra_uid("UserRole"),
            Provenance::Extracted,
            0.95
        ),
        "UserFunction —Assumes→ UserRole (Extracted 0.95): {assumes:?}"
    );

    // resolver —Routes→ UserDS —Routes→ UserFunction (both Extracted 0.95).
    let resolver_routes = edges_of(&g, &infra_uid("GetUserResolver"), EdgeKind::Routes);
    assert!(
        has_edge(
            &resolver_routes,
            &infra_uid("UserDS"),
            Provenance::Extracted,
            0.95
        ),
        "GetUserResolver —Routes→ UserDS (Extracted 0.95): {resolver_routes:?}"
    );
    let ds_routes = edges_of(&g, &infra_uid("UserDS"), EdgeKind::Routes);
    assert!(
        has_edge(
            &ds_routes,
            &infra_uid("UserFunction"),
            Provenance::Extracted,
            0.95
        ),
        "UserDS —Routes→ UserFunction (Extracted 0.95): {ds_routes:?}"
    );
}

// ── Test 4: Runs (Lambda → code Module) for BOTH the TS and Python handlers ──
//
// EARNED FLIP (Slice 9): `PyFunction`'s `app.handler` + `CodeUri: functions/py-op/`
// now resolves to the indexed `functions/py-op/app.py` Python `Module`, so it
// produces a `Runs` edge exactly like the TS handler. The pre-Python-plane
// expectation — `PyFunction has NO Runs` and `lambdas_handler_unresolved == 1` —
// is REPLACED by the assertions below. This is the whole point of the slice: the
// dead-end Python Lambda handlers now link to their code.
//   Flipped expectations:
//     • PyFunction Runs edge: ABSENT → PRESENT (Extracted 0.95, py module).
//     • cov.lambdas_runs_linked:        1 → 2.
//     • cov.lambdas_handler_unresolved: 1 → 0.

#[test]
fn lambda_runs_edges_for_ts_and_python_handlers() {
    let (g, cov) = build_with_cov();

    // UserFunction —Runs→ Module(src/handlers/user.ts) (Extracted 0.95) — the TS
    // handler, unchanged.
    let runs = edges_of(&g, &infra_uid("UserFunction"), EdgeKind::Runs);
    assert!(
        has_edge(
            &runs,
            &module_uid("src/handlers/user.ts"),
            Provenance::Extracted,
            0.95
        ),
        "UserFunction —Runs→ Module(src/handlers/user.ts) (Extracted 0.95): {runs:?}"
    );

    // EARNED: PyFunction —Runs→ Module(functions/py-op/app.py) (Extracted 0.95).
    // The Python `Module` node carries the `py` language tag, and the `Runs` edge
    // lands on it (this used to be pinned ABSENT).
    let py_runs = edges_of(&g, &infra_uid("PyFunction"), EdgeKind::Runs);
    assert!(
        has_edge(
            &py_runs,
            &py_module_uid("functions/py-op/app.py"),
            Provenance::Extracted,
            0.95
        ),
        "PyFunction —Runs→ Module(functions/py-op/app.py) (Extracted 0.95): {py_runs:?}"
    );
    // The target is a real, py-tagged Module node (not a phantom).
    assert_eq!(
        g.get_node(&py_module_uid("functions/py-op/app.py"))
            .map(|n| n.kind),
        Some(NodeKind::Module),
        "the Runs target is the indexed Python Module node"
    );

    // Both handlers resolve now; nothing is unresolved.
    assert_eq!(
        cov.lambdas_runs_linked, 2,
        "two Runs links (UserFunction → TS handler, PyFunction → Python handler)"
    );
    assert_eq!(
        cov.lambdas_handler_unresolved, 0,
        "no handler unresolved — the Python handler now resolves (the EARNED flip)"
    );
}

// ── Slice 11: a C# Lambda handler stays UNRESOLVED (Runs deferred, honest) ───
//
// A .NET Lambda `Handler` is `Assembly::Namespace.Type::Method`, not a file path,
// so `cs` is deliberately absent from `HANDLER_EXTS`. Even though the C# plane
// indexes the handler's `.cs` file into a real `Module` node, the infra `Runs`
// bridge cannot resolve the `::`-shaped handler (it needs csproj/assembly
// mapping, deferred). This pins the honest outcome: NO `Runs` edge, and the
// Lambda counts `lambdas_handler_unresolved` — a surfaced miss, never an invented
// edge. (Contrast Test 4, where the Python handler EARNED its Runs edge.)
#[test]
fn csharp_lambda_handler_is_unresolved_runs_deferred() {
    // A C# source the plane indexes into a `cs`-tagged Module node.
    let mut cs: BTreeMap<String, AnalyzedFile> = BTreeMap::new();
    cs.insert(
        "src/Handlers/UserHandler.cs".to_string(),
        analyze_cs(
            "src/Handlers/UserHandler.cs",
            "namespace MyFunctions { public class Handlers { public void Handle() { } } }",
        ),
    );

    // A SAM template whose Handler is the .NET CLR reference shape.
    let tpl = concat!(
        "Resources:\n",
        "  CsharpFn:\n",
        "    Type: AWS::Serverless::Function\n",
        "    Properties:\n",
        "      Runtime: dotnet8\n",
        "      Handler: MyFunctions::MyFunctions.Handlers::Handle\n",
        "      CodeUri: src/Handlers/\n",
    );
    let templates = vec![CfnSamAdapter.extract("template.yaml", tpl).expect("parses")];

    // Build a graph that already has the C# plane (so the Module node exists), then
    // run the infra plane with the C# file in the combined oracle.
    let mut g =
        assemble_graph_with_contracts(&BTreeMap::new(), "csinfra", &ResolveOptions::default(), &[]);
    assemble_csharp(&mut g, "csinfra", &cs);
    let cov = build_infra_plane(&mut g, "csinfra", &templates, &cs);

    // The C# Module node DOES exist (the plane indexed it) …
    assert_eq!(
        g.get_node(&Uid::new(
            "cs",
            "csinfra",
            "src/Handlers/UserHandler.cs",
            "<module>",
            ""
        ))
        .map(|n| n.kind),
        Some(NodeKind::Module),
        "the C# handler file is indexed as a Module node"
    );

    // … but the Lambda has NO Runs edge (the `::` handler is not a file path).
    let lambda = Uid::new("infra", "csinfra", "template.yaml", "CsharpFn", "");
    let runs = edges_of(&g, &lambda, EdgeKind::Runs);
    assert!(
        runs.is_empty(),
        "a C# Lambda handler must not resolve to a Runs edge (deferred): {runs:?}"
    );
    // And it is counted as an honest unresolved handler.
    assert_eq!(
        cov.lambdas_handler_unresolved, 1,
        "the C# handler counts unresolved (Runs deferred): {cov:?}"
    );
    assert_eq!(
        cov.lambdas_runs_linked, 0,
        "no Runs links for the C# handler"
    );
}

// ── Slice 21: a Rust Lambda handler stays UNRESOLVED (Runs deferred, honest) ──
//
// A Rust (cargo-lambda) Lambda's `Handler` is conventionally `bootstrap` (the
// provided.al2 entrypoint), and the deployed artifact maps to a Cargo BINARY NAME
// (Cargo.toml `[[bin]]`/`package.name`), not a `.rs` file path. So `rs` is
// deliberately absent from `HANDLER_EXTS`. Even though the Rust plane indexes the
// handler's `.rs` file into a real `Module` node, the infra `Runs` bridge cannot
// resolve `bootstrap` to that module (it needs Cargo-target mapping, deferred).
// This pins the honest outcome: NO `Runs` edge, and the Lambda counts
// `lambdas_handler_unresolved` — a surfaced miss, never an invented edge. (Contrast
// Test 4, where the Python handler EARNED its Runs edge.)
#[test]
fn rust_lambda_handler_is_unresolved_runs_deferred() {
    // A Rust source the plane indexes into a `rust`-tagged Module node.
    let mut rust: BTreeMap<String, AnalyzedFile> = BTreeMap::new();
    rust.insert(
        "lambdas/user/src/main.rs".to_string(),
        analyze_rust(
            "lambdas/user/src/main.rs",
            "fn main() {}\nfn handler() {}\n",
        ),
    );

    // A SAM template whose Handler is the cargo-lambda `bootstrap` entrypoint on the
    // provided.al2 custom runtime.
    let tpl = concat!(
        "Resources:\n",
        "  RustFn:\n",
        "    Type: AWS::Serverless::Function\n",
        "    Properties:\n",
        "      Runtime: provided.al2\n",
        "      Handler: bootstrap\n",
        "      CodeUri: lambdas/user/\n",
    );
    let templates = vec![CfnSamAdapter.extract("template.yaml", tpl).expect("parses")];

    // Build a graph that already has the Rust plane (so the Module node exists), then
    // run the infra plane with the Rust file in the combined oracle.
    let mut g = assemble_graph_with_contracts(
        &BTreeMap::new(),
        "rustinfra",
        &ResolveOptions::default(),
        &[],
    );
    assemble_rust(&mut g, "rustinfra", &rust);
    let cov = build_infra_plane(&mut g, "rustinfra", &templates, &rust);

    // The Rust Module node DOES exist (the plane indexed it) …
    assert_eq!(
        g.get_node(&Uid::new(
            "rust",
            "rustinfra",
            "lambdas/user/src/main.rs",
            "<module>",
            ""
        ))
        .map(|n| n.kind),
        Some(NodeKind::Module),
        "the Rust handler file is indexed as a Module node"
    );

    // … but the Lambda has NO Runs edge (`bootstrap` is a bin name, not a file path).
    let lambda = Uid::new("infra", "rustinfra", "template.yaml", "RustFn", "");
    let runs = edges_of(&g, &lambda, EdgeKind::Runs);
    assert!(
        runs.is_empty(),
        "a Rust Lambda handler must not resolve to a Runs edge (deferred): {runs:?}"
    );
    // And it is counted as an honest unresolved handler.
    assert_eq!(
        cov.lambdas_handler_unresolved, 1,
        "the Rust handler counts unresolved (Runs deferred): {cov:?}"
    );
    assert_eq!(
        cov.lambdas_runs_linked, 0,
        "no Runs links for the Rust handler"
    );
}

// ── Test 9: determinism — building twice → identical graphs ──────────────────

#[test]
fn building_twice_yields_identical_graphs() {
    let g1 = build();
    let g2 = build();

    let edge_set = |g: &Graph| -> std::collections::BTreeSet<(String, String, String)> {
        let mut s = std::collections::BTreeSet::new();
        for n in g.nodes() {
            for (e, _) in g.neighbors(&n.uid, Direction::Outgoing, &[]) {
                s.insert((
                    e.src.as_str().to_string(),
                    e.dst.as_str().to_string(),
                    format!("{:?}", e.kind),
                ));
            }
        }
        s
    };
    let node_set = |g: &Graph| -> std::collections::BTreeSet<String> {
        g.nodes().map(|n| n.uid.as_str().to_string()).collect()
    };

    assert_eq!(g1.node_count(), g2.node_count(), "same node count");
    assert_eq!(g1.edge_count(), g2.edge_count(), "same edge count");
    assert_eq!(node_set(&g1), node_set(&g2), "identical node sets");
    assert_eq!(edge_set(&g1), edge_set(&g2), "identical edge sets");
}

// ── Test 2: the money link (PRODUCES Lambda → GraphqlField) ──────────────────

#[test]
fn money_link_produces_lambda_to_graphql_field() {
    let g = build();

    // PRODUCES UserFunction → Query.getUser at Extracted 0.95 (chain fully
    // Resource-graded → the Lambda sources the edge).
    let lambda_produces = edges_of(&g, &infra_uid("UserFunction"), EdgeKind::Produces);
    assert!(
        has_edge(
            &lambda_produces,
            &gql_uid("Query.getUser"),
            Provenance::Extracted,
            0.95
        ),
        "UserFunction —PRODUCES→ Query.getUser (Extracted 0.95): {lambda_produces:?}"
    );
    // Same Lambda also PRODUCES Mutation.createUser (the other in-schema resolver).
    assert!(
        has_edge(
            &lambda_produces,
            &gql_uid("Mutation.createUser"),
            Provenance::Extracted,
            0.95
        ),
        "UserFunction —PRODUCES→ Mutation.createUser (Extracted 0.95): {lambda_produces:?}"
    );

    // The edge is sourced from the LAMBDA, not the resolver node (the chain is a
    // fact end-to-end), so the resolver node has NO outgoing PRODUCES.
    let resolver_produces = edges_of(&g, &infra_uid("GetUserResolver"), EdgeKind::Produces);
    assert!(
        resolver_produces.is_empty(),
        "a fully-resolved chain sources PRODUCES from the Lambda, not the resolver: \
         {resolver_produces:?}"
    );
}

// ── Test 3: THE PROOF — impact(Query.getUser) → Lambda + frontend consumer ───

#[test]
fn impact_on_field_reaches_lambda_and_frontend_consumer() {
    let g = build();
    let field = gql_uid("Query.getUser");
    assert_eq!(
        g.get_node(&field).map(|n| n.kind),
        Some(NodeKind::GraphqlField),
        "the impact target must be the Query.getUser GraphqlField node"
    );

    let r = impact(&g, &field, &ImpactOptions::default());
    let names: Vec<&str> = r.affected.iter().map(|a| a.uid.as_str()).collect();

    // The implementing Lambda is reached at depth 1, conf 0.95 (the infra PRODUCES
    // edge seeded by contract-target seeding — zero traversal changes).
    let lambda = infra_uid("UserFunction");
    let l = r
        .affected
        .iter()
        .find(|a| a.uid == lambda)
        .unwrap_or_else(|| panic!("impact(Query.getUser) must include the Lambda: {names:?}"));
    assert_eq!(l.depth, 1, "Lambda at depth 1 (incoming PRODUCES)");
    assert!(
        (l.confidence - 0.95).abs() < 1e-5,
        "Lambda reach conf = PRODUCES Extracted 0.95, got {}",
        l.confidence
    );

    // The frontend consumer is reached via the contract CONSUMES (Extracted 0.95).
    // The `GET_USER` constant lives at module top level, so the CONSUMES edge
    // originates from the client module node (the honest attribution for a
    // module-level const — exactly the call graph's own top-level rule).
    let consumer = module_uid("src/client.ts");
    let c = r.affected.iter().find(|a| a.uid == consumer).unwrap_or_else(|| {
        panic!("impact(Query.getUser) must include the frontend consumer (client module): {names:?}")
    });
    assert!(
        (c.confidence - 0.95).abs() < 1e-5,
        "consumer reach conf = CONSUMES Extracted 0.95, got {}",
        c.confidence
    );

    // include_contracts=false → neither the Lambda nor the consumer is reached
    // (proving the reach is the contract/infra plane, not a code edge).
    let code_only = impact(
        &g,
        &field,
        &ImpactOptions {
            include_contracts: false,
            ..ImpactOptions::default()
        },
    );
    assert!(
        !code_only.affected.iter().any(|a| a.uid == lambda)
            && !code_only.affected.iter().any(|a| a.uid == consumer),
        "without the contract hop, the field target reaches neither the Lambda nor the consumer"
    );
}

// ── Slice 10 B1b: THE §6.3 PROOF — impact(UserRole) → its Lambdas + reach ─────
//
// Change an IAM role, find dependent compute and its reach. `UserFunction` and
// `PyFunction` both `Assumes` `UserRole`, so impact(UserRole) reaches both at
// depth 1 (Extracted 0.95, the edge's own confidence, never re-graded); and —
// because the infra hop seeds the contract hop off each Lambda — the operations
// they produce and the frontend that consumes them are reached deeper.
// include_infra=false collapses the whole thing.

#[test]
fn impact_on_role_reaches_both_lambdas_and_their_reach() {
    let g = build();
    let role = infra_uid("UserRole");
    assert_eq!(
        g.get_node(&role).map(|n| n.kind),
        Some(NodeKind::IamRole),
        "the impact target must be the UserRole IamRole node"
    );

    let r = impact(&g, &role, &ImpactOptions::default());
    let ids: Vec<&str> = r.affected.iter().map(|a| a.uid.as_str()).collect();

    // Both assuming Lambdas at depth 1, conf 0.95 (the Assumes edge's own grade).
    for lambda_id in ["UserFunction", "PyFunction"] {
        let l = r
            .affected
            .iter()
            .find(|a| a.uid == infra_uid(lambda_id))
            .unwrap_or_else(|| {
                panic!("impact(UserRole) must include {lambda_id} at depth 1: {ids:?}")
            });
        assert_eq!(l.depth, 1, "{lambda_id} (assuming Lambda) at depth 1");
        assert!(
            (l.confidence - 0.95).abs() < 1e-5,
            "{lambda_id} reach conf = Assumes Extracted 0.95 (never re-graded), got {}",
            l.confidence
        );
    }

    // The reach off the Lambdas: UserFunction PRODUCES Query.getUser, whose
    // consumer is the client module — surfaced deeper via the money link.
    assert!(
        r.affected.iter().any(|a| a.uid == gql_uid("Query.getUser")),
        "the role's reach includes Query.getUser (produced by its UserFunction): {ids:?}"
    );
    assert!(
        r.affected
            .iter()
            .any(|a| a.uid == module_uid("src/client.ts")),
        "the role's reach includes the frontend consumer of getUser: {ids:?}"
    );

    // include_infra=false → the role has no incoming Calls, so NONE of it appears.
    let code_only = impact(
        &g,
        &role,
        &ImpactOptions {
            include_infra: false,
            ..ImpactOptions::default()
        },
    );
    assert!(
        code_only.affected.is_empty(),
        "without the infra hop, impact(UserRole) reaches nothing, got {:?}",
        code_only
            .affected
            .iter()
            .map(|a| a.uid.as_str())
            .collect::<Vec<_>>()
    );
}

// ── Slice 10 B1b: THE MODULE PROOF — impact(py handler) → Lambda → field → reach ─
//
// The chain-completer: a change to the Python handler module reaches its Lambda
// (incoming Runs), the operation that Lambda produces (Query.listUsers), and that
// operation's frontend consumer — the full code→infra→contract→frontend trace.

#[test]
fn impact_on_python_handler_module_reaches_lambda_field_and_consumer() {
    let g = build();
    let module = py_module_uid("functions/py-op/app.py");
    assert_eq!(
        g.get_node(&module).map(|n| n.kind),
        Some(NodeKind::Module),
        "the impact target must be the Python handler Module node"
    );

    let r = impact(&g, &module, &ImpactOptions::default());
    let ids: Vec<&str> = r.affected.iter().map(|a| a.uid.as_str()).collect();

    // PyFunction is reached via the incoming Runs edge at depth 1, conf 0.95.
    let lambda = r
        .affected
        .iter()
        .find(|a| a.uid == infra_uid("PyFunction"))
        .unwrap_or_else(|| panic!("impact(py module) must reach PyFunction: {ids:?}"));
    assert_eq!(
        lambda.depth, 1,
        "PyFunction (running the module) at depth 1"
    );
    assert!(
        (lambda.confidence - 0.95).abs() < 1e-5,
        "PyFunction reach conf = Runs Extracted 0.95, got {}",
        lambda.confidence
    );

    // The field PyFunction produces, and that field's consumer, are reached.
    assert!(
        r.affected
            .iter()
            .any(|a| a.uid == gql_uid("Query.listUsers")),
        "the handler's reach includes Query.listUsers (produced by PyFunction): {ids:?}"
    );
    assert!(
        r.affected
            .iter()
            .any(|a| a.uid == module_uid("src/client.ts")),
        "the handler's reach includes the frontend consumer of listUsers: {ids:?}"
    );
}

/// Print the two Slice-10 proof traces (the §6.3 role reach and the Python module
/// reach) for the slice report. Run with `--ignored --nocapture`.
#[test]
#[ignore = "run with --ignored --nocapture to print the §6.3 + module proof traces"]
fn print_role_and_module_impact_traces() {
    let g = build();
    let fmt = |r: &strata_core::ImpactResult| -> String {
        let mut rows: Vec<&strata_core::AffectedNode> = r.affected.iter().collect();
        rows.sort_by(|a, b| a.depth.cmp(&b.depth).then(a.name.cmp(&b.name)));
        rows.iter()
            .map(|a| {
                format!(
                    "    depth {} conf {:.2} {} {}",
                    a.depth,
                    a.confidence,
                    if a.ambiguous { "AMB" } else { "   " },
                    a.name
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let role = impact(&g, &infra_uid("UserRole"), &ImpactOptions::default());
    println!("§6.3 PROOF — impact(UserRole):\n{}", fmt(&role));
    let module = impact(
        &g,
        &py_module_uid("functions/py-op/app.py"),
        &ImpactOptions::default(),
    );
    println!(
        "MODULE PROOF — impact(functions/py-op/app.py):\n{}",
        fmt(&module)
    );
}

// ── Slice 10 B3: ApiId → Contains edges (AppSyncApi → resolver/datasource) ───
//
// Each resolver/datasource carries `ApiId: !GetAtt Api.ApiId`, so the API
// `Contains` it (Extracted 0.95, the same-template GetAtt). The membership edge
// lights up `context(Api).members` for free — but `impact` must NOT traverse it
// (changing the API container is not changing a resolver).

#[test]
fn api_contains_edges_to_resolvers_and_datasources() {
    let g = build();
    let contains = edges_of(&g, &infra_uid("Api"), EdgeKind::Contains);

    // The API contains both data sources and all three+ resolvers, each at the
    // graded confidence of its `ApiId` ref (Extracted 0.95, a same-template GetAtt).
    for member in [
        "UserDS",
        "GetUserResolver",
        "CreateUserResolver",
        "GhostResolver",
        "PyDS",
        "ListUsersResolver",
    ] {
        assert!(
            has_edge(&contains, &infra_uid(member), Provenance::Extracted, 0.95),
            "Api —Contains→ {member} (Extracted 0.95): {contains:?}"
        );
    }
    // Containment is one-directional: the resolver does not `Contains` the API.
    let resolver_contains = edges_of(&g, &infra_uid("GetUserResolver"), EdgeKind::Contains);
    assert!(
        resolver_contains.is_empty(),
        "a resolver does not Contain anything: {resolver_contains:?}"
    );
}

#[test]
fn context_api_lists_resolvers_under_members() {
    let g = build();
    // context(Api).members must list its resolvers/datasources (via Contains).
    let ctx = strata_core::context(&g, &infra_uid("Api")).expect("Api context");
    let member_names: Vec<&str> = ctx.members.iter().map(|n| n.name.as_str()).collect();
    for member in ["UserDS", "GetUserResolver", "ListUsersResolver"] {
        assert!(
            member_names.contains(&member),
            "context(Api).members must list {member}: {member_names:?}"
        );
    }
}

#[test]
fn impact_does_not_traverse_contains_from_the_api() {
    // THE ASSERTION: changing the API container does not change its resolvers, so
    // impact(Api) must NOT pull them in through Contains. The API has no incoming
    // dependency edges here, so its blast radius is empty.
    let g = build();
    let r = impact(&g, &infra_uid("Api"), &ImpactOptions::default());
    let pulled: Vec<&str> = r
        .affected
        .iter()
        .filter(|a| {
            [
                "GetUserResolver",
                "CreateUserResolver",
                "UserDS",
                "ListUsersResolver",
                "PyDS",
            ]
            .contains(&a.name.as_str())
        })
        .map(|a| a.name.as_str())
        .collect();
    assert!(
        pulled.is_empty(),
        "impact(Api) must NOT traverse Contains to its resolvers/datasources, got {pulled:?}"
    );
}

// ── Slice 10 B2: Fn::If distinct branch targets → an edge per target ─────────
//
// A Lambda whose `Role` is `!If [Cond, !GetAtt BlueRole.Arn, !GetAtt GreenRole.Arn]`
// names TWO distinct same-template roles. The grader returns InferredMulti, and
// the builder emits a graded `Assumes` edge to EACH (Inferred 0.70) — both
// possible deployments surfaced. Same-id branches collapse to one Inferred edge.

#[test]
fn fn_if_distinct_role_branches_emit_an_assumes_edge_each() {
    let tpl = concat!(
        "Resources:\n",
        "  BlueGreenFn:\n",
        "    Type: AWS::Serverless::Function\n",
        "    Properties:\n",
        "      Handler: x.handler\n",
        "      Role: !If [IsBlue, !GetAtt BlueRole.Arn, !GetAtt GreenRole.Arn]\n",
        "  BlueRole:\n",
        "    Type: AWS::IAM::Role\n",
        "    Properties: {}\n",
        "  GreenRole:\n",
        "    Type: AWS::IAM::Role\n",
        "    Properties: {}\n",
    );
    assert!(CfnSamAdapter.detects("bg.yaml", tpl));
    let templates = vec![CfnSamAdapter.extract("bg.yaml", tpl).expect("parses")];
    let mut g =
        assemble_graph_with_contracts(&BTreeMap::new(), "bg", &ResolveOptions::default(), &[]);
    let _ = build_infra_plane(&mut g, "bg", &templates, &BTreeMap::new());

    let fn_uid = Uid::new("infra", "bg", "bg.yaml", "BlueGreenFn", "");
    let blue = Uid::new("infra", "bg", "bg.yaml", "BlueRole", "");
    let green = Uid::new("infra", "bg", "bg.yaml", "GreenRole", "");
    let assumes: Vec<(Uid, Provenance, f32)> = g
        .neighbors(&fn_uid, Direction::Outgoing, &[EdgeKind::Assumes])
        .into_iter()
        .map(|(e, _)| (e.dst.clone(), e.provenance, e.confidence.value()))
        .collect();

    // One Inferred 0.70 Assumes edge per distinct branch role.
    assert_eq!(
        assumes.len(),
        2,
        "an Assumes edge per branch target: {assumes:?}"
    );
    for role in [&blue, &green] {
        assert!(
            assumes.iter().any(|(d, p, c)| d == role
                && *p == Provenance::Inferred
                && (*c - 0.70).abs() < 1e-6),
            "BlueGreenFn —Assumes(Inferred 0.70)→ {role:?}: {assumes:?}"
        );
    }
}

// ── Test 5: honesty — the ghostField resolver is unlinked ────────────────────

#[test]
fn ghost_field_resolver_is_unlinked() {
    let (g, cov) = build_with_cov();

    // The GhostResolver names `Query.ghostField`, which no schema declares, so it
    // produces NO PRODUCES edge — neither from the resolver nor (since it would be
    // the chain source) the Lambda to a ghost field.
    let resolver_produces = edges_of(&g, &infra_uid("GhostResolver"), EdgeKind::Produces);
    assert!(
        resolver_produces.is_empty(),
        "GhostResolver must produce no edge: {resolver_produces:?}"
    );
    // No GraphqlField node named Query.ghostField exists, and the Lambda's PRODUCES
    // set never points at one.
    let lambda_produces = edges_of(&g, &infra_uid("UserFunction"), EdgeKind::Produces);
    assert!(
        !lambda_produces
            .iter()
            .any(|(d, _, _)| d == &gql_uid("Query.ghostField")),
        "no PRODUCES edge may point at the absent Query.ghostField: {lambda_produces:?}"
    );

    assert_eq!(
        cov.resolvers_unlinked, 1,
        "exactly one unlinked resolver (ghostField)"
    );
    // EARNED (Slice 10, B1b module proof): `ListUsersResolver` now backs PyFunction
    // and names the in-schema `Query.listUsers`, so it links — 2 → 3 linked, 3 → 4
    // total. The single unlinked resolver (ghostField) is unchanged.
    assert_eq!(
        cov.resolvers_linked, 3,
        "three linked resolvers (getUser + createUser + listUsers)"
    );
    assert_eq!(cov.resolvers_total, 4, "four root resolvers total");
}

// ── Test 6: no cross-plane contamination ─────────────────────────────────────

#[test]
fn buildspec_is_not_detected_as_a_template() {
    // The fixture's buildspec.yml mentions neither a Resources map nor AWS:: types.
    let buildspec = read_fixture("buildspec.yml");
    assert!(
        !CfnSamAdapter.detects("buildspec.yml", &buildspec),
        "a buildspec must not be detected as a CFN/SAM template"
    );
}

/// Dogfood regression: SAM `CodeUri` is relative to the
/// TEMPLATE FILE's directory, not the repo root. A template at
/// `backend/template.yaml` with `CodeUri: functions/op/` must resolve its
/// handler to `backend/functions/op/app.py`. Before the fix, candidates were
/// built repo-root-relative, so every Lambda in a non-root template counted
/// handler-unresolved before the fix.
#[test]
fn runs_resolves_code_uri_relative_to_template_dir() {
    let sub = fixture_dir("infra_subdir");
    let read = |rel: &str| {
        std::fs::read_to_string(sub.join(rel))
            .unwrap_or_else(|e| panic!("read infra_subdir/{rel}: {e}"))
    };
    let mut py = BTreeMap::new();
    let rel = "backend/functions/op/app.py";
    py.insert(rel.to_string(), analyze_py(rel, &read(rel)));
    let mut g = assemble_graph_with_contracts(
        &BTreeMap::new(),
        "infra-subdir",
        &ResolveOptions::default(),
        &[],
    );
    assemble_python(&mut g, "infra-subdir", &py);
    let tpl_src = read("backend/template.yaml");
    assert!(CfnSamAdapter.detects("backend/template.yaml", &tpl_src));
    let templates = vec![CfnSamAdapter
        .extract("backend/template.yaml", &tpl_src)
        .expect("subdir template parses")];
    let cov = build_infra_plane(&mut g, "infra-subdir", &templates, &py);
    let op_uid = Uid::new(
        "infra",
        "infra-subdir",
        "backend/template.yaml",
        "OpFunction",
        "",
    );
    let runs = edges_of(&g, &op_uid, EdgeKind::Runs);
    assert_eq!(runs.len(), 1, "exactly one Runs edge: {runs:#?}");
    let (dst, prov, conf) = &runs[0];
    let dst_node = g.get_node(dst).expect("dst module node");
    assert_eq!(dst_node.path, "backend/functions/op/app.py");
    assert_eq!(*prov, Provenance::Extracted);
    assert!((conf - 0.95).abs() < f32::EPSILON, "conf {conf}");
    assert_eq!(cov.lambdas_runs_linked, 1, "{cov:?}");
    assert_eq!(cov.lambdas_handler_unresolved, 0, "{cov:?}");
}

/// Dogfood regression: IAM roles are referenced through
/// MANY property names: `ServiceRoleArn` (AppSync data sources),
/// `RoleArn` (Step Functions / EventBridge), nested
/// `LogConfig.CloudWatchLogsRoleArn` (AppSync API) — not just a Lambda's
/// `Role`. Before the fix only `Role` produced an `Assumes` edge, so every
/// real role node showed nothing assuming it.
#[test]
fn assumes_edges_from_all_role_bearing_properties() {
    let sub = fixture_dir("infra_roles");
    let tpl_src =
        std::fs::read_to_string(sub.join("template.yaml")).expect("read infra_roles template");
    assert!(CfnSamAdapter.detects("template.yaml", &tpl_src));
    let templates = vec![CfnSamAdapter
        .extract("template.yaml", &tpl_src)
        .expect("infra_roles template parses")];
    let mut g = assemble_graph_with_contracts(
        &BTreeMap::new(),
        "infra-roles",
        &ResolveOptions::default(),
        &[],
    );
    let none: BTreeMap<String, AnalyzedFile> = BTreeMap::new();
    let _cov = build_infra_plane(&mut g, "infra-roles", &templates, &none);

    let uid = |logical: &str| Uid::new("infra", "infra-roles", "template.yaml", logical, "");
    for (src, dst) in [
        ("OpFunction", "FnRole"), // Role (the only one that worked before)
        ("OpDS", "InvokeRole"),   // ServiceRoleArn
        ("Api", "LogsRole"),      // LogConfig.CloudWatchLogsRoleArn (nested)
        ("Pipeline", "ExecRole"), // RoleArn
    ] {
        let assumes = edges_of(&g, &uid(src), EdgeKind::Assumes);
        assert_eq!(
            assumes.len(),
            1,
            "{src} must Assume exactly one role, got {assumes:?}"
        );
        let (got_dst, prov, conf) = &assumes[0];
        assert_eq!(*got_dst, uid(dst), "{src} must assume {dst}");
        assert_eq!(*prov, Provenance::Extracted);
        assert!((conf - 0.95).abs() < f32::EPSILON);
    }
}
