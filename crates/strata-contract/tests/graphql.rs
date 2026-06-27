//! GraphQL adapter tests (Slice 4, Milestone 1 — Definition of Done tests 1–5).
//!
//! Extraction of SDL into canonical root-operation-field `OperationDef`s
//! (default + renamed roots), AppSync directive tolerance, graceful degradation
//! on malformed SDL, and `detects` discrimination (schema vs operations vs
//! arbitrary text), all against committed `.graphql` fixtures.

use strata_contract::{
    ContractAdapter, ContractError, ContractFormat, GraphqlAdapter, OperationDef,
};

fn fixture(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read fixture {name}: {e}"))
}

/// Look up the single operation with `key` (panics if absent).
fn op_by_key<'a>(ops: &'a [OperationDef], key: &str) -> &'a OperationDef {
    ops.iter()
        .find(|o| o.key == key)
        .unwrap_or_else(|| panic!("no operation with key {key:?}; got {ops:?}"))
}

// ── Test 1: default roots → exactly the root-type fields, format Graphql ─────

#[test]
fn extract_schema_basic_default_roots() {
    let schema = fixture("schema_basic.graphql");
    let ops = GraphqlAdapter
        .extract("schema.graphql", &schema)
        .expect("basic SDL parses");

    // Exactly the three root-operation fields; non-root types (User, UserInput)
    // contribute nothing.
    assert_eq!(
        ops.len(),
        3,
        "expected 3 root-field operations, got {ops:?}"
    );

    let get_user = op_by_key(&ops, "Query.getUser");
    assert_eq!(
        *get_user,
        OperationDef {
            format: ContractFormat::Graphql,
            key: "Query.getUser".into(),
            method: "QUERY".into(),
            path: "getUser".into(),
            norm_path: "getUser".into(),
            operation_id: None,
            spec_path: "schema.graphql".into(),
        }
    );

    let list_users = op_by_key(&ops, "Query.listUsers");
    assert_eq!(list_users.method, "QUERY");
    assert_eq!(list_users.path, "listUsers");
    assert_eq!(list_users.format, ContractFormat::Graphql);

    let create_user = op_by_key(&ops, "Mutation.createUser");
    assert_eq!(create_user.method, "MUTATION");
    assert_eq!(create_user.path, "createUser");
    assert_eq!(create_user.operation_id, None);

    // No non-root type leaked in as an operation.
    assert!(
        ops.iter()
            .all(|o| o.key.starts_with("Query.") || o.key.starts_with("Mutation.")),
        "only Query./Mutation. keys expected, got {ops:?}"
    );
}

// ── Test 2: renamed roots normalize to canonical Query./Mutation. ────────────

#[test]
fn renamed_roots_normalize_to_canonical_prefixes() {
    let schema = fixture("schema_renamed_roots.graphql");
    let ops = GraphqlAdapter
        .extract("schema.graphql", &schema)
        .expect("renamed-roots SDL parses");

    assert_eq!(ops.len(), 2, "expected 2 operations, got {ops:?}");

    // The KEY must be the canonical operation kind, NOT the renamed root type.
    let ping = op_by_key(&ops, "Query.ping");
    assert_eq!(ping.method, "QUERY");
    assert_eq!(ping.path, "ping");

    let do_it = op_by_key(&ops, "Mutation.doIt");
    assert_eq!(do_it.method, "MUTATION");
    assert_eq!(do_it.path, "doIt");

    // The renamed root names must NOT appear in any key.
    assert!(
        !ops.iter()
            .any(|o| o.key.starts_with("RootQ.") || o.key.starts_with("RootM.")),
        "renamed root type names leaked into keys: {ops:?}"
    );
}

// ── Test 3: AppSync `@aws_*` directives are tolerated; fields extracted ──────

#[test]
fn appsync_directives_are_tolerated() {
    let schema = fixture("schema_appsync.graphql");
    let ops = GraphqlAdapter
        .extract("schema.graphql", &schema)
        .expect("AppSync SDL with @aws_* directives parses");

    // All root fields across Query/Mutation/Subscription are extracted normally.
    assert_eq!(
        ops.len(),
        4,
        "expected 4 root-field operations, got {ops:?}"
    );
    op_by_key(&ops, "Query.getUser");
    op_by_key(&ops, "Query.listUsers");
    op_by_key(&ops, "Mutation.createUser");

    let sub = op_by_key(&ops, "Subscription.onCreateUser");
    assert_eq!(sub.method, "SUBSCRIPTION");
    assert_eq!(sub.path, "onCreateUser");
}

// ── Test 4: malformed SDL → ContractError::Parse, no panic ───────────────────

#[test]
fn malformed_schema_returns_parse_error() {
    let schema = fixture("schema_malformed.graphql");
    let result = GraphqlAdapter.extract("broken.graphql", &schema);
    match result {
        Err(ContractError::Parse { spec, .. }) => assert_eq!(spec, "broken.graphql"),
        other => panic!("expected ContractError::Parse, got {other:?}"),
    }
}

// ── Test 5: detects — true for schemas, false for operations/arbitrary ───────

#[test]
fn detects_schemas_not_operations_or_arbitrary() {
    let basic = fixture("schema_basic.graphql");
    let renamed = fixture("schema_renamed_roots.graphql");
    let appsync = fixture("schema_appsync.graphql");
    let ops = fixture("ops_basic.graphql");

    // True for SDL schema fixtures.
    assert!(GraphqlAdapter.detects("schema.graphql", &basic));
    assert!(GraphqlAdapter.detects("schema.graphql", &renamed));
    assert!(GraphqlAdapter.detects("schema.graphql", &appsync));

    // False for an executable operation document (operations are not a schema),
    // even under a .graphql filename — the content decides.
    assert!(
        !GraphqlAdapter.detects("ops.graphql", &ops),
        "operation documents must not be detected as a schema"
    );

    // False for arbitrary text / JSON.
    assert!(!GraphqlAdapter.detects("package.json", r#"{ "name": "x", "version": "1.0.0" }"#));
    assert!(!GraphqlAdapter.detects("notes.txt", "just some prose, not GraphQL at all"));
}
