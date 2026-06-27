//! OpenAPI adapter tests (Slice 3, Milestone 2 — Definition of Done tests 1–6).
//!
//! Test 4 (`normalize_path`) is a pure unit test colocated in `src/lib.rs`.
//! These integration tests cover extraction (1–3), graceful degradation (5),
//! and detection (6) against committed spec fixtures.

use strata_contract::{
    ContractAdapter, ContractError, ContractFormat, OpenApiAdapter, OperationDef,
};

fn fixture(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read fixture {name}: {e}"))
}

/// Look up the single operation with `key` (panics if absent), for per-field
/// assertions independent of vector ordering.
fn op_by_key<'a>(ops: &'a [OperationDef], key: &str) -> &'a OperationDef {
    ops.iter()
        .find(|o| o.key == key)
        .unwrap_or_else(|| panic!("no operation with key {key:?}; got {ops:?}"))
}

// ── Test 1: OpenAPI v3 YAML, two paths × methods + operationIds ─────────────

#[test]
fn extract_openapi_v3_yaml() {
    let spec = fixture("openapi_v3.yaml");
    let ops = OpenApiAdapter
        .extract("openapi.yaml", &spec)
        .expect("v3 yaml parses");

    // Four operations: GET+PUT /users/{id}, GET+POST /users.
    assert_eq!(ops.len(), 4, "expected 4 operations, got {ops:?}");

    let get_user = op_by_key(&ops, "getUser");
    assert_eq!(
        *get_user,
        OperationDef {
            format: ContractFormat::OpenApi,
            key: "getUser".into(),
            method: "GET".into(),
            path: "/users/{id}".into(),
            norm_path: "/users/{}".into(),
            operation_id: Some("getUser".into()),
            spec_path: "openapi.yaml".into(),
        }
    );

    let update_user = op_by_key(&ops, "updateUser");
    assert_eq!(update_user.method, "PUT");
    assert_eq!(update_user.path, "/users/{id}");
    assert_eq!(update_user.norm_path, "/users/{}");

    let list_users = op_by_key(&ops, "listUsers");
    assert_eq!(list_users.method, "GET");
    assert_eq!(list_users.norm_path, "/users");

    let create_user = op_by_key(&ops, "createUser");
    assert_eq!(create_user.method, "POST");
    assert_eq!(create_user.path, "/users");
    assert_eq!(create_user.operation_id.as_deref(), Some("createUser"));
}

// ── Test 2: Swagger v2 JSON, version-agnostic same extraction ───────────────

#[test]
fn extract_swagger_v2_json() {
    let spec = fixture("swagger_v2.json");
    let ops = OpenApiAdapter
        .extract("swagger.json", &spec)
        .expect("v2 json parses");

    assert_eq!(ops.len(), 3, "expected 3 operations, got {ops:?}");

    let get_order = op_by_key(&ops, "getOrder");
    assert_eq!(get_order.method, "GET");
    assert_eq!(get_order.path, "/orders/{orderId}");
    assert_eq!(get_order.norm_path, "/orders/{}");
    assert_eq!(get_order.operation_id.as_deref(), Some("getOrder"));

    let delete_order = op_by_key(&ops, "deleteOrder");
    assert_eq!(delete_order.method, "DELETE");
    assert_eq!(delete_order.norm_path, "/orders/{}");

    let create_order = op_by_key(&ops, "createOrder");
    assert_eq!(create_order.method, "POST");
    assert_eq!(create_order.norm_path, "/orders");
}

// ── Test 3: a path without operationId → synthesized key ────────────────────

#[test]
fn missing_operation_id_synthesizes_key() {
    let spec = fixture("no_operation_id.yaml");
    let ops = OpenApiAdapter
        .extract("openapi.yaml", &spec)
        .expect("parses");

    assert_eq!(ops.len(), 1, "expected 1 operation, got {ops:?}");
    let op = &ops[0];
    assert_eq!(
        op.key, "GET /users/{}",
        "key is synthesized as METHOD normalized-path when operationId is absent"
    );
    assert_eq!(op.method, "GET");
    assert_eq!(op.path, "/users/{id}");
    assert_eq!(op.norm_path, "/users/{}");
    assert_eq!(op.operation_id, None);
}

// ── Test 5: malformed spec text → ContractError::Parse, no panic ────────────

#[test]
fn malformed_spec_returns_parse_error() {
    // Neither valid JSON nor valid YAML.
    let bad = "paths: [this: is: not: valid: yaml";
    let result = OpenApiAdapter.extract("broken.yaml", bad);
    match result {
        Err(ContractError::Parse { spec, .. }) => assert_eq!(spec, "broken.yaml"),
        other => panic!("expected ContractError::Parse, got {other:?}"),
    }
}

#[test]
fn well_formed_but_no_paths_is_a_parse_error() {
    // Valid YAML, but not a usable spec (no `paths`): degrade visibly, no panic.
    let no_paths = "openapi: 3.0.3\ninfo:\n  title: x\n  version: '1'\n";
    let result = OpenApiAdapter.extract("nopaths.yaml", no_paths);
    assert!(
        matches!(result, Err(ContractError::Parse { .. })),
        "a spec without `paths` is a structural parse error, got {result:?}"
    );
}

// ── Test 6: detects true for specs, false for arbitrary docs ────────────────

#[test]
fn detects_openapi_and_swagger_specs() {
    let v3 = fixture("openapi_v3.yaml");
    let v2 = fixture("swagger_v2.json");

    // True by filename.
    assert!(OpenApiAdapter.detects("openapi.yaml", &v3));
    assert!(OpenApiAdapter.detects("swagger.json", &v2));
    // True by content even under a non-spec filename.
    assert!(OpenApiAdapter.detects("api/spec-A.yaml", &v3));
}

#[test]
fn does_not_detect_arbitrary_documents() {
    // Arbitrary JSON: no openapi/swagger version key, no paths.
    let arbitrary_json = r#"{ "name": "package", "version": "1.0.0", "scripts": {} }"#;
    assert!(!OpenApiAdapter.detects("package.json", arbitrary_json));

    // Arbitrary YAML config.
    let arbitrary_yaml = "name: ci\non: push\njobs:\n  build:\n    runs-on: ubuntu\n";
    assert!(!OpenApiAdapter.detects("workflow.yaml", arbitrary_yaml));

    // A YAML doc that has `paths` but no version key is NOT an OpenAPI spec.
    let paths_no_version = "paths:\n  /x:\n    get: {}\n";
    assert!(!OpenApiAdapter.detects("random.yaml", paths_no_version));
}
