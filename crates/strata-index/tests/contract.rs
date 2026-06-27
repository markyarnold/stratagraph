//! Contract-plane producer-linking tests (Slice 3, M2 — Definition of Done
//! tests 8–10, 12). Test 11 (the band invariant over `Produces`) lives in
//! `tests/confidence_bands.rs`.
//!
//! Each test copies a committed fixture repo to a tempdir before indexing, so a
//! `.strata/` graph store is never written into the source tree (same hygiene as
//! the estate tests). Indexing runs with `ResolveMode::Off` — the contract plane
//! is independent of SCIP, and the heuristic code plane is enough to create the
//! handler function nodes the producer edges attach to.

use std::path::{Path, PathBuf};

use strata_core::{Direction, EdgeKind, NodeKind, Provenance, Uid};
use strata_index::{index_repo_with_options, IndexOptions, ResolveMode};
use strata_store::{DuckGraphStore, GraphStore};

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

/// Copy `fixtures/<name>` to a tempdir, index it (ResolveMode::Off), and return
/// (the loaded graph, the repo_name used for UIDs).
fn index_fixture(name: &str) -> (strata_core::Graph, String, tempfile::TempDir) {
    let tmp = tempfile::tempdir().expect("tempdir");
    copy_dir_all(&fixture_dir(name), tmp.path()).expect("copy fixture");
    let repo_name = tmp
        .path()
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let mut store = DuckGraphStore::open_in_memory().expect("store");
    let options = IndexOptions {
        resolve_mode: ResolveMode::Off,
        allow_install: false,
        include_vendored: false,
    };
    index_repo_with_options(tmp.path(), &mut store, &options).expect("index");
    let g = store.load_graph().expect("load graph");
    (g, repo_name, tmp)
}

/// The `ApiOperation` node UID for `key` in spec file `spec_rel`.
fn op_uid(repo: &str, spec_rel: &str, key: &str) -> Uid {
    Uid::new("contract", repo, spec_rel, key, "")
}

/// The code-plane function-symbol UID for `fqn` in file `path`.
fn fn_uid(repo: &str, path: &str, fqn: &str) -> Uid {
    Uid::new("ts", repo, path, fqn, "")
}

/// All outgoing `Produces` edges from `src`, as (dst, provenance, confidence).
fn produces_edges(g: &strata_core::Graph, src: &Uid) -> Vec<(Uid, Provenance, f32)> {
    g.neighbors(src, Direction::Outgoing, &[EdgeKind::Produces])
        .into_iter()
        .map(|(e, _)| (e.dst.clone(), e.provenance, e.confidence.value()))
        .collect()
}

/// All outgoing `Consumes` edges from `src`, as (dst, provenance, confidence).
fn consumes_edges(g: &strata_core::Graph, src: &Uid) -> Vec<(Uid, Provenance, f32)> {
    g.neighbors(src, Direction::Outgoing, &[EdgeKind::Consumes])
        .into_iter()
        .map(|(e, _)| (e.dst.clone(), e.provenance, e.confidence.value()))
        .collect()
}

// ── Test 8: producer linking — single match → Inferred PRODUCES ─────────────

#[test]
fn producer_links_route_to_operation_inferred() {
    let (g, repo, _tmp) = index_fixture("producer");

    // The ApiOperation node for getUser exists, Extracted 1.0, with method/path.
    let get_user_op = op_uid(&repo, "openapi.yaml", "getUser");
    let node = g
        .get_node(&get_user_op)
        .unwrap_or_else(|| panic!("getUser ApiOperation node missing"));
    assert_eq!(node.kind, NodeKind::ApiOperation);
    assert_eq!(node.provenance, Provenance::Extracted);
    assert_eq!(node.confidence.value(), 1.0);
    assert_eq!(node.name, "getUser");
    assert_eq!(node.path, "/users/{id}"); // raw HTTP path stored on the node

    // The getUser function node -> getUser operation, PRODUCES, Inferred 0.80.
    let get_user_fn = fn_uid(&repo, "src/routes.ts", "getUser");
    let edges = produces_edges(&g, &get_user_fn);
    assert_eq!(
        edges.len(),
        1,
        "exactly one PRODUCES edge from getUser fn, got {edges:?}"
    );
    let (dst, prov, conf) = &edges[0];
    assert_eq!(*dst, get_user_op);
    assert_eq!(*prov, Provenance::Inferred);
    assert_eq!(*conf, 0.80);
}

// ── Test 9: ambiguous producer — multiple matches → Ambiguous ───────────────

#[test]
fn ambiguous_producer_emits_ambiguous_edges_not_one_confident() {
    let (g, repo, _tmp) = index_fixture("producer");

    // GET /things/{id} is declared in BOTH things-v1.yaml and things-v2.yaml, so
    // the single route app.get("/things/:id", getThing) matches two operations.
    let get_thing_fn = fn_uid(&repo, "src/routes.ts", "getThing");
    let edges = produces_edges(&g, &get_thing_fn);

    assert_eq!(
        edges.len(),
        2,
        "two Ambiguous PRODUCES edges (one per candidate), got {edges:?}"
    );
    for (_dst, prov, conf) in &edges {
        assert_eq!(
            *prov,
            Provenance::Ambiguous,
            "a multi-candidate producer link must be Ambiguous, never a single confident edge"
        );
        assert!(
            *conf < 0.40,
            "Ambiguous confidence must be < 0.40, got {conf}"
        );
    }

    // Both candidate operations are the edge targets.
    let v1 = op_uid(&repo, "things-v1.yaml", "getThingV1");
    let v2 = op_uid(&repo, "things-v2.yaml", "getThingV2");
    let dsts: Vec<&Uid> = edges.iter().map(|(d, _, _)| d).collect();
    assert!(dsts.contains(&&v1), "edge to getThingV1 expected: {dsts:?}");
    assert!(dsts.contains(&&v2), "edge to getThingV2 expected: {dsts:?}");
}

// ── Test 10: no spurious link — a route matching no operation → no edge ──────

#[test]
fn route_matching_no_operation_produces_no_edge() {
    let (g, repo, _tmp) = index_fixture("producer");

    // app.delete("/nonexistent", removeIt) matches no operation in any spec.
    let remove_fn = fn_uid(&repo, "src/routes.ts", "removeIt");
    let edges = produces_edges(&g, &remove_fn);
    assert!(
        edges.is_empty(),
        "a route with no matching operation must produce NO edge, got {edges:?}"
    );

    // And there is no DELETE /nonexistent operation node invented anywhere.
    let invented = op_uid(&repo, "openapi.yaml", "DELETE /nonexistent");
    assert!(
        g.get_node(&invented).is_none(),
        "no operation node should be invented for an unmatched route"
    );
}

// ── Test 12: R2 — a malformed spec still indexes (code graph intact) ─────────

#[test]
fn malformed_spec_does_not_break_indexing() {
    // openapi.yaml is malformed YAML; valid.yaml is a healthy spec; src/app.ts
    // is ordinary code. Indexing must not panic; the code graph and the valid
    // spec's operation survive; the malformed spec is skipped.
    let (g, repo, _tmp) = index_fixture("malformed_spec");

    // Code plane intact: the cross-function CALLS edge helper -> compute exists.
    let helper = fn_uid(&repo, "src/app.ts", "helper");
    let compute = fn_uid(&repo, "src/app.ts", "compute");
    let calls: Vec<Uid> = g
        .neighbors(&helper, Direction::Outgoing, &[EdgeKind::Calls])
        .into_iter()
        .map(|(e, _)| e.dst.clone())
        .collect();
    assert!(
        calls.contains(&compute),
        "code graph must be intact despite the malformed spec; got {calls:?}"
    );

    // The valid spec's operation is present.
    let health = op_uid(&repo, "valid.yaml", "getHealth");
    assert!(
        g.get_node(&health).is_some(),
        "the healthy spec must still yield its ApiOperation"
    );

    // The malformed spec invented no operation node (getUser came only from it).
    let from_malformed = op_uid(&repo, "openapi.yaml", "getUser");
    assert!(
        g.get_node(&from_malformed).is_none(),
        "the malformed spec must be skipped, not partially extracted"
    );
}

// ── Per-repo consumer linking (brief §3, the monolith case) ──────────────────
//
// A repo with a spec AND its own callers: a literal-URL `fetch("/users/123")`
// and an operationId-name `getUser(...)` call both link (CONSUMES) to the local
// getUser operation; a `fetch("/widgets/9")` to an undeclared endpoint links to
// nothing. This is the same `match_consumer` the estate pass uses, run against
// the repo's LOCAL operations.

#[test]
fn per_repo_consumer_links_literal_and_operation_id_to_local_operation() {
    let (g, repo, _tmp) = index_fixture("monolith");

    let get_user_op = op_uid(&repo, "openapi.yaml", "getUser");
    assert!(
        g.get_node(&get_user_op).is_some(),
        "local getUser operation node must exist"
    );

    // Literal URL consumer: loadUser fetch("/users/123") → Inferred 0.70.
    let load_user = fn_uid(&repo, "src/client.ts", "loadUser");
    let edges = consumes_edges(&g, &load_user);
    assert_eq!(
        edges.len(),
        1,
        "one CONSUMES edge from loadUser (literal URL), got {edges:?}"
    );
    let (dst, prov, conf) = &edges[0];
    assert_eq!(*dst, get_user_op);
    assert_eq!(*prov, Provenance::Inferred);
    assert_eq!(*conf, 0.70);

    // operationId-name consumer: loadUserByName getUser(...) → Inferred 0.75.
    let load_by_name = fn_uid(&repo, "src/client.ts", "loadUserByName");
    let edges = consumes_edges(&g, &load_by_name);
    assert_eq!(
        edges.len(),
        1,
        "one CONSUMES edge from loadUserByName (operationId name), got {edges:?}"
    );
    let (dst, prov, conf) = &edges[0];
    assert_eq!(*dst, get_user_op);
    assert_eq!(*prov, Provenance::Inferred);
    assert_eq!(*conf, 0.75);
}

#[test]
fn per_repo_consumer_to_undeclared_endpoint_creates_no_edge() {
    let (g, repo, _tmp) = index_fixture("monolith");

    // loadWidget fetch("/widgets/9") matches no operation → no CONSUMES edge.
    let load_widget = fn_uid(&repo, "src/client.ts", "loadWidget");
    let edges = consumes_edges(&g, &load_widget);
    assert!(
        edges.is_empty(),
        "a consumer of an undeclared endpoint must produce NO edge, got {edges:?}"
    );
}

// ── Python contract linking, end-to-end ─────────────────────────────────────
//
// The contract linker is language-agnostic: it consumes the AnalyzedFile contract
// signals the Python analyzer now emits and attaches edges to the `py` code nodes
// the Python plane built (the same producer/consumer banding as TS/JS). Fixture
// `py_contract` = app.py (a route, a requests call, a gql doc, a Graphene resolver)
// + openapi.yaml + schema.graphql.

/// The Python code-plane function/method-symbol UID for `fqn` in file `path`.
fn py_fn_uid(repo: &str, path: &str, fqn: &str) -> Uid {
    Uid::new("py", repo, path, fqn, "")
}

#[test]
fn python_route_produces_openapi_operation() {
    let (g, repo, _tmp) = index_fixture("py_contract");
    // @app.get("/users/{id}") on get_user → GET /users/{id} (getUser), Inferred 0.80.
    let op = op_uid(&repo, "openapi.yaml", "getUser");
    let handler = py_fn_uid(&repo, "app.py", "get_user");
    let edges = produces_edges(&g, &handler);
    assert_eq!(edges.len(), 1, "one PRODUCES from get_user, got {edges:?}");
    let (dst, prov, conf) = &edges[0];
    assert_eq!(*dst, op);
    assert_eq!(*prov, Provenance::Inferred);
    assert_eq!(*conf, 0.80);
}

#[test]
fn python_requests_consumes_openapi_operation() {
    let (g, repo, _tmp) = index_fixture("py_contract");
    // requests.get("/widgets/1") in fetch_widget → GET /widgets/{id} (getWidget),
    // a literal-URL consumer link, Inferred 0.70.
    let op = op_uid(&repo, "openapi.yaml", "getWidget");
    let caller = py_fn_uid(&repo, "app.py", "fetch_widget");
    let edges = consumes_edges(&g, &caller);
    assert!(
        edges
            .iter()
            .any(|(d, p, c)| *d == op && *p == Provenance::Inferred && *c == 0.70),
        "fetch_widget CONSUMES getWidget at Inferred 0.70, got {edges:?}"
    );
}

#[test]
fn python_gql_document_consumes_graphql_field() {
    let (g, repo, _tmp) = index_fixture("py_contract");
    // gql("query { getUser { id } }") in load_user → Query.getUser, Extracted 0.95.
    let field = op_uid(&repo, "schema.graphql", "Query.getUser");
    let caller = py_fn_uid(&repo, "app.py", "load_user");
    let edges = consumes_edges(&g, &caller);
    assert!(
        edges
            .iter()
            .any(|(d, p, c)| *d == field && *p == Provenance::Extracted && *c == 0.95),
        "load_user CONSUMES Query.getUser at Extracted 0.95, got {edges:?}"
    );
}

#[test]
fn python_graphene_resolver_produces_graphql_field() {
    let (g, repo, _tmp) = index_fixture("py_contract");
    // class Query(graphene.ObjectType).resolve_getUser → Query.getUser, Inferred 0.80.
    let field = op_uid(&repo, "schema.graphql", "Query.getUser");
    let resolver = py_fn_uid(&repo, "app.py", "Query.resolve_getUser");
    let edges = produces_edges(&g, &resolver);
    assert!(
        edges
            .iter()
            .any(|(d, p, c)| *d == field && *p == Provenance::Inferred && *c == 0.80),
        "resolve_getUser PRODUCES Query.getUser at Inferred 0.80, got {edges:?}"
    );
}

#[test]
fn python_django_route_path_only_is_inferred_065() {
    let (g, repo, _tmp) = index_fixture("py_contract");
    // path("health/", health_check) is method-less (Django dispatches internally),
    // so it matches GET /health on PATH ALONE -> a weaker Inferred tier (0.65) than
    // a method+path link, never claiming a method the view may not implement.
    // (Also proves the Django leading/trailing-slash canonicalisation: the pattern
    // "health/" must match the OpenAPI "/health".)
    let op = op_uid(&repo, "openapi.yaml", "getHealth");
    let handler = py_fn_uid(&repo, "app.py", "health_check");
    let edges = produces_edges(&g, &handler);
    assert_eq!(
        edges.len(),
        1,
        "one path-only PRODUCES from health_check, got {edges:?}"
    );
    let (dst, prov, conf) = &edges[0];
    assert_eq!(*dst, op);
    assert_eq!(*prov, Provenance::Inferred);
    assert_eq!(*conf, 0.65);
}

#[test]
fn python_django_route_multi_match_is_ambiguous() {
    let (g, repo, _tmp) = index_fixture("py_contract");
    // path("things/<int:pk>/", thing_detail) matches BOTH GET and DELETE
    // /things/{pk} at the same path -> method-less + multi -> Ambiguous 0.35 each,
    // never one confident method.
    let handler = py_fn_uid(&repo, "app.py", "thing_detail");
    let edges = produces_edges(&g, &handler);
    assert_eq!(
        edges.len(),
        2,
        "two Ambiguous edges (one per op at the path), got {edges:?}"
    );
    for (_dst, prov, conf) in &edges {
        assert_eq!(*prov, Provenance::Ambiguous);
        assert!(
            *conf < 0.40,
            "Ambiguous confidence must be < 0.40, got {conf}"
        );
    }
}
