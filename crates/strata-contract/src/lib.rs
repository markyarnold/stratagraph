//! strata-contract: a format-agnostic interface-contract plane.
//!
//! A [`ContractAdapter`] turns a contract specification file (its raw text) into
//! a set of canonical [`OperationDef`]s. The OpenAPI adapter is the first
//! implementation; a GraphQL adapter will be the second, which is why the
//! interface is deliberately format-agnostic.
//!
//! Pure: no IO. The caller reads the spec file and hands the adapter its text,
//! so the same `(spec_path, content)` always yields the same operations
//! (determinism, design R3). A malformed spec returns [`ContractError::Parse`]
//! and never panics (graceful degradation, design R2).

use thiserror::Error;

mod consumer;
mod graphql;
mod grpc;
mod openapi;

pub use consumer::{
    match_consumer, match_graphql_consumer, ConsumerLink, ConsumerTier, OpIndex, CONF_AMBIGUOUS,
    CONF_GRAPHQL_EXTRACTED, CONF_LITERAL_URL, CONF_OPERATION_ID, CONF_TEMPLATE_URL,
};
pub use graphql::{parse_operations, ConsumedField, DocConsumption, GraphqlAdapter, OpType};
pub use grpc::ProtoAdapter;
pub use openapi::OpenApiAdapter;

/// Which contract format an [`OperationDef`] was extracted from.
///
/// An additive discriminator (design slice-4 §3): the OpenAPI path sets
/// [`OpenApi`](ContractFormat::OpenApi) and is otherwise byte-identical; the
/// GraphQL adapter sets [`Graphql`](ContractFormat::Graphql); the protobuf/gRPC
/// adapter sets [`Grpc`](ContractFormat::Grpc). The `format` is one component of a
/// contract operation's estate-wide canonical identity `(api_id, format, key)`
/// (resolved in `strata-index`): the format part keeps a GraphQL `Query.getUser`,
/// an OpenAPI op, and a gRPC `Foo.GetUser` that share a `key` string on distinct
/// canonical nodes, and the `api_id` part keeps two *unrelated* same-key apis
/// apart (the B6 fix — see [`OperationDef::key`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ContractFormat {
    /// OpenAPI v3 / Swagger v2 (JSON or YAML).
    OpenApi,
    /// GraphQL SDL (schema definition language).
    Graphql,
    /// gRPC service definitions in protobuf `.proto` source (proto2 or proto3).
    Grpc,
}

/// A canonical interface operation, independent of the source format.
///
/// `key` is the spec-native operation key: for OpenAPI, the `operationId` when
/// present (stable across repos), otherwise a synthesized
/// `"METHOD /normalized/path"`; for GraphQL, the canonical `"Query.<field>"` /
/// `"Mutation.<field>"` / `"Subscription.<field>"`; for gRPC, the rpc's
/// `"<Service>.<Method>"` (e.g. `"UserService.GetUser"`).
///
/// `key` alone is **not** the estate-wide identity. The canonical identity is
/// `(api_id, format, key)` (assembled in `strata-index`): `key` joins a consumer
/// to a producer *within one api*, while `api_id` (manifest-declared or the repo
/// name) and [`format`](OperationDef::format) keep two unrelated apis that share a
/// `key` — even of the same format — on distinct canonical nodes. This is the B6
/// fix: keying estate dedup on the bare `key` (or `(format, key)`) merged two
/// unrelated APIs that happened to share an operation key into one node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationDef {
    /// Which contract format this operation was extracted from.
    pub format: ContractFormat,
    /// `operationId` if present, else `"METHOD /normalized/path"` (OpenAPI);
    /// `"<CanonicalRoot>.<field>"` (GraphQL); `"<Service>.<Method>"` (gRPC).
    pub key: String,
    /// Upper-case method: `GET`/`POST`/… (OpenAPI), `QUERY`/`MUTATION`/
    /// `SUBSCRIPTION` (GraphQL), or one of `GRPC`/`GRPC_SERVER_STREAM`/
    /// `GRPC_CLIENT_STREAM`/`GRPC_BIDI_STREAM` (gRPC, by streaming shape).
    pub method: String,
    /// Raw spec path, e.g. `"/users/{id}"` (OpenAPI), the field name (GraphQL), or
    /// the rpc method name (gRPC).
    pub path: String,
    /// Params canonicalised, e.g. `"/users/{}"` (see [`normalize_path`]); for
    /// GraphQL this equals the field name and for gRPC the method name (no path
    /// params to canonicalise).
    pub norm_path: String,
    /// The spec-native operation id, when the spec declares one (OpenAPI only;
    /// always `None` for GraphQL and gRPC).
    pub operation_id: Option<String>,
    /// The spec file this operation came from (repo-relative, caller-supplied).
    pub spec_path: String,
}

/// An error from parsing a contract specification.
#[derive(Debug, Error)]
pub enum ContractError {
    /// The spec text could not be parsed (malformed JSON/YAML, or a structurally
    /// invalid spec). Carries the spec path and a human-readable reason.
    #[error("parse error in {spec}: {msg}")]
    Parse { spec: String, msg: String },
}

/// A contract-format adapter. OpenAPI is the first implementation; GraphQL will
/// be the second.
pub trait ContractAdapter {
    /// Cheap heuristic: does `filename`/`content` look like this adapter's
    /// format? Used to pick spec files out of a repo before the (more expensive)
    /// [`extract`](ContractAdapter::extract).
    fn detects(&self, filename: &str, content: &str) -> bool;

    /// Parse a spec file's text into operations. A malformed spec returns
    /// [`ContractError::Parse`] so the caller can degrade (skip this spec, keep
    /// indexing) rather than crash.
    fn extract(&self, spec_path: &str, content: &str) -> Result<Vec<OperationDef>, ContractError>;
}

/// Canonicalise a path template's parameters to a single placeholder so that the
/// two common syntaxes match:
///
/// - OpenAPI/Swagger braces: `"/users/{id}"` → `"/users/{}"`
/// - Express/colon style:    `"/users/:id"` → `"/users/{}"`
///
/// Multi-parameter and mixed paths are handled segment by segment:
/// `"/users/{userId}/posts/:postId"` → `"/users/{}/posts/{}"`. A literal segment
/// is left untouched. This is the canonical form both producer routes and
/// (later) consumer URLs are matched on.
pub fn normalize_path(path: &str) -> String {
    // Split on '/', normalising each segment, then rejoin. Splitting on '/'
    // preserves a leading slash (the first segment is empty) and any trailing
    // slash, so the normalised string keeps the same shape.
    let normalized: Vec<String> = path.split('/').map(normalize_segment).collect();
    normalized.join("/")
}

/// Normalise one path segment. A whole-segment parameter — `{id}`, `:id`, a Django
/// converter `<int:pk>`/`<slug>`/`<path:rest>`, or an OpenAPI matrix/explode variant
/// like `{id*}` — becomes `{}`. Anything else is returned unchanged.
fn normalize_segment(segment: &str) -> String {
    let is_brace_param = segment.starts_with('{') && segment.ends_with('}') && segment.len() >= 2;
    let is_colon_param = segment.starts_with(':') && segment.len() >= 2;
    // Django path-converter syntax: a whole segment `<int:pk>`, `<slug>`, or `<pk>`
    // maps to the same single placeholder as `{id}`/`:id`. No REST path segment is
    // ever a literal `<…>`, so this never canonicalises a real literal away.
    let is_angle_param = segment.starts_with('<') && segment.ends_with('>') && segment.len() >= 2;
    if is_brace_param || is_colon_param || is_angle_param {
        "{}".to_string()
    } else {
        segment.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Test 4: normalize_path ──────────────────────────────────────────────

    #[test]
    fn normalize_path_unifies_brace_and_colon_params() {
        assert_eq!(normalize_path("/users/{id}"), "/users/{}");
        assert_eq!(normalize_path("/users/:id"), "/users/{}");
    }

    #[test]
    fn normalize_path_handles_multi_param_paths() {
        assert_eq!(
            normalize_path("/users/{userId}/posts/{postId}"),
            "/users/{}/posts/{}"
        );
        assert_eq!(
            normalize_path("/users/:userId/posts/:postId"),
            "/users/{}/posts/{}"
        );
        // Mixed syntaxes in one path normalise identically.
        assert_eq!(
            normalize_path("/users/{userId}/posts/:postId"),
            "/users/{}/posts/{}"
        );
    }

    #[test]
    fn normalize_path_leaves_literal_segments_untouched() {
        assert_eq!(normalize_path("/users"), "/users");
        assert_eq!(normalize_path("/"), "/");
        assert_eq!(normalize_path("/v1/users/active"), "/v1/users/active");
        // A param-looking substring that is not a whole segment is NOT a param.
        assert_eq!(normalize_path("/users/me:summary"), "/users/me:summary");
    }

    #[test]
    fn normalize_path_canonicalises_django_converters() {
        // Django `<int:pk>` / `<slug>` / `<path:rest>` normalise to the same `{}` as
        // OpenAPI braces, so a Django route path matches an OpenAPI operation path.
        assert_eq!(normalize_path("/users/<int:pk>"), "/users/{}");
        assert_eq!(
            normalize_path("users/<pk>/posts/<slug:s>"),
            "users/{}/posts/{}"
        );
        // It unifies with the brace and colon forms (all three are one path).
        assert_eq!(
            normalize_path("/users/<int:pk>"),
            normalize_path("/users/{pk}")
        );
        assert_eq!(
            normalize_path("/users/<int:pk>"),
            normalize_path("/users/:pk")
        );
    }
}
