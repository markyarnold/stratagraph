//! The protobuf / gRPC adapter — the third [`ContractAdapter`].
//!
//! It parses protobuf **`.proto` source** text into one [`OperationDef`] per
//! `rpc` declared in a `service`. A `.proto` declares zero or more services, and
//! each service declares zero or more rpcs; every rpc is one callable gRPC
//! operation, so we emit one [`OperationDef`] per rpc (messages and enums declare
//! types, not operations, and contribute nothing — design R1: facts only).
//!
//! **Key scheme (the load-bearing rule).** The cross-repo join key is the rpc's
//! fully-qualified gRPC identity: `"<package>.<Service>.<Method>"` when the file
//! declares a `package` (e.g. `acme.users.v1.UserService.GetUser`), or the bare
//! `"<Service>.<Method>"` when it does not. The package is part of the **key**
//! (the documented choice of the two options — "package as part of key" vs "part
//! of api_id"): it is the wire-correct identity (a gRPC call addresses
//! `/<package>.<Service>/<Method>`), so two repos that host the *same* proto
//! (same package+service+rpc) produce the **same** key and collapse to one
//! canonical node under the estate's `(api_id, format, key)` identity, while two
//! *different*-package services that share a `Service.Method` name stay distinct.
//! (The `api_id` dimension of the estate identity additionally keeps two unrelated
//! same-key apis apart — the B6 fix — independent of the package.)
//!
//! **Streaming.** A streaming rpc is still one operation; its [`method`] records
//! the streaming shape cheaply (`GRPC` unary, `GRPC_SERVER_STREAM`,
//! `GRPC_CLIENT_STREAM`, `GRPC_BIDI_STREAM`) so the shape is visible without a
//! schema change.
//!
//! **proto2 and proto3** are both handled — the parser yields the same
//! service/method structure for either syntax, and we read only that structure.
//!
//! Pure and version-agnostic: `.proto` text in, operations out, no IO. Built on
//! `protox-parse`'s pure single-file parser (`parse(name, source) ->
//! Result<FileDescriptorProto, ParseError>`), which surfaces syntax errors with
//! line:col diagnostics. A `.proto` that will not parse → [`ContractError::Parse`]
//! rather than a partial extraction (design R2: degrade visibly, never silently
//! emit half a contract). Cross-file `import`s are *not* resolved (the parse is
//! purely syntactic on the single file); we only need this file's package,
//! service names, and rpc declarations, all of which are present without
//! resolution.
//!
//! [`method`]: OperationDef::method

use crate::{ContractAdapter, ContractError, ContractFormat, OperationDef};

/// Adapter for protobuf `.proto` source files declaring gRPC services.
pub struct ProtoAdapter;

impl ContractAdapter for ProtoAdapter {
    /// Detect a protobuf `.proto` source file.
    ///
    /// Content-first (like the OpenAPI/GraphQL adapters), not filename-only: a
    /// file merely *named* `.proto` but containing prose must be rejected, and a
    /// proto under any name should be recognised. The discriminator is a cheap
    /// textual signal (the file mentions a proto-defining keyword — `syntax`,
    /// `service`, `message`, or `enum`) **confirmed** by a successful parse: the
    /// cheap signal rejects arbitrary prose before the (more expensive) parse, and
    /// the parse rejects prose that merely happens to contain one of those words.
    ///
    /// A valid `.proto` with only `message`s/`enum`s (no `service`) still detects
    /// `true` — it is a real proto; it simply contributes no operations (that is
    /// the honest [`extract`](ProtoAdapter::extract) result, not a detection
    /// failure).
    fn detects(&self, _filename: &str, content: &str) -> bool {
        // Quick reject: empty/whitespace-only content is never proto.
        if content.trim().is_empty() {
            return false;
        }
        // Cheap textual prefilter: a proto file declares types/services with one
        // of these keywords. Prose without any of them is rejected without a parse.
        if !has_proto_textual_signal(content) {
            return false;
        }
        // Confirm: it actually parses as protobuf. This rejects a prose file that
        // merely contains the word "service"/"message" (it will not parse).
        protox_parse::parse("detect.proto", content).is_ok()
    }

    /// Parse `.proto` source into one [`OperationDef`] per rpc.
    ///
    /// Any syntax error → [`ContractError::Parse`] (the parser surfaces a precise
    /// line:col diagnostic). The package (if any) qualifies every key; each
    /// service's rpcs become operations keyed `"<package>.<Service>.<Method>"`
    /// (or bare `"<Service>.<Method>"` with no package). Services are walked in
    /// declaration order, rpcs within each in declaration order, so the output is
    /// deterministic.
    fn extract(&self, spec_path: &str, content: &str) -> Result<Vec<OperationDef>, ContractError> {
        let file = protox_parse::parse(spec_path, content).map_err(|e| ContractError::Parse {
            spec: spec_path.to_string(),
            msg: format!("protobuf syntax error: {e}"),
        })?;

        // The file-level package (`package foo.bar.v1;`). Absent → empty string,
        // and the key is then the bare `Service.Method`.
        let package = file.package();

        let mut ops = Vec::new();
        for service in &file.service {
            let Some(service_name) = service.name.as_deref() else {
                continue; // a service with no name is not addressable; skip it.
            };
            for method in &service.method {
                let Some(method_name) = method.name.as_deref() else {
                    continue;
                };
                ops.push(operation_def(
                    package,
                    service_name,
                    method_name,
                    method.client_streaming(),
                    method.server_streaming(),
                    spec_path,
                ));
            }
        }

        Ok(ops)
    }
}

/// Build a canonical [`OperationDef`] for one rpc.
///
/// `key` is `"<package>.<Service>.<Method>"` (or bare `"<Service>.<Method>"` when
/// `package` is empty). `method` records the streaming shape.
fn operation_def(
    package: &str,
    service: &str,
    method: &str,
    client_streaming: bool,
    server_streaming: bool,
    spec_path: &str,
) -> OperationDef {
    let key = if package.is_empty() {
        format!("{service}.{method}")
    } else {
        format!("{package}.{service}.{method}")
    };
    OperationDef {
        format: ContractFormat::Grpc,
        key,
        method: grpc_method_label(client_streaming, server_streaming).to_string(),
        path: method.to_string(),
        norm_path: method.to_string(),
        operation_id: None,
        spec_path: spec_path.to_string(),
    }
}

/// The upper-case `method` label for an rpc, encoding its streaming shape:
/// `GRPC` (unary), `GRPC_SERVER_STREAM`, `GRPC_CLIENT_STREAM`, or
/// `GRPC_BIDI_STREAM`. (A method string, not a new struct field — no schema bump.)
fn grpc_method_label(client_streaming: bool, server_streaming: bool) -> &'static str {
    match (client_streaming, server_streaming) {
        (false, false) => "GRPC",
        (false, true) => "GRPC_SERVER_STREAM",
        (true, false) => "GRPC_CLIENT_STREAM",
        (true, true) => "GRPC_BIDI_STREAM",
    }
}

/// The cheap textual signal that a file is *attempting* to be protobuf: it
/// mentions one of the keywords that declare a proto's structure (`syntax`,
/// `service`, `message`, `enum`). Prose with none of these is rejected before the
/// full parse; a file that has the signal but will not parse is not proto either
/// (the parse in [`ProtoAdapter::detects`] confirms). This is the gRPC analogue of
/// the OpenAPI version-marker / SQL `CREATE…TABLE` prefilters.
fn has_proto_textual_signal(content: &str) -> bool {
    content.contains("syntax")
        || contains_keyword(content, "service")
        || contains_keyword(content, "message")
        || contains_keyword(content, "enum")
}

/// Whether `content` contains `keyword` as a whole word followed by whitespace
/// (`"service "` / `"service\n"` / `"service\t"`). A coarse but cheap check that
/// avoids matching the word inside an unrelated identifier; the real confirmation
/// is the parse. Matches the prefilter spirit without a regex dependency.
fn contains_keyword(content: &str, keyword: &str) -> bool {
    content.split_whitespace().any(|tok| tok == keyword)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The streaming label encodes all four shapes distinctly.
    #[test]
    fn streaming_labels_are_distinct() {
        assert_eq!(grpc_method_label(false, false), "GRPC");
        assert_eq!(grpc_method_label(false, true), "GRPC_SERVER_STREAM");
        assert_eq!(grpc_method_label(true, false), "GRPC_CLIENT_STREAM");
        assert_eq!(grpc_method_label(true, true), "GRPC_BIDI_STREAM");
    }

    /// A package-qualified key is the fully-qualified gRPC identity; an absent
    /// package yields the bare `Service.Method` (no leading dot).
    #[test]
    fn key_qualified_by_package_when_present() {
        let with_pkg = operation_def("foo.v1", "Svc", "Get", false, false, "s.proto");
        assert_eq!(with_pkg.key, "foo.v1.Svc.Get");
        let no_pkg = operation_def("", "Svc", "Get", false, false, "s.proto");
        assert_eq!(no_pkg.key, "Svc.Get");
    }

    /// The cheap prefilter accepts proto-keyword content and rejects bare prose.
    #[test]
    fn textual_signal_discriminates() {
        assert!(has_proto_textual_signal("service Foo { }"));
        assert!(has_proto_textual_signal("syntax = \"proto3\";"));
        assert!(has_proto_textual_signal("message M { }"));
        assert!(!has_proto_textual_signal(
            "just prose with no proto keywords at all"
        ));
        // "services" (plural) is not the `service` keyword token.
        assert!(!has_proto_textual_signal("we offer many services here"));
    }
}
