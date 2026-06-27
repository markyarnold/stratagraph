//! GraphQL executable-document parsing tests (Slice 4, Milestone 1 — Definition
//! of Done tests 6, 7, 9).
//!
//! `parse_operations` reads the root fields each operation in an executable
//! document consumes: named + anonymous + multiple operations, mutations;
//! `__`-meta fields skipped; root fragment spreads counted (not guessed);
//! malformed input → `ContractError::Parse`.

use strata_contract::{parse_operations, ConsumedField, ContractError, OpType};

fn fixture(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read fixture {name}: {e}"))
}

fn consumed(op_type: OpType, field: &str) -> ConsumedField {
    ConsumedField {
        op_type,
        field: field.to_string(),
    }
}

// ── Test 6a: ops_basic → the two named operations' root fields ───────────────

#[test]
fn parse_ops_basic_named_query_and_mutation() {
    let doc = fixture("ops_basic.graphql");
    let result = parse_operations("ops.graphql", &doc).expect("ops_basic parses");

    assert_eq!(
        result.fields,
        vec![
            consumed(OpType::Query, "getUser"),
            consumed(OpType::Mutation, "createUser"),
        ]
    );
    assert_eq!(result.unresolved_root_spreads, 0);
}

// ── Test 6b: ops_multi → all three operations incl. the anonymous one ────────

#[test]
fn parse_ops_multi_named_and_anonymous() {
    let doc = fixture("ops_multi.graphql");
    let result = parse_operations("ops.graphql", &doc).expect("ops_multi parses");

    // Two named queries (getUser, listUsers) + the anonymous `{ listUsers }`.
    assert_eq!(
        result.fields,
        vec![
            consumed(OpType::Query, "getUser"),
            consumed(OpType::Query, "listUsers"),
            consumed(OpType::Query, "listUsers"),
        ],
        "all three operations' root fields, anonymous included"
    );
    assert_eq!(result.unresolved_root_spreads, 0);
}

// ── Test 7: root fragment spread is counted; __typename is skipped ───────────

#[test]
fn parse_ops_fragment_spread_counted_meta_skipped() {
    let doc = fixture("ops_fragment.graphql");
    let result = parse_operations("ops.graphql", &doc).expect("ops_fragment parses");

    // Operation `Q`'s only root selection is `...RootFields` (a spread): counted,
    // not resolved into fields. Operation `WithMeta` contributes `listUsers`
    // (its `__typename` root field is skipped). The fragment DEFINITION's body is
    // not walked, so `getUser` inside it does NOT appear.
    assert_eq!(
        result.fields,
        vec![consumed(OpType::Query, "listUsers")],
        "spread not expanded, __typename skipped, fragment body not walked"
    );
    assert_eq!(
        result.unresolved_root_spreads, 1,
        "the one root-level fragment spread is counted"
    );
}

// ── Test 9: malformed operation document → ContractError::Parse, no panic ─────

#[test]
fn malformed_operation_document_returns_parse_error() {
    // An operation whose selection set is never closed.
    let bad = "query Broken { getUser(id: \"1\") { name } ";
    let result = parse_operations("broken.graphql", bad);
    match result {
        Err(ContractError::Parse { spec, .. }) => assert_eq!(spec, "broken.graphql"),
        other => panic!("expected ContractError::Parse, got {other:?}"),
    }
}
