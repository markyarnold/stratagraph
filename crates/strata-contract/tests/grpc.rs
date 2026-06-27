//! Protobuf / gRPC adapter tests (Slice 17, Track D4a, Milestone 1).
//!
//! Extraction of `.proto` `service` definitions into canonical
//! `"<package>.<Service>.<Method>"` `OperationDef`s (proto3 + proto2), streaming
//! shape capture, graceful degradation on malformed `.proto`, and `detects`
//! discrimination (a real proto vs a `.proto`-named prose file), all against
//! committed `.proto` fixtures.

use strata_contract::{ContractAdapter, ContractError, ContractFormat, OperationDef, ProtoAdapter};

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

// ── Test 1: proto3 service with 3 rpcs (incl. streaming) → 3 OperationDefs ────

#[test]
fn extract_proto3_service_three_rpcs() {
    let proto = fixture("user_service.proto");
    let ops = ProtoAdapter
        .extract("user.proto", &proto)
        .expect("proto3 service parses");

    // Exactly the three rpcs; messages and the enum contribute nothing.
    assert_eq!(ops.len(), 3, "expected 3 rpc operations, got {ops:?}");

    // Unary GetUser: fully specified, package-qualified key, GRPC method.
    let get_user = op_by_key(&ops, "acme.users.v1.UserService.GetUser");
    assert_eq!(
        *get_user,
        OperationDef {
            format: ContractFormat::Grpc,
            key: "acme.users.v1.UserService.GetUser".into(),
            method: "GRPC".into(),
            path: "GetUser".into(),
            norm_path: "GetUser".into(),
            operation_id: None,
            spec_path: "user.proto".into(),
        }
    );

    // Server-streaming ListUsers.
    let list_users = op_by_key(&ops, "acme.users.v1.UserService.ListUsers");
    assert_eq!(list_users.method, "GRPC_SERVER_STREAM");
    assert_eq!(list_users.path, "ListUsers");

    // Bidi-streaming Watch.
    let watch = op_by_key(&ops, "acme.users.v1.UserService.Watch");
    assert_eq!(watch.method, "GRPC_BIDI_STREAM");
    assert_eq!(watch.format, ContractFormat::Grpc);
}

// ── Test 2: proto2 service is handled (not just proto3) ──────────────────────

#[test]
fn extract_proto2_service() {
    let proto = fixture("legacy_service.proto");
    let ops = ProtoAdapter
        .extract("legacy.proto", &proto)
        .expect("proto2 service parses");

    assert_eq!(ops.len(), 1, "expected 1 rpc, got {ops:?}");
    let op = op_by_key(&ops, "legacy.LegacyService.Do");
    assert_eq!(op.method, "GRPC");
    assert_eq!(op.format, ContractFormat::Grpc);
}

// ── Test 3: a .proto with only messages → detects true, ZERO operations ──────

#[test]
fn types_only_proto_detects_but_yields_no_ops() {
    let proto = fixture("types_only.proto");
    // It IS a proto file (valid syntax, has messages/enum) — detect true.
    assert!(
        ProtoAdapter.detects("types.proto", &proto),
        "a valid message-only .proto is still proto"
    );
    // …but it declares no rpcs → zero operations (honest, no invention).
    let ops = ProtoAdapter
        .extract("types.proto", &proto)
        .expect("a message-only proto parses fine");
    assert!(
        ops.is_empty(),
        "a proto with no service declares no operations, got {ops:?}"
    );
}

// ── Test 4: malformed .proto → ContractError::Parse (diagnostic, no panic) ───

#[test]
fn malformed_proto_is_a_diagnostic_not_a_panic() {
    let proto = fixture("malformed.proto");
    let err = ProtoAdapter
        .extract("broken.proto", &proto)
        .expect_err("a malformed proto must be an error, never partial operations");
    match err {
        ContractError::Parse { spec, msg } => {
            assert_eq!(spec, "broken.proto", "the diagnostic names the spec");
            assert!(!msg.is_empty(), "the diagnostic carries a reason");
        }
    }
}

// ── Test 5: detects discrimination (real proto vs .proto-named prose) ────────

#[test]
fn detects_real_proto_and_rejects_prose() {
    // A real service proto is detected.
    let svc = fixture("user_service.proto");
    assert!(ProtoAdapter.detects("user_service.proto", &svc));

    // A file merely *named* `.proto` but containing prose is NOT proto, even
    // though it mentions the words "service" and "message".
    let prose = fixture("not_proto.proto");
    assert!(
        !ProtoAdapter.detects("not_proto.proto", &prose),
        "a .proto-named prose file is not a protobuf definition"
    );

    // Empty content is never proto.
    assert!(!ProtoAdapter.detects("empty.proto", ""));

    // A non-proto extension with proto-shaped content is still detected by
    // content (the indexer keys on `.proto`, but detect itself is content-first,
    // mirroring the OpenAPI/GraphQL adapters).
    assert!(ProtoAdapter.detects("schema.txt", &svc));
}

// ── Test 6: no-package proto → bare Service.Method key ───────────────────────

#[test]
fn no_package_yields_bare_service_method_key() {
    let proto = concat!(
        "syntax = \"proto3\";\n",
        "service Ping { rpc Do(Req) returns (Resp); }\n",
        "message Req {}\n",
        "message Resp {}\n",
    );
    let ops = ProtoAdapter.extract("ping.proto", proto).expect("parses");
    assert_eq!(ops.len(), 1);
    // No package declared → the key is just `Service.Method` (no leading dot).
    assert_eq!(ops[0].key, "Ping.Do");
}
