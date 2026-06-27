//! GraphQL estate link-pass tests (Slice 4, M2 — Definition of Done tests 7, 8,
//! 9, 10, 11). The headline cross-repo *impact* test (6) lives in
//! `tests/graphql_crossrepo_impact.rs`; the coverage report/floor tests (12) in
//! `tests/graphql_coverage.rs`.
//!
//! Every test copies the committed `crossrepo_graphql` fixture estate to a
//! tempdir, indexes each repo (`ResolveMode::Off` — no Node/SCIP), then calls
//! `link_estate`. `.strata/` dirs are only ever created inside the tempdir.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use strata_core::{Direction, EdgeKind, NodeKind, Provenance, Uid};
use strata_index::{index_estate, link_estate, ResolveMode, WorkspaceManifest};

const ESTATE: &str = "gql-estate";

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

/// The canonical (estate-wide) GraphQL field UID for `key` owned by api `api_id`:
/// the `{api_id}/graphql` discriminator in the spec-path slot (B6 api-scoped
/// identity). The default `api_id` is the repo name of the schema's repo.
fn canonical_gql_uid(estate: &str, api_id: &str, key: &str) -> Uid {
    Uid::new("contract", estate, &format!("{api_id}/graphql"), key, "")
}

/// The canonical OpenAPI operation UID for `key` owned by api `api_id`: the
/// `{api_id}/openapi` discriminator (B6 api-scoped identity).
fn canonical_openapi_uid(estate: &str, api_id: &str, key: &str) -> Uid {
    Uid::new("contract", estate, &format!("{api_id}/openapi"), key, "")
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

fn count_nodes_with_fqn(g: &strata_core::Graph, kind: NodeKind, fqn: &str) -> usize {
    g.nodes().filter(|n| n.kind == kind && n.fqn == fqn).count()
}

// ── Test 7: the `.graphql` operation file links (CONSUMES, cross-repo) ───────

#[test]
fn graphql_operation_file_links_cross_repo_from_its_module() {
    let (tmp, manifest) = prep_estate("crossrepo_graphql");
    let (g, _coverage, results) = link_estate(&manifest, tmp.path());
    assert!(
        results.iter().all(|r| r.ok),
        "both repos load ok: {results:?}"
    );

    // src/orders.graphql is an operation document → a Module node that CONSUMES
    // the canonical Query.listUsers in the OTHER repo (cross-repo).
    let orders_module = module_uid("repo-app", "src/orders.graphql");
    assert!(
        g.get_node(&orders_module).is_some(),
        "the .graphql operation file must get a Module node"
    );
    let list_users = canonical_gql_uid(ESTATE, "repo-schema", "Query.listUsers");
    let consumes = edges_of(&g, &orders_module, EdgeKind::Consumes);
    assert!(
        consumes.iter().any(|(dst, prov, conf)| *dst == list_users
            && *prov == Provenance::Extracted
            && (*conf - 0.95).abs() < 1e-6),
        "orders.graphql module must CONSUMES Query.listUsers (Extracted 0.95) cross-repo: {consumes:?}"
    );
}

// ── Test 8: no-false-edge — unknown field + interpolated template ────────────

#[test]
fn graphql_unknown_field_and_interpolated_template_make_no_edge() {
    let (tmp, manifest) = prep_estate("crossrepo_graphql");
    let (g, _coverage, _results) = link_estate(&manifest, tmp.path());

    // unknown.ts queries `nonExistentField` — no schema declares it → no CONSUMES.
    let unknown = fn_uid("repo-app", "src/unknown.ts", "loadUnknown");
    assert!(
        edges_of(&g, &unknown, EdgeKind::Consumes).is_empty(),
        "a query for an undeclared field must create NO CONSUMES edge"
    );
    // No operation node was invented for the unknown field, in either format.
    assert_eq!(
        count_nodes_with_fqn(&g, NodeKind::GraphqlField, "Query.nonExistentField"),
        0,
        "no invented GraphqlField for the unknown field"
    );

    // broken.ts's interpolated `gql` template must produce no link from its
    // enclosing function either.
    let broken = fn_uid("repo-app", "src/broken.ts", "loadComposed");
    assert!(
        edges_of(&g, &broken, EdgeKind::Consumes).is_empty(),
        "an interpolated gql template must produce NO CONSUMES edge (counted unparsed)"
    );
}

// ── Test 9: (format, key) dedup + synthetic OpenAPI non-merge ────────────────

#[test]
fn graphql_dedup_by_format_key_and_no_openapi_merge() {
    let (tmp, manifest) = prep_estate("crossrepo_graphql");
    let (g, _coverage, results) = link_estate(&manifest, tmp.path());
    assert!(results.iter().all(|r| r.ok), "both repos load ok");

    // The schema is in repo-schema only here, so its canonical node is keyed by
    // (api_id, format, key) with api_id = repo name `repo-schema`, and is unique.
    // Here we pin the single canonical GraphqlField node and that its UID carries
    // the `{api_id}/graphql` discriminator.
    let canonical = canonical_gql_uid(ESTATE, "repo-schema", "Query.getUser");
    let node = g
        .get_node(&canonical)
        .unwrap_or_else(|| panic!("canonical Query.getUser GraphqlField node missing"));
    assert_eq!(node.kind, NodeKind::GraphqlField);
    assert_eq!(
        count_nodes_with_fqn(&g, NodeKind::GraphqlField, "Query.getUser"),
        1,
        "Query.getUser must collapse to ONE canonical GraphqlField node"
    );

    // SYNTHETIC NON-MERGE: an OpenAPI op whose key string equals a GraphQL key
    // would get a DIFFERENT canonical UID (the format part of the discriminator
    // differs: `graphql` vs `openapi`), so the two can never merge — even within
    // the same api id. We assert the UID *formulas* differ (B6 api-scoped form).
    let gql_uid = canonical_gql_uid(ESTATE, "repo-schema", "Query.getUser");
    let openapi_uid = canonical_openapi_uid(ESTATE, "repo-schema", "Query.getUser");
    assert_ne!(
        gql_uid, openapi_uid,
        "a GraphQL key and an OpenAPI op of the same key string must have DISTINCT \
         canonical UIDs (the format part of the (api_id, format, key) identity)"
    );
    // The api-scoped UID anatomy: contract | estate | {api_id}/{format} | key |.
    assert_eq!(
        openapi_uid.as_str(),
        format!("contract|{ESTATE}|repo-schema/openapi|Query.getUser|"),
        "OpenAPI canonical UID must carry the {{api_id}}/openapi discriminator"
    );
    assert_eq!(
        gql_uid.as_str(),
        format!("contract|{ESTATE}|repo-schema/graphql|Query.getUser|"),
        "GraphQL canonical UID must carry the {{api_id}}/graphql discriminator"
    );
}

// ── Test 9 (cont.): same Query.getUser in TWO repos that DECLARE a shared api id
// → ONE node (the opt-in MERGE, B6 fix) ──────────────────────────────────────

#[test]
fn graphql_same_field_in_two_repos_collapses_to_one_node() {
    // The `dedup_graphql` fixture's manifest now POSITIVELY declares the same api
    // id `user` in both repo-x and repo-y (the explicit opt-in). Only because of
    // that declaration does the shared `Query.getUser` key collapse to one node.
    let (tmp, manifest) = prep_estate("dedup_graphql");
    let (g, _coverage, results) = link_estate(&manifest, tmp.path());
    assert!(
        results.iter().all(|r| r.ok),
        "both repos load ok: {results:?}"
    );

    // getUser declared in BOTH repos under the SAME api id `user` → ONE node.
    assert_eq!(
        count_nodes_with_fqn(&g, NodeKind::GraphqlField, "Query.getUser"),
        1,
        "the same field in two repos that declare the same api id must collapse to \
         ONE canonical GraphqlField (earned by the declaration)"
    );

    // Both resolver producers point at that single canonical node (api id `user`).
    let canonical = canonical_gql_uid("dedup-gql-estate", "user", "Query.getUser");
    let x_fn = fn_uid("repo-x", "src/x.ts", "getUser");
    let y_fn = fn_uid("repo-y", "src/y.ts", "getUser");
    assert!(
        edges_of(&g, &x_fn, EdgeKind::Produces)
            .iter()
            .any(|(d, _, _)| *d == canonical),
        "repo-x resolver produces the canonical node"
    );
    assert!(
        edges_of(&g, &y_fn, EdgeKind::Produces)
            .iter()
            .any(|(d, _, _)| *d == canonical),
        "repo-y resolver produces the SAME canonical node"
    );
}

// ── Test 9 (sibling): WITHOUT the api declaration, the same field namespaces
// apart into two nodes — repos never merge a shared key by default (B6) ──────

#[test]
fn graphql_same_field_in_two_repos_does_not_merge_without_api_declaration() {
    // `dedup_graphql_undeclared` is byte-identical to `dedup_graphql` but its v1
    // manifest declares NO api id. The default api_id is the repo name, so the two
    // `Query.getUser` schemas namespace apart — TWO canonical nodes, never merged.
    let (tmp, manifest) = prep_estate("dedup_graphql_undeclared");
    let (g, _coverage, results) = link_estate(&manifest, tmp.path());
    assert!(
        results.iter().all(|r| r.ok),
        "both repos load ok: {results:?}"
    );

    assert_eq!(
        count_nodes_with_fqn(&g, NodeKind::GraphqlField, "Query.getUser"),
        2,
        "without a declared shared api id, the same field in two repos must stay as \
         TWO distinct canonical GraphqlField nodes (the safe default)"
    );

    // Each resolver producer points at its OWN repo-scoped canonical node.
    let estate = "dedup-gql-undeclared-estate";
    let x_op = canonical_gql_uid(estate, "repo-x", "Query.getUser");
    let y_op = canonical_gql_uid(estate, "repo-y", "Query.getUser");
    assert_ne!(x_op, y_op, "the two api nodes must be distinct");
    let x_fn = fn_uid("repo-x", "src/x.ts", "getUser");
    let y_fn = fn_uid("repo-y", "src/y.ts", "getUser");
    assert!(
        edges_of(&g, &x_fn, EdgeKind::Produces)
            .iter()
            .any(|(d, _, _)| *d == x_op),
        "repo-x resolver produces repo-x's own canonical node"
    );
    assert!(
        edges_of(&g, &y_fn, EdgeKind::Produces)
            .iter()
            .any(|(d, _, _)| *d == y_op),
        "repo-y resolver produces repo-y's own (distinct) canonical node"
    );
}

// ── Test 10: determinism — link_estate twice → identical estate graph ────────

#[test]
fn graphql_link_estate_is_deterministic() {
    let (tmp, manifest) = prep_estate("crossrepo_graphql");

    let (g1, cov1, _) = link_estate(&manifest, tmp.path());
    let (g2, cov2, _) = link_estate(&manifest, tmp.path());

    assert_eq!(g1.node_count(), g2.node_count(), "node counts match");
    assert_eq!(g1.edge_count(), g2.edge_count(), "edge counts match");
    assert_eq!(cov1, cov2, "coverage is identical across runs");

    let nodes1: BTreeSet<String> = g1.nodes().map(|n| n.uid.to_string()).collect();
    let nodes2: BTreeSet<String> = g2.nodes().map(|n| n.uid.to_string()).collect();
    assert_eq!(nodes1, nodes2, "node UID sets match");

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

// ── Test 11: R2 — broken schema repo still contributes code + valid links; an
// empty/comment-only `.graphql` is a benign skip ────────────────────────────

#[test]
fn graphql_r2_degradation_and_empty_doc_benign_skip() {
    // The fixture's repo-app contains `src/empty.graphql` (comments only). It must
    // not break indexing/linking, and must yield no links. The valid cross-repo
    // links and the producer code plane must be intact.
    let (tmp, manifest) = prep_estate("crossrepo_graphql");
    let (g, _coverage, results) = link_estate(&manifest, tmp.path());
    assert!(
        results.iter().all(|r| r.ok),
        "both repos load ok despite the empty.graphql: {results:?}"
    );

    // The empty doc's Module node exists but has no CONSUMES edge.
    let empty_module = module_uid("repo-app", "src/empty.graphql");
    assert!(
        g.get_node(&empty_module).is_some(),
        "the comment-only .graphql still gets a Module node"
    );
    assert!(
        edges_of(&g, &empty_module, EdgeKind::Consumes).is_empty(),
        "a comment-only .graphql yields no links (benign skip)"
    );

    // The producer code plane is intact: the resolver handler node exists.
    let handler = fn_uid("repo-schema", "src/resolvers.ts", "getUser");
    assert!(
        g.get_node(&handler).is_some(),
        "producer code plane must be intact"
    );

    // The valid cross-repo consumer link still formed.
    let consumer = fn_uid("repo-app", "src/queries.ts", "loadUserProfile");
    let canonical = canonical_gql_uid(ESTATE, "repo-schema", "Query.getUser");
    assert!(
        edges_of(&g, &consumer, EdgeKind::Consumes)
            .iter()
            .any(|(d, _, _)| *d == canonical),
        "the valid gql consumer link must be unaffected by the empty doc"
    );
}
