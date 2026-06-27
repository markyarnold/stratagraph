//! gRPC / protobuf contract-plane tests (Slice 17, Track D4a, M1).
//!
//! Substrate-first, exactly as the GraphQL plane started: this slice lands the
//! `.proto` → `ApiOperation` extraction and the estate `(api_id, format, key)`
//! identity. Producer/consumer CODE linking is M2 (with honest banding), so these
//! tests assert operation NODES + estate identity only — no `PRODUCES`/`CONSUMES`
//! edges are expected for gRPC at M1.
//!
//! Two levels:
//! - In-memory (`assemble_graph_with_contracts`): a gRPC `OperationDef` builds an
//!   `ApiOperation` node (Extracted 1.0) — the rpc is a fact.
//! - End-to-end estate (`index_estate` + `link_estate` over the committed
//!   `crossrepo_grpc` fixture): a shared `.proto` across two repos that declare
//!   the same api id collapses to ONE canonical node (B6 dedup); a gRPC `Query.
//!   getOrder` and a GraphQL `Query.getOrder` in one estate stay TWO distinct
//!   nodes (the format discriminator).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use strata_contract::{ContractAdapter, ContractFormat, ProtoAdapter};
use strata_core::{AnalyzedFile, NodeKind, Provenance, Uid};
use strata_index::{
    assemble_graph_with_contracts, index_estate, link_estate, ResolveMode, WorkspaceManifest,
};
use strata_lang_ts::ResolveOptions;

const ESTATE: &str = "grpc-estate";

// ── Fixture helpers (mirroring the GraphQL estate tests) ─────────────────────

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

/// Copy `fixtures/<name>` to a tempdir, index each repo, and return (tempdir,
/// manifest).
fn prep_estate(name: &str) -> (tempfile::TempDir, WorkspaceManifest) {
    let tmp = tempfile::tempdir().expect("tempdir");
    copy_dir_all(&fixture_dir(name), tmp.path()).expect("copy fixture");
    let manifest_path = tmp.path().join("strata.workspace.toml");
    let manifest = WorkspaceManifest::parse_file(&manifest_path).expect("manifest parses");
    index_estate(&manifest, &manifest_path, ResolveMode::Off);
    (tmp, manifest)
}

/// The canonical (estate-wide) operation UID for `key` owned by api `api_id` in
/// `format`: the `{api_id}/{format}` discriminator in the spec-path slot (B6
/// api-scoped identity).
fn canonical_uid(estate: &str, api_id: &str, format: &str, key: &str) -> Uid {
    Uid::new("contract", estate, &format!("{api_id}/{format}"), key, "")
}

fn count_nodes_with_fqn(g: &strata_core::Graph, kind: NodeKind, fqn: &str) -> usize {
    g.nodes().filter(|n| n.kind == kind && n.fqn == fqn).count()
}

// ── In-memory: a gRPC OperationDef → an ApiOperation node (Extracted 1.0) ─────

#[test]
fn grpc_operation_builds_an_extracted_api_operation_node() {
    // Extract a real proto into ops, then assemble the per-repo plane in memory.
    let proto = std::fs::read_to_string(
        fixture_dir("crossrepo_grpc")
            .join("repo-a")
            .join("service.proto"),
    )
    .expect("read fixture proto");
    let ops = ProtoAdapter
        .extract("service.proto", &proto)
        .expect("proto parses");
    assert_eq!(ops.len(), 1, "one rpc → one op, got {ops:?}");
    assert_eq!(ops[0].format, ContractFormat::Grpc);
    assert_eq!(ops[0].key, "shop.orders.v1.OrderService.GetOrder");

    let analyzed: BTreeMap<String, AnalyzedFile> = BTreeMap::new();
    let g = assemble_graph_with_contracts(&analyzed, "repo-a", &ResolveOptions::default(), &ops);

    // The op node: NodeKind::ApiOperation (a gRPC rpc IS an api operation — no new
    // kind), Extracted, confidence 1.0, fqn = the key.
    let uid = Uid::new(
        "contract",
        "repo-a",
        "service.proto",
        "shop.orders.v1.OrderService.GetOrder",
        "",
    );
    let node = g.get_node(&uid).expect("the gRPC operation node exists");
    assert_eq!(node.kind, NodeKind::ApiOperation, "gRPC rpc → ApiOperation");
    assert_eq!(node.provenance, Provenance::Extracted, "an rpc is a fact");
    assert_eq!(node.confidence.value(), 1.0);
    assert_eq!(node.fqn, "shop.orders.v1.OrderService.GetOrder");

    // M1 substrate: no producer/consumer edges for gRPC (that is M2).
    assert_eq!(
        count_nodes_with_fqn(
            &g,
            NodeKind::ApiOperation,
            "shop.orders.v1.OrderService.GetOrder"
        ),
        1,
        "exactly one gRPC ApiOperation node in this repo"
    );
}

// ── Estate dedup: shared proto across two repos (same api id) → ONE node ──────

#[test]
fn grpc_shared_proto_in_two_repos_collapses_to_one_node() {
    let (tmp, manifest) = prep_estate("crossrepo_grpc");
    let (g, _coverage, results) = link_estate(&manifest, tmp.path());
    assert!(
        results.iter().all(|r| r.ok),
        "all repos load ok: {results:?}"
    );

    // repo-a and repo-b carry a byte-identical `service.proto` AND declare the
    // SAME api id `orders`, so their shared rpc collapses to ONE canonical node.
    assert_eq!(
        count_nodes_with_fqn(
            &g,
            NodeKind::ApiOperation,
            "shop.orders.v1.OrderService.GetOrder"
        ),
        1,
        "the same rpc in two repos that declare the same api id must collapse to ONE \
         canonical ApiOperation (earned by the declaration)"
    );

    // That single node is the api-scoped canonical UID (api id `orders`, grpc).
    let canonical = canonical_uid(
        ESTATE,
        "orders",
        "grpc",
        "shop.orders.v1.OrderService.GetOrder",
    );
    let node = g
        .get_node(&canonical)
        .expect("the canonical gRPC node carries the {api_id}/grpc discriminator");
    assert_eq!(node.kind, NodeKind::ApiOperation);
    assert_eq!(node.provenance, Provenance::Extracted);
    assert_eq!(
        canonical.to_string(),
        format!("contract|{ESTATE}|orders/grpc|shop.orders.v1.OrderService.GetOrder|"),
        "the canonical UID must carry the {{api_id}}/grpc discriminator"
    );
}

// ── Format discriminator: gRPC Query.getOrder ≠ GraphQL Query.getOrder ───────

#[test]
fn grpc_and_graphql_same_key_are_two_distinct_nodes() {
    let (tmp, manifest) = prep_estate("crossrepo_grpc");
    let (g, _coverage, results) = link_estate(&manifest, tmp.path());
    assert!(
        results.iter().all(|r| r.ok),
        "all repos load ok: {results:?}"
    );

    // repo-collide declares BOTH a package-less gRPC service `Query.getOrder` and a
    // GraphQL field `Query.getOrder`. They share the key STRING but differ in
    // format → TWO distinct canonical nodes (different kind, different UID).
    let grpc_node = canonical_uid(ESTATE, "repo-collide", "grpc", "Query.getOrder");
    let gql_node = canonical_uid(ESTATE, "repo-collide", "graphql", "Query.getOrder");
    assert_ne!(
        grpc_node, gql_node,
        "the gRPC and GraphQL nodes must have distinct UIDs (format discriminator)"
    );

    let grpc = g
        .get_node(&grpc_node)
        .expect("the gRPC Query.getOrder node exists");
    let gql = g
        .get_node(&gql_node)
        .expect("the GraphQL Query.getOrder node exists");
    assert_eq!(
        grpc.kind,
        NodeKind::ApiOperation,
        "the gRPC node is an ApiOperation"
    );
    assert_eq!(
        gql.kind,
        NodeKind::GraphqlField,
        "the GraphQL node is a GraphqlField"
    );

    // The same key string `Query.getOrder` is realised by exactly these two nodes —
    // one per format — never merged into one.
    let total = g.nodes().filter(|n| n.fqn == "Query.getOrder").count();
    assert_eq!(
        total, 2,
        "Query.getOrder must be exactly two nodes (one gRPC ApiOperation, one \
         GraphqlField), got {total}"
    );
}

// ── Consistency: docs/accuracy/grpc-linking.md must match the live estate ─────
//
// M1 is operation-EXTRACTION coverage (code linking is M2), so the measured fact
// the report publishes is the estate's contract-node tally over `crossrepo_grpc`,
// not a link count. This test pins the doc's numbers equal to the live graph so
// the report cannot silently drift; regenerate with the `#[ignore]` printer below.

/// The gRPC ApiOperation nodes published in `docs/accuracy/grpc-linking.md` for
/// the `crossrepo_grpc` fixture estate: `shop.orders.v1.OrderService.GetOrder`
/// (repo-a+repo-b deduped to ONE) and the package-less `Query.getOrder` in
/// repo-collide → 2.
const DOC_GRPC_API_OPERATIONS: usize = 2;
/// The GraphqlField nodes in the same estate: repo-collide's `Query.getOrder` —
/// the format-discriminator twin that must NOT merge into the gRPC node → 1.
const DOC_GRAPHQL_FIELDS: usize = 1;

#[test]
fn grpc_report_matches_committed_node_tally() {
    let (tmp, manifest) = prep_estate("crossrepo_grpc");
    let (g, _coverage, results) = link_estate(&manifest, tmp.path());
    assert!(results.iter().all(|r| r.ok), "repos load ok: {results:?}");

    let api_operations = g
        .nodes()
        .filter(|n| n.kind == NodeKind::ApiOperation)
        .count();
    let graphql_fields = g
        .nodes()
        .filter(|n| n.kind == NodeKind::GraphqlField)
        .count();

    assert_eq!(
        api_operations, DOC_GRPC_API_OPERATIONS,
        "grpc-linking.md publishes {DOC_GRPC_API_OPERATIONS} gRPC ApiOperation nodes; \
         live estate has {api_operations} — update the doc + this constant together"
    );
    assert_eq!(
        graphql_fields, DOC_GRAPHQL_FIELDS,
        "grpc-linking.md publishes {DOC_GRAPHQL_FIELDS} GraphqlField node; live estate \
         has {graphql_fields}"
    );
}

/// Regenerate the raw figures for `docs/accuracy/grpc-linking.md`:
/// `cargo test -p strata-index --test grpc_estate print_grpc_estate_tally -- --ignored --nocapture`
#[test]
#[ignore]
fn print_grpc_estate_tally() {
    let (tmp, manifest) = prep_estate("crossrepo_grpc");
    let (g, _coverage, _results) = link_estate(&manifest, tmp.path());
    let grpc_ops = g
        .nodes()
        .filter(|n| n.kind == NodeKind::ApiOperation)
        .count();
    let gql_fields = g
        .nodes()
        .filter(|n| n.kind == NodeKind::GraphqlField)
        .count();
    eprintln!("GRPC_TALLY api_operations={grpc_ops} graphql_fields={gql_fields}");
    for n in g
        .nodes()
        .filter(|n| n.kind == NodeKind::ApiOperation || n.kind == NodeKind::GraphqlField)
    {
        eprintln!("  NODE kind={:?} uid={} fqn={}", n.kind, n.uid, n.fqn);
    }
}
