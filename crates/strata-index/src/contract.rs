//! Contract-plane assembly: `ApiOperation` nodes + producer (`PRODUCES`) edges.
//!
//! This runs *after* the code-plane graph is built (it consumes the route
//! declarations the code plane extracted and attaches to the function/module
//! nodes the code plane created). It is a pure, deterministic function of the
//! analyzed-file set and the extracted [`OperationDef`]s — no IO — so it is unit
//! testable in isolation and reproducible (design R3).
//!
//! **Honest provenance (R1).** A route does not *name* the operation it
//! implements; the link is a method+normalized-path convention match. So a
//! producer edge is `Inferred` (0.80, the Inferred ceiling) when exactly one
//! operation matches, `Ambiguous` (0.35) when several do, and **absent** when
//! none does (the route implements something the spec doesn't declare — surfaced
//! by its absence, never invented). It is never `Extracted`/`Resolved`.

use std::collections::{BTreeMap, BTreeSet};

use strata_contract::{
    match_consumer, match_graphql_consumer, normalize_path, parse_operations, ConsumerLink,
    ContractFormat, OpIndex, OperationDef,
};
use strata_core::{
    AnalyzedFile, Confidence, Edge, EdgeKind, Graph, Node, NodeKind, Provenance, RouteDecl, Span,
    Uid, ROUTE_METHOD_ANY,
};

/// The language/plane tag for contract-plane UIDs (distinct from code `"ts"`).
const CONTRACT_LANG: &str = "contract";

/// The code-plane language tag for a file path (`ts`/`py`/`cs`/`rust`), defaulting
/// to `ts` for an unrecognised extension (the historical contract default; a
/// non-code path has no code node to attach to anyway). Producer/consumer edges
/// must use the file's OWN language so the edge attaches to the node the code plane
/// actually built — a Python handler is `py`, not `ts`. (`.js`/`.jsx` map to `ts`,
/// matching the TS analyzer, so TS/JS edges are byte-identical to before.)
fn code_lang(path: &str) -> &'static str {
    crate::code_language_of(path).unwrap_or("ts")
}

/// The producer/consumer code node UID for `fqn` in `path`, language-tagged by the
/// file (cf. the code plane's own `uid_symbol`, which is hard-tagged `ts`).
fn code_symbol_uid(repo_name: &str, path: &str, fqn: &str) -> Uid {
    Uid::new(code_lang(path), repo_name, path, fqn, "")
}

/// The enclosing producer/consumer code node UID: the function `fqn` when present,
/// else the file's module node — both language-tagged by `path`.
fn code_enclosing_uid(repo_name: &str, path: &str, enclosing_fqn: &str) -> Uid {
    if enclosing_fqn.is_empty() {
        Uid::new(code_lang(path), repo_name, path, "<module>", "")
    } else {
        code_symbol_uid(repo_name, path, enclosing_fqn)
    }
}

/// Map an [`OperationDef`]'s format to the contract-plane [`NodeKind`]: OpenAPI
/// operations and **gRPC rpcs** are both [`ApiOperation`](NodeKind::ApiOperation)
/// (a gRPC rpc IS an api operation — no new node kind is warranted, brief §3),
/// GraphQL root fields are [`GraphqlField`](NodeKind::GraphqlField). All are
/// handled generically by the graph/traversal; only the node's kind label differs.
/// The `grpc` vs `openapi` distinction is carried by the estate UID's
/// [`format_discriminator`], not the node kind, so a gRPC `Foo.Get` and an OpenAPI
/// op of the same key still land on distinct canonical nodes.
fn node_kind_for(format: ContractFormat) -> NodeKind {
    match format {
        ContractFormat::OpenApi | ContractFormat::Grpc => NodeKind::ApiOperation,
        ContractFormat::Graphql => NodeKind::GraphqlField,
    }
}

/// Producer link, single unambiguous match: `Inferred`, at the band ceiling.
/// Provenance: Inferred (design §4.1 band 0.40–0.80). A convention match (route
/// method+path ⇒ operation) is a confident inference but NOT a fact — it never
/// reaches the Resolved/Extracted tiers.
pub const CONF_PRODUCES_SINGLE: f32 = 0.80;
/// Producer link, several candidate operations matched: `Ambiguous`.
/// Provenance: Ambiguous (design §4.1 band < 0.40). Recall-biased: we emit an
/// edge to each candidate rather than guess one or drop the link (R5).
pub const CONF_PRODUCES_MULTI: f32 = 0.35;
/// Producer link, **method-agnostic** unique match: a route declared without an
/// HTTP method (a Django `path()`/`re_path()`, [`ROUTE_METHOD_ANY`]) matched a
/// single operation by normalized **path alone**. `Inferred`, but **below**
/// [`CONF_PRODUCES_SINGLE`] — matching path without method is weaker evidence, and
/// the view may not implement every method declared at that path. Still in the
/// Inferred band (0.40–0.80); several path matches collapse to
/// [`CONF_PRODUCES_MULTI`] Ambiguous, never claiming one method over another (R5).
pub const CONF_PRODUCES_PATH_ONLY: f32 = 0.65;

/// Add the contract plane to an already-built code-plane graph: one
/// `ApiOperation` node per [`OperationDef`], then a `PRODUCES` edge from each
/// matching route's producer node to the operation(s) it matches.
///
/// `operations` are all the `OperationDef`s extracted from this repo's specs
/// (deduped by UID at insert time — a spec listing the same operation twice, or
/// two specs, collapse to one node). `analyzed` is the same map the code plane
/// was built from; its `routes` drive the producer links.
pub fn build_contract_plane(
    g: &mut Graph,
    repo_name: &str,
    analyzed: &BTreeMap<String, AnalyzedFile>,
    operations: &[OperationDef],
) {
    build_producer_plane(g, repo_name, analyzed, operations);
    build_graphql_producer_plane(g, repo_name, analyzed, operations);

    // ── Consumer edges (per-repo / monolith case, brief §3). ──
    //
    // Link this repo's consumer signals to this repo's LOCAL operations. The
    // op-uid resolver maps an operation `key` to the *local* operation node UID
    // (the same UID the producer plane created above). The estate pass (M3)
    // reuses [`link_consumers_into`] with a CANONICAL resolver + a seeded
    // existing-edge set; here we start from an empty set (no prior CONSUMES edge).
    let key_to_uid = local_key_to_uid(repo_name, operations);
    let ops = OpIndex::new(operations);
    let mut existing: BTreeSet<(Uid, Uid)> = BTreeSet::new();
    link_consumers_into(g, repo_name, analyzed, &ops, &key_to_uid, &mut existing);
}

/// Add the `ApiOperation` nodes + producer (`PRODUCES`) edges for one repo. This
/// is the M2 producer plane, unchanged; consumer linking (M3) is layered on top
/// by [`build_contract_plane`].
fn build_producer_plane(
    g: &mut Graph,
    repo_name: &str,
    analyzed: &BTreeMap<String, AnalyzedFile>,
    operations: &[OperationDef],
) {
    // ── ApiOperation nodes (Extracted 1.0). Idempotent by UID. ──
    for op in operations {
        let uid = operation_uid(repo_name, op);
        // name = operationId, else "METHOD path"; fqn = the cross-repo key;
        // path = the raw HTTP path (per the brief: store method/path on the node).
        let name = op
            .operation_id
            .clone()
            .unwrap_or_else(|| format!("{} {}", op.method, op.path));
        g.add_node(Node {
            uid,
            kind: node_kind_for(op.format),
            name,
            fqn: op.key.clone(),
            path: op.path.clone(),
            span: Span::default(),
            provenance: Provenance::Extracted,
            confidence: Confidence::new(1.0),
        });
    }

    // ── Producer edges. Match each route to operation(s) by method + norm_path. ──
    //
    // Iterate files/routes in sorted order (BTreeMap + Vec) for determinism.
    for (path, file) in analyzed {
        for route in &file.routes {
            let route_norm = normalize_path(&route.path);
            // A method-less route (Django `path()`/`re_path()`, method ==
            // ROUTE_METHOD_ANY) matches on normalized path alone — its view
            // dispatches methods internally; a method-bearing route (Flask/FastAPI/
            // Express) matches on method AND path. (Operation methods are already
            // upper-cased by the adapter.)
            let method_agnostic = route.method == ROUTE_METHOD_ANY;
            let matches: Vec<&OperationDef> = operations
                .iter()
                .filter(|op| {
                    op.norm_path == route_norm && (method_agnostic || op.method == route.method)
                })
                .collect();

            if matches.is_empty() {
                // No matching operation: the route implements something not in
                // the spec. No edge — surfaced by absence, never invented (R1).
                continue;
            }

            let src = producer_uid(g, repo_name, path, file, route);
            // A unique match is Inferred; several are Ambiguous. A method-agnostic
            // (path-only) unique match is a *weaker* inference than a method+path
            // one, so it lands at its own lower Inferred tier (CONF_PRODUCES_PATH_ONLY).
            let (prov, conf) = match (matches.len(), method_agnostic) {
                (1, true) => (Provenance::Inferred, CONF_PRODUCES_PATH_ONLY),
                (1, false) => (Provenance::Inferred, CONF_PRODUCES_SINGLE),
                _ => (Provenance::Ambiguous, CONF_PRODUCES_MULTI),
            };
            for op in matches {
                let dst = operation_uid(repo_name, op);
                g.add_edge(Edge {
                    src: src.clone(),
                    dst,
                    kind: EdgeKind::Produces,
                    provenance: prov,
                    confidence: Confidence::new(conf),
                });
            }
        }
    }
}

/// Add GraphQL producer (`PRODUCES`) edges from resolver-map entries to the
/// `GraphqlField` operation(s) they implement. The `GraphqlField` nodes already
/// exist (created by [`build_producer_plane`]'s node loop via `node_kind_for`);
/// this only adds the edges.
///
/// Each [`ResolverEntry`](strata_core::ResolverEntry) keys on the canonical
/// `"<op_type>.<field>"` and matches GraphQL-format operations with that key:
/// exactly one → `PRODUCES` `Inferred` (0.80) from the handler node; the key
/// declared in several schemas → `Ambiguous` (0.35) each; none → no edge
/// (surfaced by absence — the resolver implements a field no schema declares).
fn build_graphql_producer_plane(
    g: &mut Graph,
    repo_name: &str,
    analyzed: &BTreeMap<String, AnalyzedFile>,
    operations: &[OperationDef],
) {
    for (path, file) in analyzed {
        for entry in &file.resolver_entries {
            let key = format!("{}.{}", entry.op_type, entry.field);
            // Candidate operations: GraphQL-format with the canonical key.
            let matches: Vec<&OperationDef> = operations
                .iter()
                .filter(|op| op.format == ContractFormat::Graphql && op.key == key)
                .collect();
            if matches.is_empty() {
                continue; // resolver implements a field no schema declares — no edge.
            }
            let src = handler_or_enclosing_uid(
                g,
                repo_name,
                path,
                file,
                entry.handler_name.as_deref(),
                &entry.enclosing_fqn,
            );
            let (prov, conf) = if matches.len() == 1 {
                (Provenance::Inferred, CONF_PRODUCES_SINGLE)
            } else {
                (Provenance::Ambiguous, CONF_PRODUCES_MULTI)
            };
            for op in matches {
                let dst = operation_uid(repo_name, op);
                g.add_edge(Edge {
                    src: src.clone(),
                    dst,
                    kind: EdgeKind::Produces,
                    provenance: prov,
                    confidence: Confidence::new(conf),
                });
            }
        }
    }
}

/// Build a full graph — the slice-1/2 code plane **plus** the contract plane —
/// from an in-memory analyzed-file set and the operations extracted from specs.
///
/// A convenience for hermetic tests (and any in-memory caller) that want the
/// producer edges without reading a repo from disk. The heuristic code plane is
/// used (no SCIP); the contract plane is layered on top exactly as `index_impl`
/// does it.
pub fn assemble_graph_with_contracts(
    analyzed: &BTreeMap<String, AnalyzedFile>,
    repo_name: &str,
    opts: &strata_lang_ts::ResolveOptions,
    operations: &[OperationDef],
) -> Graph {
    let mut g = crate::build::assemble_graph(analyzed, repo_name, opts);
    build_contract_plane(&mut g, repo_name, analyzed, operations);
    g
}

// ── Consumer linking (shared by per-repo and estate passes) ──────────────────

/// Consumer link, single unambiguous match. Provenance: Inferred (band
/// 0.40–0.80). A name- or URL-convention match is a confident inference, never a
/// fact — it never reaches Resolved/Extracted. The numeric tier is the
/// `match_consumer` confidence (operationId 0.75, literal 0.70, template 0.60).
const CONSUMES_INFERRED_CEILING: f32 = 0.80;
/// The Ambiguous band ceiling is exclusive (< 0.40); a multi-candidate consumer
/// link is clamped to this defensively. The real Ambiguous tier (0.35) is
/// already below it, so the clamp is a no-op for valid `match_consumer` output.
const AMBIGUOUS_CEILING: f32 = 0.39;
/// The Extracted band floor (0.95). A GraphQL document *names* the contract in
/// its own language, so a unique match is `Extracted` (`match_graphql_consumer`
/// returns 0.95). This floor is enforced defensively so such a link can never
/// drop below its band — the dual of the Inferred *ceiling* above.
const CONSUMES_EXTRACTED_FLOOR: f32 = 0.95;
/// The confidence of an **api fan-out** consumer edge (B6 fix). When a consumer's
/// matched `(format, key)` is owned by SEVERAL apis in the estate (e.g. two
/// unrelated services both declaring `GET /health`), we cannot honestly pick one,
/// so we emit one `Ambiguous` edge per owning api at this tier — recall-biased,
/// band-correct (< 0.40), `ambiguous: true` — rather than a silent confident pick
/// (the false merge B6 fixed). Equal to the `Ambiguous` tier used everywhere else.
const CONSUMES_AMBIGUOUS_FANOUT: f32 = 0.35;

/// Add every `CONSUMES` edge implied by this analyzed-file set's consumer signals
/// (each file's `http_calls` and ordinary `CallRef`s) against the operations in
/// `ops`, resolving each matched `key` to an operation node UID via `key_to_uid`.
///
/// This is the single shared linker (brief §3 monolith + §4 cross-repo): the
/// per-repo pass passes a LOCAL `key → uid` map; the estate pass passes a
/// CANONICAL map and a pre-seeded `existing` set so it only ADDS cross-repo
/// edges not already created locally. `existing` accumulates `(src, dst)` pairs
/// to keep edges de-duplicated across both passes.
///
/// A matched `key` whose UID is not in `key_to_uid` (or whose node is absent
/// from the graph) is skipped — never an edge to a nonexistent operation. The
/// consumer node is the enclosing function/module; a `CONSUMES` edge is only
/// added when that consumer node exists in the graph (it always does for code
/// the plane built, but the guard keeps the estate pass robust, R2).
///
/// Deterministic: files iterate in `BTreeMap` order, signals in source order,
/// matched links in `op_key` order (`match_consumer` sorts them).
pub(crate) fn link_consumers_into(
    g: &mut Graph,
    repo_name: &str,
    analyzed: &BTreeMap<String, AnalyzedFile>,
    ops: &OpIndex,
    key_to_uid: &BTreeMap<(ContractFormat, String), Vec<Uid>>,
    existing: &mut BTreeSet<(Uid, Uid)>,
) {
    for (path, file) in analyzed {
        // Signal A: outgoing HTTP calls (`fetch`/`axios`) → URL-shape tiers.
        for http in &file.http_calls {
            let links = match_consumer(None, Some(http), ops);
            let consumer = code_enclosing_uid(repo_name, path, &http.enclosing_fqn);
            add_consumer_links(g, &consumer, &links, key_to_uid, existing);
        }
        // Signal B: ordinary calls whose callee name equals an operationId.
        for call in &file.calls {
            let links = match_consumer(Some(&call.callee_name), None, ops);
            if links.is_empty() {
                continue;
            }
            let consumer = code_enclosing_uid(repo_name, path, &call.enclosing_fqn);
            add_consumer_links(g, &consumer, &links, key_to_uid, existing);
        }
        // Signal C: GraphQL documents → GraphQL fields. Parse-gated: a document
        // is linked only when `parse_operations` succeeds, at which point a TAGGED
        // and an UNTAGGED document are evidence-identical (the parse IS the proof
        // it is GraphQL), so both use the same `match_graphql_consumer` tiers
        // (Extracted 0.95 unique / Ambiguous 0.35 multi). The link decision is the
        // same for either provenance; only the *unparsed accounting* differs (a
        // tagged parse failure is counted in coverage, an untagged one is silently
        // dropped — see `compute_coverage`). An interpolated TAGGED template is
        // unreliable and never parsed; an untagged candidate is always
        // interpolation-free by construction.
        for doc in &file.gql_documents {
            if !doc.interpolation_free {
                continue; // interpolated tagged template: counted, never linked.
            }
            let Ok(consumption) = parse_operations(path, &doc.text) else {
                // Parse failure: no links for either provenance. A tagged doc is
                // additionally counted unparsed by `compute_coverage`; an untagged
                // candidate is not (it never claimed to be GraphQL).
                continue;
            };
            if consumption.fields.is_empty() {
                continue;
            }
            let consumer = code_enclosing_uid(repo_name, path, &doc.enclosing_fqn);
            for field in &consumption.fields {
                let links = match_graphql_consumer(field, ops);
                add_consumer_links(g, &consumer, &links, key_to_uid, existing);
            }
        }
    }
}

/// Materialize `links` as `CONSUMES` edges from `consumer`, de-duped via
/// `existing`. Skips a link whose operation UID is unknown or whose endpoint
/// nodes are absent (never an edge to a phantom node).
///
/// **Api fan-out (B6 fix).** `key_to_uid` maps a matched `(format, key)` to the
/// canonical node(s) that own it. When a key is owned by exactly one api the link
/// keeps its own provenance/confidence (the unique case is byte-identical to
/// before). When several apis own it — two unrelated services declaring the same
/// key — we emit one `Ambiguous` [`CONSUMES_AMBIGUOUS_FANOUT`] (0.35) edge per
/// owning api: recall-biased and honestly flagged, never a silent confident pick.
fn add_consumer_links(
    g: &mut Graph,
    consumer: &Uid,
    links: &[ConsumerLink],
    key_to_uid: &BTreeMap<(ContractFormat, String), Vec<Uid>>,
    existing: &mut BTreeSet<(Uid, Uid)>,
) {
    // A consumer signal in a file the code plane built always has its enclosing
    // node; guard anyway so a stray signal can never invent an edge endpoint.
    if g.get_node(consumer).is_none() {
        return;
    }
    for link in links {
        // Resolve the link's `op_key` to the node(s) of the link's *format* (its
        // tier tells us): a GraphQL field never resolves to an OpenAPI op of the
        // same key string, and vice versa.
        let Some(op_uids) = key_to_uid.get(&(link.tier.format(), link.op_key.clone())) else {
            continue; // matched a key we have no node for — skip, never invent.
        };
        // Several owning apis → an Ambiguous fan-out (one edge per api). A single
        // owner keeps the link's own honest tier.
        let fan_out = op_uids.len() > 1;
        for op_uid in op_uids {
            if g.get_node(op_uid).is_none() {
                continue;
            }
            if !existing.insert((consumer.clone(), op_uid.clone())) {
                continue; // edge already added (locally or by an earlier signal).
            }
            let (provenance, conf) = if fan_out {
                // Cannot honestly pick one of several apis: Ambiguous 0.35 each.
                (Provenance::Ambiguous, CONSUMES_AMBIGUOUS_FANOUT)
            } else {
                // Unique owner: keep the matcher's tier, enforcing the §4.1 band
                // invariant defensively. The matchers already return band-
                // respecting tiers (Extracted 0.95 for a unique GraphQL doc,
                // Inferred ≤ 0.75 for REST, Ambiguous 0.35); these caps/floors
                // guarantee the invariant even if a tier constant were mis-set,
                // without distorting an in-band value.
                let c = match link.provenance {
                    Provenance::Ambiguous => link.confidence.min(AMBIGUOUS_CEILING),
                    Provenance::Extracted => link.confidence.clamp(CONSUMES_EXTRACTED_FLOOR, 1.0),
                    _ => link.confidence.min(CONSUMES_INFERRED_CEILING),
                };
                (link.provenance, c)
            };
            g.add_edge(Edge {
                src: consumer.clone(),
                dst: op_uid.clone(),
                kind: EdgeKind::Consumes,
                provenance,
                confidence: Confidence::new(conf),
            });
        }
    }
}

/// Map each operation `(format, key)` to its **local** node UID for this repo.
/// The first occurrence of an identity wins (matching the producer plane's
/// idempotent node insertion). Keying on `(format, key)` — not bare `key` — keeps
/// a GraphQL field and an OpenAPI op that share a key string on distinct nodes.
///
/// Returns a single-element `Vec` per key: within ONE repo a `(format, key)` is
/// always one node, so the per-repo consumer pass never fans out (the unique
/// case, byte-identical to before). Cross-api fan-out only arises estate-wide,
/// where a key can be owned by several apis (see `estate::link_estate`).
fn local_key_to_uid(
    repo_name: &str,
    operations: &[OperationDef],
) -> BTreeMap<(ContractFormat, String), Vec<Uid>> {
    let mut map: BTreeMap<(ContractFormat, String), Vec<Uid>> = BTreeMap::new();
    for op in operations {
        map.entry((op.format, op.key.clone()))
            .or_insert_with(|| vec![operation_uid(repo_name, op)]);
    }
    map
}

/// The UID of the `ApiOperation` node for `op`: keyed by the cross-repo `key`
/// within its spec file, so the same `operationId` from the same spec collapses
/// to one node. (Cross-*repo* dedup-by-key is Milestone 3.)
pub(crate) fn operation_uid(repo_name: &str, op: &OperationDef) -> strata_core::Uid {
    strata_core::Uid::new(CONTRACT_LANG, repo_name, &op.spec_path, &op.key, "")
}

/// The estate-wide UID discriminator for a contract format, rendered
/// **explicitly** as `openapi` | `graphql` | `grpc`.
///
/// Slice 8 (B6 fix) made this explicit on purpose. Previously OpenAPI used the
/// *empty* string (so a canonical OpenAPI UID was byte-identical to slice 3,
/// `contract|estate||key|`); that implicit discriminator is gone — canonical
/// UIDs are keyed on `(api_id, format, key)` now, and a stable explicit token per
/// format keeps the `{api_id}/{format}` path slot unambiguous. The format part
/// alone still guarantees a GraphQL `Query.getUser`, an OpenAPI op, and a gRPC
/// `Foo.Get` sharing a key string never collide; the `api_id` prefix additionally
/// keeps two *unrelated* APIs of the same format from merging (the B6 false merge).
fn format_discriminator(format: ContractFormat) -> &'static str {
    match format {
        ContractFormat::OpenApi => "openapi",
        ContractFormat::Graphql => "graphql",
        ContractFormat::Grpc => "grpc",
    }
}

/// The **canonical** (estate-wide) UID of the operation for `(api_id, format,
/// key)`: keyed by the estate name and the `{api_id}/{format}` discriminator (no
/// spec path), so the same `(api_id, format, key)` from any repo collapses to one
/// node across the estate, while two *unrelated* APIs that share a key string —
/// even of the same format — land on **distinct** canonical nodes (B6 fix).
///
/// `api_id` is the manifest-declared `[[repos.apis]]` id when a declared `spec`
/// owns the operation, else the repo name (see `estate::resolve_api_id`). Per-repo
/// (non-estate) contract uids are unaffected — they keep [`operation_uid`].
pub(crate) fn canonical_operation_uid(
    estate_name: &str,
    api_id: &str,
    format: ContractFormat,
    key: &str,
) -> Uid {
    Uid::new(
        CONTRACT_LANG,
        estate_name,
        &format!("{api_id}/{}", format_discriminator(format)),
        key,
        "",
    )
}

/// The canonical operation node for `op` in the estate graph: identical to the
/// per-repo node (Extracted 1.0, name = operationId|"METHOD path", path = raw
/// HTTP path, fqn = key) but with the estate-wide api-scoped
/// [`canonical_operation_uid`]. `api_id` is the operation's resolved api identity.
pub(crate) fn canonical_operation_node(estate_name: &str, api_id: &str, op: &OperationDef) -> Node {
    let name = op
        .operation_id
        .clone()
        .unwrap_or_else(|| format!("{} {}", op.method, op.path));
    Node {
        uid: canonical_operation_uid(estate_name, api_id, op.format, &op.key),
        kind: node_kind_for(op.format),
        name,
        fqn: op.key.clone(),
        path: op.path.clone(),
        span: Span::default(),
        provenance: Provenance::Extracted,
        confidence: Confidence::new(1.0),
    }
}

/// The producer node a route's `PRODUCES` edge originates from. Thin wrapper over
/// [`handler_or_enclosing_uid`] using the route's handler/enclosing.
fn producer_uid(
    g: &Graph,
    repo_name: &str,
    path: &str,
    file: &AnalyzedFile,
    route: &RouteDecl,
) -> Uid {
    handler_or_enclosing_uid(
        g,
        repo_name,
        path,
        file,
        route.handler_name.as_deref(),
        &route.enclosing_fqn,
    )
}

/// The producer node a `PRODUCES` edge originates from, given an optional named
/// handler and the declaring scope. Shared by route producers (REST) and
/// resolver-map producers (GraphQL).
///
/// Prefer the **named handler**'s function node when `handler_name` is an
/// identifier that resolves to a Function/Method symbol *in the same file*
/// (`app.get("/x", getUser)` / `{ Query: { getUser } }` ⇒ the `getUser` node). A
/// top-level handler is the common case; if several symbols share the name, a
/// top-level (un-nested) one is preferred. When the handler is inline, unnamed,
/// or not a symbol in this file, fall back to the declaring `enclosing_fqn`
/// function / module node — the same attribution the call graph uses for a call
/// with no resolvable target.
fn handler_or_enclosing_uid(
    g: &Graph,
    repo_name: &str,
    path: &str,
    file: &AnalyzedFile,
    handler_name: Option<&str>,
    enclosing_fqn: &str,
) -> Uid {
    if let Some(handler) = handler_name {
        // Find a Function/Method symbol named `handler` in this file, preferring
        // a top-level one (no container) for a stable, unambiguous target.
        let mut best: Option<&str> = None;
        for sym in &file.symbols {
            if matches!(sym.kind, NodeKind::Function | NodeKind::Method) && sym.name == handler {
                let top_level = sym.container_fqn.is_none();
                if best.is_none() || top_level {
                    best = Some(&sym.fqn);
                    if top_level {
                        break;
                    }
                }
            }
        }
        if let Some(fqn) = best {
            let candidate = code_symbol_uid(repo_name, path, fqn);
            if g.get_node(&candidate).is_some() {
                return candidate;
            }
        }
    }
    // Fall back to the enclosing function/module node.
    code_enclosing_uid(repo_name, path, enclosing_fqn)
}
