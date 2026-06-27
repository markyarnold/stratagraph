//! Shared consumer matching: map a consumer call signal to operation(s).
//!
//! This is the consumer-side mirror of the producer linking in `strata-index`.
//! It is **pure** (no IO, no graph) and **format-agnostic** (it consumes the
//! canonical [`OperationDef`] set, however it was extracted), so both the
//! per-repo linking pass and the cross-repo estate pass call exactly this one
//! function — they can never drift (design R3; brief §2).
//!
//! **Honest provenance — REST is convention, GraphQL names the contract.** A
//! *REST* consumer call does not *name* the operation it hits; the link is a
//! name- or URL-convention match, so every REST consumer link is at most
//! `Inferred`. A *GraphQL* consumer document, by contrast, names the contract
//! element in the contract's own language (`query { getUser }` ⇒ `Query.getUser`);
//! the only residual uncertainty is which schema owns the key, so a unique
//! GraphQL match is `Extracted` (0.95, the band floor) and a multi-schema
//! collision is `Ambiguous` (brief §2; spec §3). When nothing matches we emit
//! **no** link (surfaced by absence, never invented). The confidence tiers are:
//!
//! | signal                                   | unique match  | several matches |
//! |------------------------------------------|---------------|-----------------|
//! | GraphQL field (`query { getUser }`)      | Extracted 0.95| Ambiguous 0.35  |
//! | callee name == an `operationId`          | Inferred 0.75 | Ambiguous 0.35  |
//! | literal URL (`fetch("/users/1")`)        | Inferred 0.70 | Ambiguous 0.35  |
//! | template URL (`` fetch(`/users/${id}`) ``) | Inferred 0.60 | Ambiguous 0.35  |
//! | dynamic / no match                       | — (no link, surfaced as unmatched)|
//!
//! All tiers respect the §4.1 band invariant: Extracted ≥ 0.95, Inferred ≤ 0.80,
//! Ambiguous < 0.40.

use strata_core::{HttpCall, Provenance, UrlShape};

use crate::{normalize_path, ConsumedField, ContractFormat, OperationDef};

// ── Confidence tiers (honest, band-respecting). ──────────────────────────────
//
// Each is < the Inferred ceiling (0.80) and ordered by signal strength: an
// operationId name match is the strongest static signal, a literal URL next, a
// template URL weakest of the concrete tiers. A multi-candidate match of any
// tier collapses to a single Ambiguous confidence (< 0.40) — recall-biased: we
// emit an edge to each candidate rather than guess one (R5).

/// A parsed GraphQL document field matched a unique schema operation: the
/// document *names* the contract element in its own language, so this is
/// `Extracted` (band 0.95–1.0), at the band floor. The only residual uncertainty
/// (which schema owns the key) is handled by [`CONF_AMBIGUOUS`] on a collision.
pub const CONF_GRAPHQL_EXTRACTED: f32 = 0.95;
/// `callee_name == operationId`, exactly one operation: a strong name signal.
/// Inferred (band 0.40–0.80).
pub const CONF_OPERATION_ID: f32 = 0.75;
/// Literal URL matched a unique operation by method + normalized path.
/// Inferred (band 0.40–0.80).
pub const CONF_LITERAL_URL: f32 = 0.70;
/// Template URL matched a unique operation by method + normalized path.
/// Inferred (band 0.40–0.80) — weaker than a literal because the interpolations
/// are erased to `{}` before matching.
pub const CONF_TEMPLATE_URL: f32 = 0.60;
/// Any tier with several candidate operations. Ambiguous (band < 0.40).
pub const CONF_AMBIGUOUS: f32 = 0.35;

/// Which signal produced a [`ConsumerLink`] (mirrors the tier table above).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsumerTier {
    /// A parsed GraphQL document field matched a schema operation by canonical
    /// key. The strongest consumer signal — the document names the contract.
    GraphqlField,
    /// The callee identifier equals an operation's `operationId`.
    OperationIdName,
    /// A string-literal URL matched method + normalized path.
    LiteralUrl,
    /// A template-string URL matched method + normalized path.
    TemplateUrl,
}

impl ConsumerTier {
    /// The contract format a link of this tier joins to. A GraphQL document field
    /// links to a GraphQL operation; the REST tiers (operationId / URL) link to an
    /// OpenAPI operation. The caller uses this to resolve a link's `op_key` to the
    /// **correct-format** operation node — a GraphQL key and an OpenAPI key that
    /// share the same string must never cross-resolve (brief §2/§5).
    pub fn format(self) -> ContractFormat {
        match self {
            ConsumerTier::GraphqlField => ContractFormat::Graphql,
            ConsumerTier::OperationIdName
            | ConsumerTier::LiteralUrl
            | ConsumerTier::TemplateUrl => ContractFormat::OpenApi,
        }
    }
}

/// One consumer→operation link: the operation's cross-repo `key`, the honest
/// provenance, and the band-respecting confidence. The `key` (not a UID) is the
/// linkage currency so the same result works for per-repo *and* cross-repo
/// linking — the caller resolves `key` to whichever operation node it has.
#[derive(Debug, Clone, PartialEq)]
pub struct ConsumerLink {
    /// The matched operation's cross-repo `key` ([`OperationDef::key`]).
    pub op_key: String,
    /// Which signal matched (for the coverage report's by-tier tally).
    pub tier: ConsumerTier,
    /// Honest provenance: `Inferred` for a unique match, `Ambiguous` for several.
    pub provenance: Provenance,
    /// Band-respecting confidence (Inferred ≤ 0.80, Ambiguous < 0.40).
    pub confidence: f32,
}

/// A lookup over the canonical operation set for consumer matching.
///
/// Built once from the operations in scope (one repo's local ops for per-repo
/// linking; the full deduped estate set for cross-repo linking) and queried per
/// consumer signal. Operations are deduped by their full source identity
/// `(format, key, spec_path)` — so the *same* operation listed twice in one spec
/// contributes one candidate, while two **distinct schemas/specs** declaring the
/// same `key` are kept as two candidates (a genuine multi-source collision that
/// the GraphQL matcher reports as `Ambiguous`; brief §4).
pub struct OpIndex {
    /// All operations in scope, deduped by `(format, key, spec_path)`, in
    /// deterministic key order.
    ops: Vec<OperationDef>,
}

impl OpIndex {
    /// Build an index over `operations`, deduping by `(format, key, spec_path)`.
    /// The first occurrence of each identity wins (deterministic given the
    /// caller's order); the result is sorted by `key` so matches come out in a
    /// stable order. Two specs that each declare the same `key` are preserved as
    /// distinct candidates (the multi-source ambiguity case).
    pub fn new(operations: &[OperationDef]) -> OpIndex {
        let mut seen: std::collections::BTreeSet<(ContractFormat, &str, &str)> =
            std::collections::BTreeSet::new();
        let mut ops: Vec<OperationDef> = Vec::new();
        for op in operations {
            if seen.insert((op.format, op.key.as_str(), op.spec_path.as_str())) {
                ops.push(op.clone());
            }
        }
        ops.sort_by(|a, b| a.key.cmp(&b.key));
        OpIndex { ops }
    }

    /// Operations whose `operation_id` equals `name`, in key order.
    fn by_operation_id(&self, name: &str) -> Vec<&OperationDef> {
        self.ops
            .iter()
            .filter(|op| op.operation_id.as_deref() == Some(name))
            .collect()
    }

    /// Operations whose method == `method` and whose `norm_path` matches the
    /// consumer's normalized URL path `url_norm` (placeholder-aware), in key
    /// order. `method` is compared case-insensitively (operations are already
    /// upper-cased by the adapter; the caller passes an upper-cased method).
    fn by_method_and_path(&self, method: &str, url_norm: &str) -> Vec<&OperationDef> {
        self.ops
            .iter()
            .filter(|op| op.method == method && path_matches(&op.norm_path, url_norm))
            .collect()
    }

    /// GraphQL-format operations whose canonical `key` equals `key`, in key
    /// order. The format filter is load-bearing: a GraphQL document field
    /// (`Query.getUser`) must never join to an OpenAPI operation that happens to
    /// share the string `key` (brief §2 — `(format, key)` is the real identity).
    fn graphql_by_key(&self, key: &str) -> Vec<&OperationDef> {
        self.ops
            .iter()
            .filter(|op| op.format == ContractFormat::Graphql && op.key == key)
            .collect()
    }
}

/// Match one consumer signal against the operation set, returning every matching
/// link (possibly several → each `Ambiguous`).
///
/// The two signals are independent and both honoured (a call can have *both* an
/// operationId-shaped callee name AND an HTTP URL — each is matched and the
/// union returned, deduped by `op_key`+tier):
/// - `callee_name`: when it equals an operation's `operationId`, an
///   `OperationIdName` link (covers operationId-named generated clients).
/// - `http`: when present, its method + URL shape drive a `LiteralUrl` or
///   `TemplateUrl` match. A `Dynamic` URL never matches (opaque path; R5).
///
/// Returns an **empty** vec when nothing matches — the caller surfaces that as
/// an unmatched consumer, never an invented edge.
pub fn match_consumer(
    callee_name: Option<&str>,
    http: Option<&HttpCall>,
    ops: &OpIndex,
) -> Vec<ConsumerLink> {
    let mut links: Vec<ConsumerLink> = Vec::new();

    // ── Tier 1: operationId == callee name. ──
    if let Some(name) = callee_name {
        let matches = ops.by_operation_id(name);
        push_tier(&mut links, &matches, ConsumerTier::OperationIdName);
    }

    // ── Tier 2/3: URL shape (literal / template). Dynamic never matches. ──
    if let Some(call) = http {
        // No method ⇒ we cannot match a method+path operation (method unknown,
        // not assumed). A missing method is only produced when an explicit
        // options object had a non-literal `method`; treat it as unmatchable.
        if let Some(method) = call.method.as_deref() {
            match &call.url {
                UrlShape::Literal(raw) => {
                    let url_norm = normalize_path(raw);
                    let matches = ops.by_method_and_path(method, &url_norm);
                    push_tier(&mut links, &matches, ConsumerTier::LiteralUrl);
                }
                UrlShape::Template(pattern) => {
                    // The template already carries `{}` placeholders; normalize
                    // it too so any literal `:id`-style segments also canonicalise.
                    let url_norm = normalize_path(pattern);
                    let matches = ops.by_method_and_path(method, &url_norm);
                    push_tier(&mut links, &matches, ConsumerTier::TemplateUrl);
                }
                // Dynamic: opaque path. No concrete link (R5) — surfaced as
                // unmatched rather than guessed.
                UrlShape::Dynamic => {}
            }
        }
    }

    // De-dup identical (op_key, tier) pairs that two signals could both yield.
    links.sort_by(|a, b| {
        a.op_key
            .cmp(&b.op_key)
            .then(tier_ord(a.tier).cmp(&tier_ord(b.tier)))
    });
    links.dedup_by(|a, b| a.op_key == b.op_key && a.tier == b.tier);
    links
}

/// Match one consumed GraphQL document field against the operation set, returning
/// a link per matching GraphQL operation.
///
/// The join key is the canonical `"<op_type>.<field>"` (e.g. `Query.getUser`) —
/// exactly what the schema adapter keys its operations on — restricted to
/// GraphQL-format operations (so a GraphQL field never joins to an OpenAPI op).
/// Provenance is tiered by match count (brief §2; spec §3):
/// - **exactly one** → `Extracted` (0.95): the document *names* the contract
///   element in its own language; the schema match is a fact, not a convention.
/// - **several** (the key is declared in two schemas) → one `Ambiguous` (0.35)
///   link per candidate, recall-biased — never guess which schema owns it (R5).
/// - **none** → empty: the field hits something no schema declares; surfaced by
///   absence, never invented (R1/R5).
pub fn match_graphql_consumer(field: &ConsumedField, ops: &OpIndex) -> Vec<ConsumerLink> {
    let key = format!("{}.{}", field.op_type.canonical(), field.field);
    let matches = ops.graphql_by_key(&key);
    if matches.is_empty() {
        return Vec::new();
    }
    let unique = matches.len() == 1;
    let (prov, conf) = if unique {
        (Provenance::Extracted, CONF_GRAPHQL_EXTRACTED)
    } else {
        (Provenance::Ambiguous, CONF_AMBIGUOUS)
    };
    matches
        .into_iter()
        .map(|op| ConsumerLink {
            op_key: op.key.clone(),
            tier: ConsumerTier::GraphqlField,
            provenance: prov,
            confidence: conf,
        })
        .collect()
}

/// Push a link per matched operation in `tier`, choosing provenance/confidence
/// by the match count: a unique match is `Inferred` at the tier's confidence,
/// several matches are each `Ambiguous` (recall-biased — one edge per candidate).
fn push_tier(out: &mut Vec<ConsumerLink>, matches: &[&OperationDef], tier: ConsumerTier) {
    if matches.is_empty() {
        return;
    }
    let unique = matches.len() == 1;
    let (prov, conf) = if unique {
        (Provenance::Inferred, tier_confidence(tier))
    } else {
        (Provenance::Ambiguous, CONF_AMBIGUOUS)
    };
    for op in matches {
        out.push(ConsumerLink {
            op_key: op.key.clone(),
            tier,
            provenance: prov,
            confidence: conf,
        });
    }
}

/// The unique-match confidence for `tier`. The REST tiers are Inferred; the
/// GraphQL tier is Extracted (0.95) — its provenance is set in
/// [`match_graphql_consumer`], not via [`push_tier`].
fn tier_confidence(tier: ConsumerTier) -> f32 {
    match tier {
        ConsumerTier::GraphqlField => CONF_GRAPHQL_EXTRACTED,
        ConsumerTier::OperationIdName => CONF_OPERATION_ID,
        ConsumerTier::LiteralUrl => CONF_LITERAL_URL,
        ConsumerTier::TemplateUrl => CONF_TEMPLATE_URL,
    }
}

/// A total order over tiers for deterministic de-dup/sort.
fn tier_ord(tier: ConsumerTier) -> u8 {
    match tier {
        ConsumerTier::GraphqlField => 0,
        ConsumerTier::OperationIdName => 1,
        ConsumerTier::LiteralUrl => 2,
        ConsumerTier::TemplateUrl => 3,
    }
}

/// Whether a consumer's normalized URL path matches an operation's normalized
/// path template, segment by segment.
///
/// Both are already normalized (params → `{}`), so this is mostly an equality
/// check — *except* a consumer literal carries concrete values where the spec
/// has a placeholder: `fetch("/users/123")` normalizes to `/users/123` (123 is
/// not a whole-segment param), which must match the spec's `/users/{}`. So a
/// spec `{}` segment matches ANY single consumer segment; every other segment
/// must be byte-equal. Segment counts must be equal (no prefix matching here —
/// that is the dynamic-fallback's job, not the concrete tiers').
fn path_matches(op_norm: &str, url_norm: &str) -> bool {
    let op_segs: Vec<&str> = op_norm.split('/').collect();
    let url_segs: Vec<&str> = url_norm.split('/').collect();
    if op_segs.len() != url_segs.len() {
        return false;
    }
    op_segs
        .iter()
        .zip(url_segs.iter())
        .all(|(o, u)| *o == "{}" || o == u)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::OpType;
    use strata_core::Span;

    fn op(key: &str, method: &str, path: &str, op_id: Option<&str>) -> OperationDef {
        OperationDef {
            format: crate::ContractFormat::OpenApi,
            key: key.to_string(),
            method: method.to_string(),
            path: path.to_string(),
            norm_path: normalize_path(path),
            operation_id: op_id.map(str::to_string),
            spec_path: "openapi.yaml".to_string(),
        }
    }

    /// A GraphQL-format operation `Query.<field>` (or other root) from `spec`.
    fn gql_op(key: &str, method: &str, field: &str, spec: &str) -> OperationDef {
        OperationDef {
            format: crate::ContractFormat::Graphql,
            key: key.to_string(),
            method: method.to_string(),
            path: field.to_string(),
            norm_path: field.to_string(),
            operation_id: None,
            spec_path: spec.to_string(),
        }
    }

    fn consumed(op_type: OpType, field: &str) -> ConsumedField {
        ConsumedField {
            op_type,
            field: field.to_string(),
        }
    }

    fn http(method: &str, url: UrlShape) -> HttpCall {
        HttpCall {
            method: Some(method.to_string()),
            url,
            enclosing_fqn: "caller".to_string(),
            span: Span::default(),
        }
    }

    // ── Test 2 (brief §"match_consumer (pure)"): the tier ladder. ──

    #[test]
    fn callee_name_equals_operation_id_is_inferred_075() {
        let ops = OpIndex::new(&[op("getUser", "GET", "/users/{id}", Some("getUser"))]);
        let links = match_consumer(Some("getUser"), None, &ops);
        assert_eq!(links.len(), 1, "one operationId-name match");
        assert_eq!(links[0].op_key, "getUser");
        assert_eq!(links[0].tier, ConsumerTier::OperationIdName);
        assert_eq!(links[0].provenance, Provenance::Inferred);
        assert_eq!(links[0].confidence, 0.75);
    }

    #[test]
    fn literal_url_unique_match_is_inferred_070() {
        let ops = OpIndex::new(&[op("getUser", "GET", "/users/{id}", Some("getUser"))]);
        let call = http("GET", UrlShape::Literal("/users/123".to_string()));
        let links = match_consumer(None, Some(&call), &ops);
        assert_eq!(links.len(), 1, "one literal-URL match");
        assert_eq!(links[0].op_key, "getUser");
        assert_eq!(links[0].tier, ConsumerTier::LiteralUrl);
        assert_eq!(links[0].provenance, Provenance::Inferred);
        assert_eq!(links[0].confidence, 0.70);
    }

    #[test]
    fn template_url_unique_match_is_inferred_060() {
        let ops = OpIndex::new(&[op("getUser", "GET", "/users/{id}", Some("getUser"))]);
        let call = http("GET", UrlShape::Template("/users/{}".to_string()));
        let links = match_consumer(None, Some(&call), &ops);
        assert_eq!(links.len(), 1, "one template-URL match");
        assert_eq!(links[0].op_key, "getUser");
        assert_eq!(links[0].tier, ConsumerTier::TemplateUrl);
        assert_eq!(links[0].provenance, Provenance::Inferred);
        assert_eq!(links[0].confidence, 0.60);
    }

    #[test]
    fn multi_match_is_ambiguous_below_040() {
        // Same operationId in two operations (different specs) → both Ambiguous.
        let ops = OpIndex::new(&[
            op("getThing@v1", "GET", "/things/{id}", Some("getThing")),
            op("getThing@v2", "GET", "/things/{id}", Some("getThing")),
        ]);
        // A literal URL that matches BOTH /things/{} operations.
        let call = http("GET", UrlShape::Literal("/things/9".to_string()));
        let links = match_consumer(None, Some(&call), &ops);
        assert_eq!(links.len(), 2, "two candidate operations → two edges");
        for link in &links {
            assert_eq!(link.provenance, Provenance::Ambiguous);
            assert!(
                link.confidence < 0.40,
                "Ambiguous confidence must be < 0.40, got {}",
                link.confidence
            );
        }
    }

    #[test]
    fn no_match_returns_empty() {
        let ops = OpIndex::new(&[op("getUser", "GET", "/users/{id}", Some("getUser"))]);
        // A URL with no matching operation.
        let call = http("GET", UrlShape::Literal("/widgets/9".to_string()));
        assert!(
            match_consumer(Some("noSuchOp"), Some(&call), &ops).is_empty(),
            "no operationId match AND no path match → empty"
        );
    }

    #[test]
    fn dynamic_url_never_matches() {
        let ops = OpIndex::new(&[op("getUser", "GET", "/users/{id}", Some("getUser"))]);
        let call = http("GET", UrlShape::Dynamic);
        assert!(
            match_consumer(None, Some(&call), &ops).is_empty(),
            "a dynamic URL is opaque — no concrete link (R5)"
        );
    }

    #[test]
    fn method_must_match() {
        let ops = OpIndex::new(&[op("getUser", "GET", "/users/{id}", Some("getUser"))]);
        // Right path, wrong method.
        let call = http("POST", UrlShape::Literal("/users/123".to_string()));
        assert!(
            match_consumer(None, Some(&call), &ops).is_empty(),
            "method mismatch yields no link"
        );
    }

    #[test]
    fn literal_static_path_matches_static_operation() {
        // A path with no params: exact match, no placeholder involved.
        let ops = OpIndex::new(&[op("getHealth", "GET", "/health", Some("getHealth"))]);
        let call = http("GET", UrlShape::Literal("/health".to_string()));
        let links = match_consumer(None, Some(&call), &ops);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].op_key, "getHealth");
    }

    #[test]
    fn segment_count_mismatch_does_not_match() {
        // /users/{} must NOT match a bare /users (different segment counts).
        let ops = OpIndex::new(&[op("getUser", "GET", "/users/{id}", Some("getUser"))]);
        let call = http("GET", UrlShape::Literal("/users".to_string()));
        assert!(match_consumer(None, Some(&call), &ops).is_empty());
    }

    #[test]
    fn both_signals_dedup_when_same_operation_different_tier() {
        // A call that is BOTH an operationId-name match AND a literal-URL match to
        // the same operation yields one link per tier (they are distinct tiers).
        let ops = OpIndex::new(&[op("getUser", "GET", "/users/{id}", Some("getUser"))]);
        let call = http("GET", UrlShape::Literal("/users/1".to_string()));
        let links = match_consumer(Some("getUser"), Some(&call), &ops);
        // Two links: OperationIdName (0.75) + LiteralUrl (0.70), same op_key.
        assert_eq!(links.len(), 2);
        assert!(links.iter().all(|l| l.op_key == "getUser"));
        assert!(links
            .iter()
            .any(|l| l.tier == ConsumerTier::OperationIdName));
        assert!(links.iter().any(|l| l.tier == ConsumerTier::LiteralUrl));
    }

    // ── Test 3 (brief §"matcher (pure)"): the GraphQL consumer matcher. ──

    #[test]
    fn graphql_unique_match_is_extracted_095() {
        // A single GraphQL schema declares Query.getUser → Extracted 0.95.
        let ops = OpIndex::new(&[
            gql_op("Query.getUser", "QUERY", "getUser", "schema.graphql"),
            gql_op(
                "Mutation.createUser",
                "MUTATION",
                "createUser",
                "schema.graphql",
            ),
        ]);
        let links = match_graphql_consumer(&consumed(OpType::Query, "getUser"), &ops);
        assert_eq!(links.len(), 1, "one unique GraphQL match");
        assert_eq!(links[0].op_key, "Query.getUser");
        assert_eq!(links[0].tier, ConsumerTier::GraphqlField);
        assert_eq!(links[0].provenance, Provenance::Extracted);
        assert_eq!(links[0].confidence, 0.95);
        assert!(
            links[0].confidence >= 0.95,
            "Extracted must sit in the 0.95–1.0 band"
        );
    }

    #[test]
    fn graphql_two_schema_collision_is_ambiguous_035_each() {
        // The SAME canonical key Query.getUser declared in TWO distinct schemas
        // (different spec_path) → one Ambiguous link per candidate (recall-biased),
        // each < 0.40. The OpIndex keeps both because their source identity
        // (format, key, spec_path) differs.
        let ops = OpIndex::new(&[
            gql_op("Query.getUser", "QUERY", "getUser", "schema-a.graphql"),
            gql_op("Query.getUser", "QUERY", "getUser", "schema-b.graphql"),
        ]);
        let links = match_graphql_consumer(&consumed(OpType::Query, "getUser"), &ops);
        assert_eq!(
            links.len(),
            2,
            "two schemas declaring the key → two Ambiguous edges, got {links:?}"
        );
        for link in &links {
            assert_eq!(link.op_key, "Query.getUser");
            assert_eq!(link.tier, ConsumerTier::GraphqlField);
            assert_eq!(link.provenance, Provenance::Ambiguous);
            assert!(
                link.confidence < 0.40,
                "Ambiguous confidence must be < 0.40, got {}",
                link.confidence
            );
        }
    }

    #[test]
    fn graphql_miss_returns_empty() {
        let ops = OpIndex::new(&[gql_op(
            "Query.getUser",
            "QUERY",
            "getUser",
            "schema.graphql",
        )]);
        // A field no schema declares → no link (surfaced as unmatched).
        assert!(
            match_graphql_consumer(&consumed(OpType::Query, "nonExistentField"), &ops).is_empty(),
            "an undeclared GraphQL field yields no link"
        );
        // Right field name but WRONG op type (Mutation.getUser) → no match either.
        assert!(
            match_graphql_consumer(&consumed(OpType::Mutation, "getUser"), &ops).is_empty(),
            "op-type must match: Mutation.getUser != Query.getUser"
        );
    }

    #[test]
    fn graphql_matcher_ignores_openapi_ops_with_same_key() {
        // A GraphQL field must NEVER join to an OpenAPI op sharing the string key.
        let ops = OpIndex::new(&[
            // An OpenAPI op whose key happens to be "Query.getUser".
            op("Query.getUser", "GET", "/q/get-user", None),
        ]);
        assert!(
            match_graphql_consumer(&consumed(OpType::Query, "getUser"), &ops).is_empty(),
            "format filter: a GraphQL field must not match an OpenAPI op (R2/§2)"
        );
    }
}
