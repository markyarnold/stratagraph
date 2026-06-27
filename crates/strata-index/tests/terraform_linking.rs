//! Terraform-plane graph-integration tests (Track D1, Slice 14, M1).
//!
//! The whole point of this slice: a Terraform `.tf` config flows through the SAME
//! `build_infra_plane` path the CloudFormation/SAM adapter uses, producing the
//! SAME typed nodes (`LambdaFn`/`IamRole`/`AppSyncApi`/…) and the SAME graded
//! edges (`Assumes`/`Routes`/`Contains`/`PRODUCES`). These tests prove:
//!
//! - Test 1: typed nodes + `Assumes`/`Routes` wiring at Extracted 0.95.
//! - Test 2: the money link (`PRODUCES` Lambda → `GraphqlField`).
//! - Test 3: honesty — a `var.`-only role ref invents NO `Assumes` edge; a
//!   ghost-field resolver invents NO `PRODUCES` edge.
//! - Test 4: an unknown provider (`google_*`) is a Generic `CloudResource` node,
//!   never dropped.
//! - Test 5: a Terraform config and a CloudFormation template COEXIST in one
//!   graph (a mixed repo).
//! - Test 6: determinism (building twice → identical graphs).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use strata_contract::{ContractAdapter, GraphqlAdapter, OperationDef};
use strata_core::{AnalyzedFile, Direction, EdgeKind, Graph, NodeKind, Provenance, Uid};
use strata_index::{assemble_graph_with_infra, build_infra_plane, InfraLinkCoverage};
use strata_infra::{CfnSamAdapter, IacAdapter, InfraTemplate, TerraformAdapter};
use strata_lang_ts::ResolveOptions;

const REPO: &str = "tf-appsync";
const FIXTURE: &str = "terraform_appsync";

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

fn operations() -> Vec<OperationDef> {
    let schema = read_fixture("schema.graphql");
    GraphqlAdapter
        .extract("schema.graphql", &schema)
        .expect("schema fixture parses")
}

/// Extract the Terraform config from the fixture.
fn tf_templates() -> Vec<InfraTemplate> {
    let content = read_fixture("main.tf");
    assert!(
        TerraformAdapter.detects("main.tf", &content),
        "main.tf must be detected as Terraform"
    );
    vec![TerraformAdapter
        .extract("main.tf", &content)
        .expect("main.tf parses")]
}

/// The full graph for the fixture (TF config + contract plane), with coverage.
fn build_with_cov() -> (Graph, InfraLinkCoverage) {
    let analyzed: BTreeMap<String, AnalyzedFile> = BTreeMap::new();
    assemble_graph_with_infra(
        &analyzed,
        REPO,
        &ResolveOptions::default(),
        &operations(),
        &tf_templates(),
    )
}

fn build() -> Graph {
    build_with_cov().0
}

fn infra_uid(logical_id: &str) -> Uid {
    Uid::new("infra", REPO, "main.tf", logical_id, "")
}

fn gql_uid(key: &str) -> Uid {
    Uid::new("contract", REPO, "schema.graphql", key, "")
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
fn tf_nodes_and_wiring_edges() {
    let g = build();

    // The typed nodes, each at the right kind (the `type.name` address is the id).
    let kinds = [
        ("aws_lambda_function.user", NodeKind::LambdaFn),
        ("aws_iam_role.user_exec", NodeKind::IamRole),
        ("aws_appsync_graphql_api.main", NodeKind::AppSyncApi),
        (
            "aws_appsync_datasource.user_ds",
            NodeKind::AppSyncDataSource,
        ),
        ("aws_appsync_resolver.get_user", NodeKind::AppSyncResolver),
        ("google_storage_bucket.assets", NodeKind::CloudResource),
    ];
    for (id, kind) in kinds {
        let node = g
            .get_node(&infra_uid(id))
            .unwrap_or_else(|| panic!("infra node {id} missing"));
        assert_eq!(node.kind, kind, "{id} kind");
        assert_eq!(node.name, id, "{id} name = logical id");
        assert_eq!(node.path, "main.tf", "{id} path = config path");
        assert_eq!(node.provenance, Provenance::Extracted);
    }

    // aws_lambda_function.user —Assumes→ aws_iam_role.user_exec (Extracted 0.95).
    let assumes = edges_of(
        &g,
        &infra_uid("aws_lambda_function.user"),
        EdgeKind::Assumes,
    );
    assert!(
        has_edge(
            &assumes,
            &infra_uid("aws_iam_role.user_exec"),
            Provenance::Extracted,
            0.95
        ),
        "user Lambda —Assumes→ user_exec role (Extracted 0.95): {assumes:?}"
    );

    // resolver —Routes→ data source —Routes→ Lambda (both Extracted 0.95).
    let resolver_routes = edges_of(
        &g,
        &infra_uid("aws_appsync_resolver.get_user"),
        EdgeKind::Routes,
    );
    assert!(
        has_edge(
            &resolver_routes,
            &infra_uid("aws_appsync_datasource.user_ds"),
            Provenance::Extracted,
            0.95
        ),
        "get_user resolver —Routes→ user_ds (Extracted 0.95): {resolver_routes:?}"
    );
    let ds_routes = edges_of(
        &g,
        &infra_uid("aws_appsync_datasource.user_ds"),
        EdgeKind::Routes,
    );
    assert!(
        has_edge(
            &ds_routes,
            &infra_uid("aws_lambda_function.user"),
            Provenance::Extracted,
            0.95
        ),
        "user_ds —Routes→ user Lambda (Extracted 0.95): {ds_routes:?}"
    );

    // The data source's `service_role_arn` also Assumes the role.
    let ds_assumes = edges_of(
        &g,
        &infra_uid("aws_appsync_datasource.user_ds"),
        EdgeKind::Assumes,
    );
    assert!(
        has_edge(
            &ds_assumes,
            &infra_uid("aws_iam_role.user_exec"),
            Provenance::Extracted,
            0.95
        ),
        "user_ds —Assumes→ user_exec (service_role_arn, Extracted 0.95): {ds_assumes:?}"
    );

    // api —Contains→ datasource/resolvers via their `api_id` (B3 parity).
    let contains = edges_of(
        &g,
        &infra_uid("aws_appsync_graphql_api.main"),
        EdgeKind::Contains,
    );
    for member in [
        "aws_appsync_datasource.user_ds",
        "aws_appsync_resolver.get_user",
        "aws_appsync_resolver.create_user",
    ] {
        assert!(
            has_edge(&contains, &infra_uid(member), Provenance::Extracted, 0.95),
            "main api —Contains→ {member} (Extracted 0.95): {contains:?}"
        );
    }
}

// ── Test 2: the money link (PRODUCES Lambda → GraphqlField) ──────────────────

#[test]
fn tf_money_link_produces_lambda_to_graphql_field() {
    let (g, cov) = build_with_cov();

    // The chain resolver→datasource→lambda is wholly Resource-graded, so the
    // PRODUCES edge sources from the LAMBDA at Extracted 0.95.
    let lambda_produces = edges_of(
        &g,
        &infra_uid("aws_lambda_function.user"),
        EdgeKind::Produces,
    );
    assert!(
        has_edge(
            &lambda_produces,
            &gql_uid("Query.getUser"),
            Provenance::Extracted,
            0.95
        ),
        "user Lambda —PRODUCES→ Query.getUser (Extracted 0.95): {lambda_produces:?}"
    );
    assert!(
        has_edge(
            &lambda_produces,
            &gql_uid("Mutation.createUser"),
            Provenance::Extracted,
            0.95
        ),
        "user Lambda —PRODUCES→ Mutation.createUser (Extracted 0.95): {lambda_produces:?}"
    );

    assert_eq!(cov.resolvers_linked, 2, "getUser + createUser linked");
    assert_eq!(
        cov.resolvers_total, 3,
        "three root resolvers (incl. the ghost)"
    );
}

// ── Test 3: honesty — var-only role + ghost-field resolver invent nothing ─────

#[test]
fn tf_honesty_no_invented_edges() {
    let (g, cov) = build_with_cov();

    // The `worker` Lambda's role is `var.worker_role_arn` — no same-file resource,
    // so NO Assumes edge is invented.
    let worker_assumes = edges_of(
        &g,
        &infra_uid("aws_lambda_function.worker"),
        EdgeKind::Assumes,
    );
    assert!(
        worker_assumes.is_empty(),
        "a var-only role ref must invent no Assumes edge: {worker_assumes:?}"
    );

    // The ghost resolver names `Query.ghostField`, which the schema does not
    // declare → NO PRODUCES edge (neither from the resolver nor the Lambda).
    let ghost_produces = edges_of(
        &g,
        &infra_uid("aws_appsync_resolver.ghost"),
        EdgeKind::Produces,
    );
    assert!(
        ghost_produces.is_empty(),
        "the ghost-field resolver must produce no edge: {ghost_produces:?}"
    );
    assert!(
        g.get_node(&gql_uid("Query.ghostField")).is_none(),
        "no GraphqlField node exists for the absent ghostField"
    );
    assert_eq!(
        cov.resolvers_unlinked, 1,
        "exactly one unlinked resolver (ghost)"
    );
}

// ── Test 4: an unknown provider is Generic inventory, never dropped ───────────

#[test]
fn tf_unknown_provider_is_generic_node() {
    let g = build();
    let bucket = g
        .get_node(&infra_uid("google_storage_bucket.assets"))
        .expect("the google_* resource must be a node, never dropped");
    assert_eq!(bucket.kind, NodeKind::CloudResource);
    // A Generic cloud resource carries no outgoing infra edges.
    assert!(
        g.neighbors(
            &infra_uid("google_storage_bucket.assets"),
            Direction::Outgoing,
            &[]
        )
        .is_empty(),
        "a Generic resource is inventory only (no outgoing edges)"
    );
}

// ── Test 5: a Terraform config and a CFN template COEXIST in one graph ───────

#[test]
fn tf_and_cfn_resources_coexist_in_one_graph() {
    // A tiny CFN/SAM template alongside the TF config — both detected by their own
    // adapters, both fed to the SAME `build_infra_plane`, both present in one graph
    // with their respective UID `path`s keeping them disjoint.
    let cfn_src = concat!(
        "Resources:\n",
        "  CfnFn:\n",
        "    Type: AWS::Serverless::Function\n",
        "    Properties:\n",
        "      Handler: c.handler\n",
        "      Role: !GetAtt CfnRole.Arn\n",
        "  CfnRole:\n",
        "    Type: AWS::IAM::Role\n",
        "    Properties: {}\n",
    );
    assert!(CfnSamAdapter.detects("template.yaml", cfn_src));
    let cfn = CfnSamAdapter
        .extract("template.yaml", cfn_src)
        .expect("cfn parses");

    // Both template sets in one builder call.
    let mut templates = tf_templates();
    templates.push(cfn);

    let analyzed: BTreeMap<String, AnalyzedFile> = BTreeMap::new();
    let mut g = strata_index::assemble_graph_with_contracts(
        &analyzed,
        REPO,
        &ResolveOptions::default(),
        &operations(),
    );
    let _cov = build_infra_plane(&mut g, REPO, &templates, &analyzed);

    // The TF Lambda node (path main.tf) AND the CFN Lambda node (path
    // template.yaml) both exist — disjoint by their UID path component.
    let tf_lambda = Uid::new("infra", REPO, "main.tf", "aws_lambda_function.user", "");
    let cfn_lambda = Uid::new("infra", REPO, "template.yaml", "CfnFn", "");
    assert_eq!(
        g.get_node(&tf_lambda).map(|n| n.kind),
        Some(NodeKind::LambdaFn),
        "the Terraform Lambda node coexists"
    );
    assert_eq!(
        g.get_node(&cfn_lambda).map(|n| n.kind),
        Some(NodeKind::LambdaFn),
        "the CloudFormation Lambda node coexists"
    );

    // Each Lambda Assumes its OWN role (no cross-contamination between planes).
    let cfn_assumes = edges_of(&g, &cfn_lambda, EdgeKind::Assumes);
    assert!(
        has_edge(
            &cfn_assumes,
            &Uid::new("infra", REPO, "template.yaml", "CfnRole", ""),
            Provenance::Extracted,
            0.95
        ),
        "CFN Lambda —Assumes→ CfnRole: {cfn_assumes:?}"
    );
    let tf_assumes = edges_of(&g, &tf_lambda, EdgeKind::Assumes);
    assert!(
        has_edge(
            &tf_assumes,
            &Uid::new("infra", REPO, "main.tf", "aws_iam_role.user_exec", ""),
            Provenance::Extracted,
            0.95
        ),
        "TF Lambda —Assumes→ its own role: {tf_assumes:?}"
    );
}

// ── Test 6: the TF Lambda Runs link is honestly unresolved (zip artifact) ────
//
// A `.tf` Lambda packages a zip (`filename = "build/user.zip"`); the source
// DIRECTORY is not present in the static config (it hides behind the build
// artifact / `archive_file`), so the `Runs` bridge resolves to no module and the
// Lambda is counted `lambdas_handler_unresolved` — a surfaced miss, never an
// invented edge. (This is the TF analogue of the C# `::`-handler deferral.)

#[test]
fn tf_lambda_runs_is_unresolved_zip_artifact() {
    let (g, cov) = build_with_cov();
    let runs = edges_of(&g, &infra_uid("aws_lambda_function.user"), EdgeKind::Runs);
    assert!(
        runs.is_empty(),
        "a zip-packaged TF Lambda resolves to no Runs edge (source dir not in config): {runs:?}"
    );
    // Both Lambdas (user + worker) are counted unresolved; none invented.
    assert_eq!(
        cov.lambdas_runs_linked, 0,
        "no TF Runs links in this fixture"
    );
    assert!(
        cov.lambdas_handler_unresolved >= 2,
        "both Lambdas' handlers are honestly unresolved: {cov:?}"
    );
}

// ── Test 7: determinism — building twice → identical graphs ──────────────────

#[test]
fn tf_building_twice_yields_identical_graphs() {
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
