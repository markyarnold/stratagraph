//! Transport-independent MCP tool dispatch.
//!
//! [`call_tool`] maps a `(graph, tool name, args)` triple to a JSON result
//! payload — no IO, no MCP framing. This is the part that must be correct, and
//! it is exercised directly by unit tests without any live MCP client.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use strata_core::{
    context, explain, impact, query, Direction, EdgeKind, Explanation, Graph, ImpactOptions, Node,
    NodeKind, Provenance,
};
use strata_index::{blast_for_file, detect_changes, rename, ChangeScope, RenameOptions};

use crate::resolve::{resolve_symbol, ResolveOutcome};

/// The ambient context a tool call may need beyond the loaded graph.
///
/// Most tools (`context`/`impact`/`query`) are pure functions of the graph and
/// ignore this entirely. The filesystem-touching tools (`detect_changes`,
/// `rename`) need the repository root for git/IO; it lives here so the dispatch
/// signature stays uniform. [`Default`] is `repo_root: None`, `member_roots:
/// vec![]` — the ctx-less [`call_tool`] path, which makes those tools return a
/// clear "needs a repo root" error rather than guessing.
#[derive(Debug, Clone, Default)]
pub struct ToolCtx {
    /// The repository working directory, when the server knows it (derived from
    /// the `--db` path or an explicit `--repo`). `None` over the ctx-less path.
    pub repo_root: Option<PathBuf>,
    /// Every estate member's repo root, when the server is serving a linked
    /// workspace graph (`--workspace`) — filled from the manifest's `[[repos]]`
    /// paths by the CLI's workspace-mode server construction. Empty in
    /// single-repo mode. Additive: only `search_docs` reads it today (estate
    /// fan-out — every member's own `.strata/docs.idx` is searched and merged),
    /// independent of `repo_root`'s single-member meaning used by
    /// `detect_changes`/`rename`.
    pub member_roots: Vec<PathBuf>,
}

/// Errors a tool call can fail with. Mapped to MCP `isError` results by the server.
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("symbol not found: {0}")]
    NotFound(String),
    #[error("ambiguous symbol {0}: {1} candidates")]
    Ambiguous(String, usize),
    #[error("bad arguments: {0}")]
    BadArgs(String),
}

/// Compact JSON view of a node used throughout the tool payloads.
fn node_json(n: &Node) -> Value {
    json!({
        "uid": n.uid.as_str(),
        "name": n.name,
        "kind": kind_name(n.kind),
        "path": n.path,
    })
}

fn kind_name(kind: NodeKind) -> String {
    // Reuse serde's unit-variant name (e.g. "Function") without the quotes.
    serde_json::to_value(kind)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| format!("{kind:?}"))
}

fn edge_kind_name(kind: EdgeKind) -> String {
    serde_json::to_value(kind)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| format!("{kind:?}"))
}

/// The serde name of a [`Provenance`] variant (e.g. `"Extracted"`, `"Ambiguous"`)
/// without the JSON quotes — used in the `explain` hop payload so the agent sees
/// the same provenance vocabulary the graph uses.
fn provenance_name(prov: strata_core::Provenance) -> String {
    serde_json::to_value(prov)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| format!("{prov:?}"))
}

/// Compact JSON view of a [`strata_core::ContextDocRef`] — `context`'s `docs`
/// bucket entry: refs-only (uid/name/anchor/path/provenance/confidence), never
/// body text.
fn doc_ref_json(d: &strata_core::ContextDocRef) -> Value {
    json!({
        "uid": d.uid.as_str(),
        "name": d.name,
        "anchor": d.anchor,
        "path": d.path,
        "provenance": provenance_name(d.provenance),
        "confidence": d.confidence,
    })
}

/// Read a required string argument from the tool's `args` object.
fn require_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, ToolError> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::BadArgs(format!("missing string field `{key}`")))
}

/// The outcome of resolving a tool's symbol when ambiguity should be *surfaced*
/// (the candidate-on-`Many` contract `context` pioneered) rather than errored.
enum NodeOrCandidates {
    /// A single resolved node — run the tool normally.
    Node(Node),
    /// Several matches; the caller returns them as a candidates payload so the
    /// agent can pin one with `uid`. Never a silent pick.
    Candidates(Vec<Node>),
}

/// Resolve a tool's `symbol` to a node OR a candidate list, honouring an optional
/// `uid` pin — the shared resolver `impact`/`explain` use so an ambiguous symbol
/// lists candidates instead of dead-ending (mirroring `tool_context`).
///
/// * a `uid` pin (read from `uid_key`) → [`Graph::get_node`] (a missing uid is a
///   clear [`ToolError::NotFound`] — never a silent fall-back to name resolution);
/// * else `resolve_symbol`: `One` → the node, `None` → `NotFound`, `Many` →
///   [`NodeOrCandidates::Candidates`].
fn resolve_or_candidates(
    graph: &Graph,
    args: &Value,
    symbol: &str,
    uid_key: &str,
) -> Result<NodeOrCandidates, ToolError> {
    if let Some(uid) = opt_str(args, uid_key)? {
        return graph
            .get_node(&strata_core::Uid(uid.to_string()))
            .cloned()
            .map(NodeOrCandidates::Node)
            .ok_or_else(|| ToolError::NotFound(uid.to_string()));
    }
    match resolve_symbol(graph, symbol) {
        ResolveOutcome::One(n) => Ok(NodeOrCandidates::Node(n)),
        ResolveOutcome::None => Err(ToolError::NotFound(symbol.to_string())),
        ResolveOutcome::Many(c) => Ok(NodeOrCandidates::Candidates(c)),
    }
}

/// The shared ambiguity payload `context`/`impact`/`explain` all emit on a `Many`
/// resolution: `{"ambiguous":true,"symbol":…,"candidates":[node_json,…]}`. One
/// shape, so an agent disambiguates the same way across every tool. `extra` adds
/// tool-specific keys (e.g. `explain`'s `ambiguous_end`).
fn candidates_payload(symbol: &str, candidates: &[Node], extra: &[(&str, Value)]) -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert("ambiguous".into(), json!(true));
    obj.insert("symbol".into(), json!(symbol));
    obj.insert(
        "candidates".into(),
        json!(candidates.iter().map(node_json).collect::<Vec<_>>()),
    );
    for (k, v) in extra {
        obj.insert((*k).to_string(), v.clone());
    }
    Value::Object(obj)
}

/// Read an optional string argument, erroring if present but not a string.
fn opt_str<'a>(args: &'a Value, key: &str) -> Result<Option<&'a str>, ToolError> {
    match args.get(key) {
        None => Ok(None),
        Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_str()
            .map(Some)
            .ok_or_else(|| ToolError::BadArgs(format!("`{key}` must be a string"))),
    }
}

/// Dispatch a tool call over an already-loaded graph, returning the JSON result.
///
/// The ctx-less entry point: delegates to [`call_tool_ctx`] with a default
/// (empty) [`ToolCtx`]. The graph-only tools (`context`/`impact`/`query`) are
/// fully served here and are **byte-identical** to before the ctx existed; a
/// filesystem tool (`detect_changes`) reached this way gets a clear "needs a
/// repo root" error (it cannot guess the working tree from a graph alone).
///
/// Supported graph-only tools: `context`, `impact`, `query`. Any other name is
/// [`ToolError::BadArgs`].
pub fn call_tool(graph: &Graph, name: &str, args: &Value) -> Result<Value, ToolError> {
    call_tool_ctx(graph, &ToolCtx::default(), name, args)
}

/// Dispatch a tool call with an ambient [`ToolCtx`] (carrying the repo root for
/// the filesystem-touching tools), returning the JSON result.
///
/// Supported tools: `context`, `impact`, `explain`, `query`, `blast` (graph-only,
/// ignore the ctx), `detect_changes`/`rename` (need `ctx.repo_root`), and
/// `search_docs` (lexical-only — needs `ctx.repo_root`/`ctx.member_roots`,
/// ignores `graph` entirely: it reads the tantivy docs index, not the code
/// graph). Any other name is [`ToolError::BadArgs`].
pub fn call_tool_ctx(
    graph: &Graph,
    ctx: &ToolCtx,
    name: &str,
    args: &Value,
) -> Result<Value, ToolError> {
    match name {
        "context" => tool_context(graph, args),
        "impact" => tool_impact(graph, args),
        "explain" => tool_explain(graph, args),
        "query" => tool_query(graph, args),
        "blast" => tool_blast(graph, args),
        "detect_changes" => tool_detect_changes(graph, ctx, args),
        "rename" => tool_rename(graph, ctx, args),
        "search_docs" => tool_search_docs(ctx, args),
        "guidance" => tool_guidance(graph, ctx, args),
        other => Err(ToolError::BadArgs(format!("unknown tool: {other}"))),
    }
}

fn tool_context(graph: &Graph, args: &Value) -> Result<Value, ToolError> {
    let symbol = require_str(args, "symbol")?;
    // For context we surface ambiguity as a candidate list payload rather than
    // an error, so the agent can pick. None still errors (nothing to show).
    match resolve_symbol(graph, symbol) {
        ResolveOutcome::None => Err(ToolError::NotFound(symbol.to_string())),
        // The shared ambiguity payload — byte-identical to before, now via the
        // helper `impact`/`explain` reuse so all three emit the one shape.
        ResolveOutcome::Many(c) => Ok(candidates_payload(symbol, &c, &[])),
        ResolveOutcome::One(node) => {
            // context() is Some: the uid came from the graph.
            let ctx = context(graph, &node.uid)
                .ok_or_else(|| ToolError::BadArgs("resolved node vanished".into()))?;
            Ok(json!({
                "node": node_json(&ctx.node),
                "callers": ctx.callers.iter().map(node_json).collect::<Vec<_>>(),
                "callees": ctx.callees.iter().map(node_json).collect::<Vec<_>>(),
                "imports_in": ctx.imports_in.iter().map(node_json).collect::<Vec<_>>(),
                "imports_out": ctx.imports_out.iter().map(node_json).collect::<Vec<_>>(),
                "members": ctx.members.iter().map(node_json).collect::<Vec<_>>(),
                "container": ctx.container.as_ref().map(node_json),
                // Contract plane (additive): the relationships that apply to a
                // schema field/operation — incoming PRODUCES/CONSUMES and the
                // outgoing producer/consumer views.
                "producers": ctx.producers.iter().map(node_json).collect::<Vec<_>>(),
                "consumers": ctx.consumers.iter().map(node_json).collect::<Vec<_>>(),
                "produces": ctx.produces.iter().map(node_json).collect::<Vec<_>>(),
                "consumes": ctx.consumes.iter().map(node_json).collect::<Vec<_>>(),
                // Infra plane (additive): the wiring that applies to a role/
                // datasource/Lambda/handler-module — a role's `assumed_by` lists
                // its Lambdas, the resolver→DS→lambda chain shows from both ends,
                // a handler module's `run_by` lists its Lambda.
                "assumes": ctx.assumes.iter().map(node_json).collect::<Vec<_>>(),
                "assumed_by": ctx.assumed_by.iter().map(node_json).collect::<Vec<_>>(),
                "routes_to": ctx.routes_to.iter().map(node_json).collect::<Vec<_>>(),
                "routed_from": ctx.routed_from.iter().map(node_json).collect::<Vec<_>>(),
                "runs": ctx.runs.iter().map(node_json).collect::<Vec<_>>(),
                "run_by": ctx.run_by.iter().map(node_json).collect::<Vec<_>>(),
                // Data plane (Slice 25, D3, M2b): a Table's `mapped_by` lists the ORM
                // model classes that map to it; a model class's `maps_to` is its table.
                "mapped_by": ctx.mapped_by.iter().map(node_json).collect::<Vec<_>>(),
                "maps_to": ctx.maps_to.iter().map(node_json).collect::<Vec<_>>(),
                // Knowledge plane (K6): every doc section that documents or mentions
                // this node, refs-only (never body text — `guidance` fetches it).
                "docs": ctx.docs.iter().map(doc_ref_json).collect::<Vec<_>>(),
            }))
        }
    }
}

fn tool_impact(graph: &Graph, args: &Value) -> Result<Value, ToolError> {
    let symbol = require_str(args, "symbol")?;
    // Ambiguity is SURFACED, not errored: an ambiguous symbol returns the
    // candidate list (mirroring `context`) so the agent pins one with `uid`,
    // instead of dead-ending on a bare count.
    let node = match resolve_or_candidates(graph, args, symbol, "uid")? {
        NodeOrCandidates::Node(n) => n,
        NodeOrCandidates::Candidates(c) => return Ok(candidates_payload(symbol, &c, &[])),
    };

    // `depth`/`min_confidence`/`include_contracts`/`include_infra` — the same
    // option parsing `explain` uses, so both tools walk the graph identically.
    let opts = impact_opts_from_args(args)?;

    let result = impact(graph, &node.uid, &opts);
    let affected: Vec<Value> = result
        .affected
        .iter()
        .map(|a| {
            // Additive (K7 fix F1): the affected node's kind, looked up from the
            // graph by uid — the same `kind_name` vocabulary `node_json`/`context`
            // already use, and the same shape `detect_changes`' own AffectedNode
            // (strata_index::changes) already carries. Lets a caller recognize a
            // `Doc`/`DocSection` dependent and apply the steering's downgrade
            // (doc-kind ⇒ "needs review", never WILL BREAK) instead of trusting
            // the mechanical `will_break` bool blindly — see docs/src/reference/mcp.md.
            // A uid that has vanished from the graph (should not happen — impact
            // only ever returns real node uids) degrades to "Unknown" rather than
            // panicking or dropping the entry.
            let kind = graph
                .get_node(&a.uid)
                .map(|n| kind_name(n.kind))
                .unwrap_or_else(|| "Unknown".to_string());
            json!({
                "uid": a.uid.as_str(),
                "name": a.name,
                "kind": kind,
                "depth": a.depth,
                "confidence": a.confidence,
                "ambiguous": a.ambiguous,
                // Additive (§15.6): the derived will-break verdict. Existing keys
                // above are byte-identical; only this field is new.
                "will_break": a.will_break,
            })
        })
        .collect();

    let mut out = serde_json::Map::new();
    out.insert("target".into(), node_json(&node));
    out.insert("affected".into(), json!(affected));
    // Honest surfacing for a member-bearing target whose own blast radius is empty:
    // the members that THEMSELVES have a dependent (so an agent pins one and re-runs
    // instead of reading a misleading empty result). Mirrors the candidates pattern —
    // a structured field, present ONLY on the zero-direct case (the engine populates
    // it there only), so the normal non-empty-`affected` result shape is unchanged.
    // A listed member is a real graph dependent of that member — never framed as
    // "the type's direct dependents".
    if !result.members_with_dependents.is_empty() {
        let members: Vec<Value> = result
            .members_with_dependents
            .iter()
            .map(|m| {
                json!({
                    "uid": m.uid.as_str(),
                    "name": m.name,
                    "kind": kind_name(m.kind),
                })
            })
            .collect();
        out.insert("members_with_dependents".into(), json!(members));
    }
    Ok(Value::Object(out))
}

/// Read the impact options (`depth`/`min_confidence`/`include_contracts`/
/// `include_infra`) shared by the `impact` and `explain` tools off the args
/// object, starting from [`ImpactOptions::default`]. So `explain` walks the
/// SAME graph as `impact` under the same toggles — the consistency invariant
/// holds through the dispatch, not just the engine.
fn impact_opts_from_args(args: &Value) -> Result<ImpactOptions, ToolError> {
    let mut opts = ImpactOptions::default();
    if let Some(d) = args.get("depth") {
        let d = d
            .as_u64()
            .ok_or_else(|| ToolError::BadArgs("`depth` must be a non-negative integer".into()))?;
        opts.max_depth = d as usize;
    }
    if let Some(c) = args.get("min_confidence") {
        let c = c
            .as_f64()
            .ok_or_else(|| ToolError::BadArgs("`min_confidence` must be a number".into()))?;
        opts.min_confidence = c as f32;
    }
    if let Some(ic) = args.get("include_contracts") {
        opts.include_contracts = ic
            .as_bool()
            .ok_or_else(|| ToolError::BadArgs("`include_contracts` must be a boolean".into()))?;
    }
    if let Some(ii) = args.get("include_infra") {
        opts.include_infra = ii
            .as_bool()
            .ok_or_else(|| ToolError::BadArgs("`include_infra` must be a boolean".into()))?;
    }
    Ok(opts)
}

/// Serialize an [`Explanation`]'s hops as JSON. Each hop carries the `from`/`to`
/// uids and the edge's kind/provenance/confidence plus the running (accumulated)
/// confidence after that hop — the visible form of the never-confident-wrong
/// thesis.
fn explanation_hops_json(explanation: &Explanation) -> Vec<Value> {
    explanation
        .hops
        .iter()
        .map(|h| {
            json!({
                "from": h.from.as_str(),
                "to": h.to.as_str(),
                "edge_kind": edge_kind_name(h.edge_kind),
                "provenance": provenance_name(h.provenance),
                "confidence": h.confidence,
                "running_confidence": h.running_confidence,
            })
        })
        .collect()
}

/// The `explain` tool: **why is B in A's blast radius?** Resolves `symbol` (the
/// changed target, alias `target`) and `affected` like `impact`/`context`, then
/// runs [`strata_core::explain`] — the SAME reverse walk `impact` uses — and
/// returns the evidence chain.
///
/// Honest outcomes:
/// * `affected` is **not reachable** → `{ "reachable": false, … }` (not in the
///   blast radius — an explicit "nothing to explain", never an empty success);
/// * `target == affected` → `reachable: true` with an empty `hops` and
///   `confidence: 1.0`;
/// * otherwise the `hops` chain, the overall `confidence` (== the affected node's
///   impact confidence — the consistency invariant), and `ambiguous`.
fn tool_explain(graph: &Graph, args: &Value) -> Result<Value, ToolError> {
    // Accept `symbol` (matching impact/context) or its `target` alias.
    let target_ident = match args.get("symbol").or_else(|| args.get("target")) {
        Some(v) => v
            .as_str()
            .ok_or_else(|| ToolError::BadArgs("`symbol`/`target` must be a string".into()))?,
        None => {
            return Err(ToolError::BadArgs(
                "missing string field `symbol` (the changed target; `target` also accepted)".into(),
            ))
        }
    };
    let affected_ident = require_str(args, "affected")?;

    // Resolve BOTH ends with the candidate-surfacing resolver — each end may be
    // ambiguous, so an ambiguous target OR affected returns the candidate list
    // (naming which end via `ambiguous_end`) instead of dead-ending. Each end has
    // its own uid pin (`uid` for the target, `affected_uid` for the affected).
    let target = match resolve_or_candidates(graph, args, target_ident, "uid")? {
        NodeOrCandidates::Node(n) => n,
        NodeOrCandidates::Candidates(c) => {
            return Ok(candidates_payload(
                target_ident,
                &c,
                &[("ambiguous_end", json!("target"))],
            ))
        }
    };
    let affected = match resolve_or_candidates(graph, args, affected_ident, "affected_uid")? {
        NodeOrCandidates::Node(n) => n,
        NodeOrCandidates::Candidates(c) => {
            return Ok(candidates_payload(
                affected_ident,
                &c,
                &[("ambiguous_end", json!("affected"))],
            ))
        }
    };

    let opts = impact_opts_from_args(args)?;

    match explain(graph, &target.uid, &affected.uid, &opts) {
        // Not in the blast radius: an explicit honest negative, not empty success.
        None => Ok(json!({
            "target": node_json(&target),
            "affected": node_json(&affected),
            "reachable": false,
            "reason": format!(
                "{} is not in {}'s blast radius (nothing to explain)",
                affected.name, target.name
            ),
        })),
        Some(explanation) => Ok(json!({
            "target": node_json(&target),
            "affected": node_json(&affected),
            "reachable": true,
            "confidence": explanation.confidence,
            "ambiguous": explanation.ambiguous,
            "will_break": strata_core::will_break_label(explanation.confidence, explanation.ambiguous),
            "hops": explanation_hops_json(&explanation),
        })),
    }
}

fn tool_query(graph: &Graph, args: &Value) -> Result<Value, ToolError> {
    let text = require_str(args, "text")?;
    let hits: Vec<Value> = query(graph, text).iter().map(node_json).collect();
    Ok(json!({ "matches": hits }))
}

/// The `blast` tool: the **pre-edit blast radius of a FILE** — the symbols it
/// defines, the aggregated reverse blast radius of changing them, and the risk.
/// Graph-only (ignores the ctx); a file with no indexed symbols returns an honest
/// empty report (never a fabricated all-clear).
///
/// Args: `{ file }` (repo-relative; an absolute path's suffix still matches via
/// the engine's `path_matches`). The result is the serialized
/// [`strata_index::BlastReport`] — reusing the `detect_changes` aggregation + risk
/// verbatim, so it agrees with `detect_changes` for the same symbols.
fn tool_blast(graph: &Graph, args: &Value) -> Result<Value, ToolError> {
    let file = require_str(args, "file")?;
    let report = blast_for_file(graph, file);
    serde_json::to_value(&report)
        .map_err(|e| ToolError::BadArgs(format!("failed to serialize blast report: {e}")))
}

/// The `detect_changes` tool: git-diff → per-plane changed symbols → aggregated
/// blast radius over the loaded graph → risk. Needs a repo root (from the ctx);
/// the ctx-less path returns a clear actionable error rather than guessing.
///
/// Args: `{ staged?: bool }` (default false → the working tree vs HEAD). The
/// result is the serialized [`strata_index::ChangeReport`].
fn tool_detect_changes(graph: &Graph, ctx: &ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let repo_root = ctx.repo_root.as_deref().ok_or_else(|| {
        ToolError::BadArgs(
            "detect_changes needs a repo root — launch the MCP server with a `--db \
             <repo>/.strata/graph.duckdb` (repo root is its grandparent) or an explicit `--repo \
             <path>`. In estate (`--workspace`) mode the root is `--repo` or the working \
             directory of the member repo you launched from."
                .to_string(),
        )
    })?;
    let staged = match args.get("staged") {
        None => false,
        Some(v) => v
            .as_bool()
            .ok_or_else(|| ToolError::BadArgs("`staged` must be a boolean".into()))?,
    };
    let scope = if staged {
        ChangeScope::Staged
    } else {
        ChangeScope::Working
    };
    let report =
        detect_changes(graph, repo_root, scope).map_err(|e| ToolError::BadArgs(e.to_string()))?;
    serde_json::to_value(&report)
        .map_err(|e| ToolError::BadArgs(format!("failed to serialize change report: {e}")))
}

/// The `rename` tool: graph-aware, confidence-tagged multi-file rename. Needs a
/// repo root (from the ctx) to read/write files; dry-run by default.
///
/// Args: `{ symbol, new_name, apply?, uid?, force? }`. The result is the
/// serialized [`strata_index::RenameOutcome`] — either a `candidates` list
/// (ambiguous target) or a `plan` (the edit set, `applied` iff written).
fn tool_rename(graph: &Graph, ctx: &ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let repo_root = ctx.repo_root.as_deref().ok_or_else(|| {
        ToolError::BadArgs(
            "rename needs a repo root — launch the MCP server with a `--db \
             <repo>/.strata/graph.duckdb` (repo root is its grandparent) or an explicit `--repo \
             <path>`."
                .to_string(),
        )
    })?;
    let symbol = require_str(args, "symbol")?;
    let new_name = require_str(args, "new_name")?;
    let apply = bool_arg(args, "apply")?.unwrap_or(false);
    let force = bool_arg(args, "force")?.unwrap_or(false);
    let uid = args
        .get("uid")
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .ok_or_else(|| ToolError::BadArgs("`uid` must be a string".into()))
        })
        .transpose()?;

    let opts = RenameOptions { apply, uid, force };
    let outcome = rename(graph, repo_root, symbol, new_name, &opts)
        .map_err(|e| ToolError::BadArgs(e.to_string()))?;
    serde_json::to_value(&outcome)
        .map_err(|e| ToolError::BadArgs(format!("failed to serialize rename outcome: {e}")))
}

/// Read an optional boolean argument, erroring if present but not a bool.
fn bool_arg(args: &Value, key: &str) -> Result<Option<bool>, ToolError> {
    match args.get(key) {
        None => Ok(None),
        Some(v) => v
            .as_bool()
            .map(Some)
            .ok_or_else(|| ToolError::BadArgs(format!("`{key}` must be a boolean"))),
    }
}

// ── search_docs (K5): lexical (tantivy) search over the knowledge plane's
// indexed docs — markdown sections, doc comments, spec descriptions. This is
// the ONLY tool that never touches `graph` at all: it reads
// `<repo>/.strata/docs.idx`, a separate, local-only artifact `strata index`
// writes (`strata_index::docs_index::write_docs_index`). Deterministic term
// matching, no ML — every hit is labeled with what matched, never presented
// as more than that.

/// `search_docs`'s default result count when `limit` is omitted.
const SEARCH_DOCS_DEFAULT_LIMIT: usize = 5;
/// The hard cap on `limit`, regardless of what the caller asks for.
const SEARCH_DOCS_MAX_LIMIT: usize = 25;
/// The honest "nothing to search" note — returned instead of an error when no
/// `docs.idx` is reachable at all (never indexed, or every configured index is
/// unreadable), so a missing index degrades to an empty, explained result
/// rather than a tool-call failure.
const NO_DOCS_INDEX_NOTE: &str = "no docs index — run strata index";

/// The `<repo>/.strata/docs.idx` paths to search: `ctx.repo_root`'s own index
/// first, then every `ctx.member_roots` index (estate mode) — deduped by
/// resolved path, since `repo_root` is commonly ALSO one of the manifest's
/// members (the CLI's workspace-mode construction carries both), and
/// searching the same index twice would double-count its hits.
fn docs_index_paths(ctx: &ToolCtx) -> Vec<PathBuf> {
    let mut seen = std::collections::HashSet::new();
    let mut paths = Vec::new();
    for root in ctx.repo_root.iter().chain(ctx.member_roots.iter()) {
        let idx = root.join(".strata").join(strata_index::DOCS_INDEX_DIR);
        if seen.insert(idx.clone()) {
            paths.push(idx);
        }
    }
    paths
}

/// One search hit, before cross-index merging (a single `docs.idx`'s view).
struct DocsHit {
    uid: String,
    name: String,
    path: String,
    anchor: String,
    kind: String,
    score: f32,
    snippet: String,
    matched_terms: Vec<String>,
}

/// The outcome of searching ONE `docs.idx`. `Missing`/`Unusable` are both
/// expected, non-fatal states the caller tries the next configured index
/// past — only a malformed QUERY (independent of which index it is tried
/// against) is escalated to the caller as an error (see [`tool_search_docs`]).
enum OneIndexOutcome {
    Hits(Vec<DocsHit>),
    /// No `docs.idx` directory at this path at all.
    Missing,
    /// The directory exists but could not be opened/read as a valid tantivy
    /// index (corrupt, mid-write, wrong shape) — degrade, do not error.
    Unusable,
}

/// Read a stored string field off `doc`, `""` if absent (defensive: every
/// field this reader looks up is written by `write_docs_index` for every
/// entry, so absence should not happen — but a reader never panics on a
/// malformed/foreign index).
fn stored_str(doc: &tantivy::TantivyDocument, field: tantivy::schema::Field) -> String {
    use tantivy::schema::Value;
    doc.get_first(field)
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string()
}

/// Tokenize `text` with `analyzer` — the SAME pipeline the field was indexed
/// with (fetched via `Index::tokenizer_for_field`, never a hand-rolled
/// stand-in) — and collect the resulting token strings, deduped.
///
/// **Review fix:** `matched_terms` used to test SUBSTRING containment on
/// lowercased raw text (`body_lower.contains(term)`), which reports a false
/// positive whenever a query term happens to be a substring of a real token
/// that is NOT actually that term — e.g. `"category".contains("cat")` is
/// `true`, but the token `"cat"` never occurs; only the (different) token
/// `"category"` does. Tokenizing the hit's own text with its field's own
/// analyzer and testing TOKEN EQUALITY against the query's terms is the
/// correct comparison — the query terms themselves are drawn from
/// `Query::query_terms`, i.e. already tokenized the same way by
/// `QueryParser`, so both sides of the comparison are in the same normalized
/// (lowercased, per the "default" tokenizer) form.
fn tokenize(
    analyzer: &mut tantivy::tokenizer::TextAnalyzer,
    text: &str,
) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    let mut stream = analyzer.token_stream(text);
    stream.process(&mut |token| {
        out.insert(token.text.clone());
    });
    out
}

/// Search one `docs.idx` at `idx_path` for `query_text`, returning up to
/// `limit` hits ordered by score descending. A query-syntax error is
/// returned as `Err` (escalated to the caller — see [`tool_search_docs`]);
/// every other failure mode (missing/corrupt index, an internal tantivy
/// error past the parse step) degrades to [`OneIndexOutcome::Missing`] /
/// [`OneIndexOutcome::Unusable`], never an error.
fn search_one_index(
    idx_path: &Path,
    query_text: &str,
    limit: usize,
) -> Result<OneIndexOutcome, ToolError> {
    if !idx_path.is_dir() {
        return Ok(OneIndexOutcome::Missing);
    }
    let Ok(index) = tantivy::Index::open_in_dir(idx_path) else {
        return Ok(OneIndexOutcome::Unusable);
    };
    let schema = index.schema();
    let fields = (
        schema.get_field("uid"),
        schema.get_field("name"),
        schema.get_field("path"),
        schema.get_field("anchor"),
        schema.get_field("kind"),
        schema.get_field("body"),
    );
    let (uid_f, name_f, path_f, anchor_f, kind_f, body_f) = match fields {
        (Ok(uid), Ok(name), Ok(path), Ok(anchor), Ok(kind), Ok(body)) => {
            (uid, name, path, anchor, kind, body)
        }
        _ => return Ok(OneIndexOutcome::Unusable),
    };
    let Ok(reader) = index.reader() else {
        return Ok(OneIndexOutcome::Unusable);
    };
    let searcher = reader.searcher();

    let query_parser = tantivy::query::QueryParser::for_index(&index, vec![body_f, name_f]);
    let query = query_parser
        .parse_query(query_text)
        .map_err(|e| ToolError::BadArgs(format!("invalid search_docs query: {e}")))?;

    let Ok(top_docs) = searcher.search(
        &query,
        &tantivy::collector::TopDocs::with_limit(limit).order_by_score(),
    ) else {
        return Ok(OneIndexOutcome::Unusable);
    };
    let Ok(snippet_generator) =
        tantivy::snippet::SnippetGenerator::create(&searcher, &*query, body_f)
    else {
        return Ok(OneIndexOutcome::Unusable);
    };
    // The SAME analyzers `body_f`/`name_f` were indexed with — fetched from
    // the index, never hand-rolled — so tokenizing a hit's stored text below
    // reproduces exactly the tokens that field's postings were built from
    // (review fix: token equality, not substring containment; see `tokenize`).
    let Ok(mut body_tokenizer) = index.tokenizer_for_field(body_f) else {
        return Ok(OneIndexOutcome::Unusable);
    };
    let Ok(mut name_tokenizer) = index.tokenizer_for_field(name_f) else {
        return Ok(OneIndexOutcome::Unusable);
    };

    // Every term the parsed query carries (across both queried fields, deduped
    // by text) — the pool `matched_terms` is filtered from, per hit, below.
    let mut all_terms: Vec<String> = Vec::new();
    {
        // `Term::value().as_str()` is an inherent method (`ValueBytes`), not
        // trait-provided — no `Value` import needed here (unlike `stored_str`).
        let mut seen = std::collections::HashSet::new();
        query.query_terms(&mut |term, _positions| {
            if let Some(s) = term.value().as_str() {
                if seen.insert(s.to_string()) {
                    all_terms.push(s.to_string());
                }
            }
        });
    }

    let mut hits = Vec::new();
    for (score, addr) in top_docs {
        let Ok(doc) = searcher.doc::<tantivy::TantivyDocument>(addr) else {
            continue;
        };
        let name = stored_str(&doc, name_f);
        let body = stored_str(&doc, body_f);
        let snippet = snippet_generator.snippet(&body).to_html();

        // The terms that actually HIT this document — a subset of `all_terms`
        // when the query has several terms and only some occur here (the
        // default query conjunction is OR, so a hit does not imply every term
        // matched). TOKEN EQUALITY, not substring containment: tokenize the
        // hit's own body/name text with the field's own indexing analyzer and
        // test each query term for membership in that exact token set — a
        // query term that merely happens to be a SUBSTRING of a real, longer
        // token (`"cat"` inside `"category"`) must never be reported as
        // matched, since it never occurs as its own token.
        let name_tokens = tokenize(&mut name_tokenizer, &name);
        let body_tokens = tokenize(&mut body_tokenizer, &body);
        let matched_terms: Vec<String> = all_terms
            .iter()
            .filter(|t| body_tokens.contains(t.as_str()) || name_tokens.contains(t.as_str()))
            .cloned()
            .collect();

        hits.push(DocsHit {
            uid: stored_str(&doc, uid_f),
            name,
            path: stored_str(&doc, path_f),
            anchor: stored_str(&doc, anchor_f),
            kind: stored_str(&doc, kind_f),
            score,
            snippet,
            matched_terms,
        });
    }
    Ok(OneIndexOutcome::Hits(hits))
}

/// The `search_docs` tool: `{ query: string, limit?: number=5 (max 25) }` →
/// `{ results: [{ uid, name, path, anchor, kind, score, snippet,
/// matched_terms }] }`. Single-repo mode searches `ctx.repo_root`'s
/// `docs.idx`; estate mode (`ctx.member_roots` non-empty) searches every
/// member's `docs.idx` and merges by score descending, tie-broken by `uid`
/// ascending for deterministic ordering across runs. No `docs.idx` reachable
/// at all (never indexed, every one unreadable) → `{ results: [], note:
/// "no docs index — run strata index" }`, never an error — the ONE exception
/// is a query the tantivy syntax parser itself rejects, which IS a caller
/// error (`BadArgs`), independent of which/whether any index exists.
fn tool_search_docs(ctx: &ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let query_text = require_str(args, "query")?;
    let limit = match args.get("limit") {
        None | Some(Value::Null) => SEARCH_DOCS_DEFAULT_LIMIT,
        Some(v) => v
            .as_u64()
            .ok_or_else(|| ToolError::BadArgs("`limit` must be a number".into()))?
            as usize,
    }
    .clamp(1, SEARCH_DOCS_MAX_LIMIT);

    let index_paths = docs_index_paths(ctx);
    if index_paths.is_empty() {
        return Ok(json!({ "results": [], "note": NO_DOCS_INDEX_NOTE }));
    }

    let mut all_hits: Vec<DocsHit> = Vec::new();
    let mut any_usable = false;
    for idx_path in &index_paths {
        match search_one_index(idx_path, query_text, limit)? {
            OneIndexOutcome::Hits(mut hits) => {
                any_usable = true;
                all_hits.append(&mut hits);
            }
            OneIndexOutcome::Missing | OneIndexOutcome::Unusable => continue,
        }
    }

    if !any_usable {
        return Ok(json!({ "results": [], "note": NO_DOCS_INDEX_NOTE }));
    }

    // Deterministic cross-index merge: score descending, `uid` ascending as
    // the tie-break so equal-score hits (common — e.g. two exact single-term
    // matches) sort the same way on every run/every machine, never by
    // incidental HashMap/thread ordering.
    all_hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.uid.cmp(&b.uid))
    });
    all_hits.truncate(limit);

    Ok(json!({
        "results": all_hits.iter().map(|h| json!({
            "uid": h.uid,
            "name": h.name,
            "path": h.path,
            "anchor": h.anchor,
            "kind": h.kind,
            "score": h.score,
            "snippet": h.snippet,
            "matched_terms": h.matched_terms,
        })).collect::<Vec<_>>()
    }))
}

// ── guidance (K6): token-budgeted digest of what the repo knows about a
// symbol/file — its own doc comment, the docs that document/mention it, and
// (for a contract operation) its spec description, bodies sliced from disk at
// query time. Token budgets here are TESTED requirements (plan Global
// Constraints), not aspirations.

/// `guidance`'s default total budget, in the SAME unit `str::len()` reports
/// (UTF-8 bytes) — ~1,200 tokens per the plan.
const GUIDANCE_DEFAULT_BUDGET: usize = 4800;
/// The per-section cap within that budget.
const GUIDANCE_SECTION_CAP: usize = 1200;
/// Ordering tier 0: a live-re-extracted contract spec description (K4) —
/// always first, regardless of its (fixed 1.0) confidence.
const GUIDANCE_TIER_DESCRIPTION: u8 = 0;
/// Ordering tier 1: incoming `Documents` edges (a symbol's own doc comment).
const GUIDANCE_TIER_DOCUMENTS: u8 = 1;
/// Ordering tier 2: incoming `Mentions` edges.
const GUIDANCE_TIER_MENTIONS: u8 = 2;

/// Where a [`GuidanceCandidate`]'s body text comes from.
enum GuidanceBody {
    /// Read from `<root>/<candidate.path>`, sliced to `[start_line, end_line]`
    /// (1-based inclusive) — never stored in the graph, read fresh each call.
    /// `section_repo` is the section's own uid `package` field (the 2nd
    /// `|`-delimited component), used to pick the right root in estate mode.
    Disk {
        section_repo: String,
        start_line: u32,
        end_line: u32,
    },
    /// Already in hand (the live re-extracted spec description) — no disk IO.
    Inline(String),
}

/// One resolved doc reference BEFORE budget trimming.
struct GuidanceCandidate {
    uid: String,
    name: String,
    path: String,
    anchor: String,
    provenance: Provenance,
    confidence: f32,
    tier: u8,
    body: GuidanceBody,
}

/// The `k`-th `|`-delimited field of a uid string (`language|package|path|fqn|signature`).
fn uid_field(uid: &str, k: usize) -> Option<&str> {
    uid.split('|').nth(k)
}

/// Split a `DocSection`'s fqn (`<path>#<anchor>` by construction — see
/// `NodeKind::DocSection`'s doc comment) into its anchor half.
fn anchor_from_fqn(fqn: &str) -> &str {
    fqn.split_once('#').map(|(_, anchor)| anchor).unwrap_or(fqn)
}

/// The repo root to read a knowledge-plane section's file from: `ctx.repo_root`
/// when set (single-repo mode — the common case); else, in estate mode, the
/// `ctx.member_roots` entry whose OWN final path component equals `repo` (the
/// section's uid `package` field). No match (an unnamed/mismatched estate
/// layout) → `None`, which the caller turns into an honest "body unavailable"
/// rather than ever guessing a possibly-wrong repo's file.
fn resolve_root_for_repo(ctx: &ToolCtx, repo: &str) -> Option<PathBuf> {
    if let Some(root) = &ctx.repo_root {
        return Some(root.clone());
    }
    ctx.member_roots
        .iter()
        .find(|r| r.file_name().and_then(|n| n.to_str()) == Some(repo))
        .cloned()
}

/// Try both contract adapters' `detects`-then-`extract` routing (mirrors
/// `strata-index`'s `contract_op_sigs`) so guidance re-parses a spec with the
/// SAME format-detection logic the indexer uses — never a guess. gRPC is
/// included for completeness (`ApiOperation` covers both OpenAPI and gRPC),
/// though its adapter never captures a description (K4), so it always yields
/// an empty description set in practice.
fn extract_contract_operations(path: &str, content: &str) -> Vec<strata_contract::OperationDef> {
    use strata_contract::{ContractAdapter, GraphqlAdapter, OpenApiAdapter, ProtoAdapter};
    let openapi = OpenApiAdapter;
    let graphql = GraphqlAdapter;
    let grpc = ProtoAdapter;
    if openapi.detects(path, content) {
        openapi.extract(path, content).unwrap_or_default()
    } else if graphql.detects(path, content) {
        graphql.extract(path, content).unwrap_or_default()
    } else if grpc.detects(path, content) {
        grpc.extract(path, content).unwrap_or_default()
    } else {
        Vec::new()
    }
}

/// Live re-extraction (K4) of `target`'s spec description: read the spec file
/// fresh and re-run the same adapter routing the indexer used, so the text is
/// NEVER stale. `target.uid`'s `package`/`path` fields are the repo name and
/// the spec's repo-relative path respectively (see `strata-index::contract`'s
/// `operation_uid` — the per-repo shape this resolves against; an estate
/// CANONICAL contract uid has a different 3rd field and honestly yields
/// `None` here rather than misreading it as a path). `None` on ANY failure —
/// no root, unreadable file, no adapter detects it, no matching operation key,
/// or a blank declared description — never an error, never a guess.
fn live_operation_description(ctx: &ToolCtx, target: &Node) -> Option<String> {
    let repo = uid_field(target.uid.as_str(), 1)?;
    let root = resolve_root_for_repo(ctx, repo)?;
    let spec_path = uid_field(target.uid.as_str(), 2)?;
    let content = std::fs::read_to_string(root.join(spec_path)).ok()?;
    extract_contract_operations(spec_path, &content)
        .into_iter()
        .find(|op| op.key == target.fqn)
        .and_then(|op| op.description)
        .map(|d| d.trim().to_string())
        .filter(|d| !d.is_empty())
}

/// Push every incoming `Documents`/`Mentions` candidate for `uid` onto `out`
/// (tier 1 / tier 2 respectively) — shared by the symbol and file gathering
/// paths so both read the SAME edges the SAME way.
fn push_doc_edge_candidates(
    graph: &Graph,
    uid: &strata_core::Uid,
    out: &mut Vec<GuidanceCandidate>,
) {
    for (edge, node) in graph.neighbors(
        uid,
        Direction::Incoming,
        &[EdgeKind::Documents, EdgeKind::Mentions],
    ) {
        let tier = if edge.kind == EdgeKind::Documents {
            GUIDANCE_TIER_DOCUMENTS
        } else {
            GUIDANCE_TIER_MENTIONS
        };
        out.push(GuidanceCandidate {
            uid: node.uid.as_str().to_string(),
            name: node.name.clone(),
            path: node.path.clone(),
            anchor: anchor_from_fqn(&node.fqn).to_string(),
            provenance: edge.provenance,
            confidence: edge.confidence.value(),
            tier,
            body: GuidanceBody::Disk {
                section_repo: uid_field(node.uid.as_str(), 1)
                    .unwrap_or_default()
                    .to_string(),
                start_line: node.span.start_line,
                end_line: node.span.end_line,
            },
        });
    }
}

/// Every candidate for a SYMBOL target, unsorted: the live spec description
/// (tier 0, ApiOperation/GraphqlField only) first if one resolves, then its
/// incoming Documents/Mentions.
fn guidance_candidates_for_symbol(
    graph: &Graph,
    ctx: &ToolCtx,
    target: &Node,
) -> Vec<GuidanceCandidate> {
    let mut out = Vec::new();
    if matches!(target.kind, NodeKind::ApiOperation | NodeKind::GraphqlField) {
        if let Some(desc) = live_operation_description(ctx, target) {
            out.push(GuidanceCandidate {
                uid: format!("{}#description", target.uid.as_str()),
                name: format!("{} description", target.name),
                path: target.path.clone(),
                anchor: "description".to_string(),
                provenance: Provenance::Extracted,
                confidence: 1.0,
                tier: GUIDANCE_TIER_DESCRIPTION,
                body: GuidanceBody::Inline(desc),
            });
        }
    }
    push_doc_edge_candidates(graph, &target.uid, &mut out);
    out
}

/// Whether a graph node's `path` matches the guidance `file` target — an
/// exact match, or a **path-component-boundary** suffix either way (mirrors
/// `strata-index::changes`'s private `node_in_file`, same contract, not
/// importable since it's private and small enough to duplicate here). So
/// `src/a.ts` matches a stored `src/a.ts` AND an absolute
/// `/repo/src/a.ts` (the PreToolUse hook passes absolute paths) — but
/// `a.ts` does NOT match `schema.ts`, and an empty `node_path` matches
/// nothing (a structural container, never a real file member).
///
/// **C3 fix (review):** the OLD code required byte-exact `n.path == file`,
/// so `guidance --file <absolute path>` silently found nothing on a real
/// repo (`blast` on the SAME file found 11 dependents) — the PreToolUse hook
/// always passes an absolute `tool_input.file_path`, so this was the common
/// case failing, not an edge case.
fn node_in_file(node_path: &str, file: &str) -> bool {
    if node_path.is_empty() {
        return false;
    }
    if node_path == file {
        return true;
    }
    let boundary_suffix = |longer: &str, shorter: &str| -> bool {
        longer.len() > shorter.len()
            && longer.ends_with(shorter)
            && longer.as_bytes()[longer.len() - shorter.len() - 1] == b'/'
    };
    boundary_suffix(file, node_path) || boundary_suffix(node_path, file)
}

/// Every candidate for a FILE target: the union of every node's (symbols +
/// the file's own Module — anything whose `path` [`node_in_file`]-matches
/// `file`) incoming Documents/Mentions, deduped by section uid keeping the
/// MAX confidence (a section can link to several of the file's symbols).
fn guidance_candidates_for_file(graph: &Graph, file: &str) -> Vec<GuidanceCandidate> {
    let mut by_uid: std::collections::BTreeMap<String, GuidanceCandidate> =
        std::collections::BTreeMap::new();
    for n in graph.nodes().filter(|n| node_in_file(&n.path, file)) {
        let mut local = Vec::new();
        push_doc_edge_candidates(graph, &n.uid, &mut local);
        for cand in local {
            use std::collections::btree_map::Entry;
            match by_uid.entry(cand.uid.clone()) {
                Entry::Vacant(e) => {
                    e.insert(cand);
                }
                Entry::Occupied(mut e) => {
                    if cand.confidence > e.get().confidence {
                        e.insert(cand);
                    }
                }
            }
        }
    }
    by_uid.into_values().collect()
}

/// The joined source lines `[start_line, end_line]` (1-based, inclusive) —
/// mirrors `strata-index`'s private `slice_span`; small and file-local enough
/// to duplicate rather than plumb a new cross-crate export for one caller.
fn slice_body_lines(content: &str, start_line: u32, end_line: u32) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let start = (start_line.saturating_sub(1)) as usize;
    let end = (end_line as usize).min(lines.len());
    if start >= end {
        return String::new();
    }
    lines[start..end].join("\n")
}

/// Read one candidate's FULL, untrimmed body (before any budget/cap is
/// applied). `Ok("")` is a legitimate empty body; `Err(note)` is the honest
/// "could not read" case (no resolvable root, or the file is missing/unreadable
/// on disk) — never a panic, never silence.
fn guidance_read_body(ctx: &ToolCtx, cand: &GuidanceCandidate) -> Result<String, &'static str> {
    match &cand.body {
        GuidanceBody::Inline(text) => Ok(text.clone()),
        GuidanceBody::Disk {
            section_repo,
            start_line,
            end_line,
        } => {
            let root = resolve_root_for_repo(ctx, section_repo).ok_or("body unavailable")?;
            let content =
                std::fs::read_to_string(root.join(&cand.path)).map_err(|_| "body unavailable")?;
            Ok(slice_body_lines(&content, *start_line, *end_line))
        }
    }
}

/// Slice `body` to at most `cap` BYTES (the same unit `str::len()`/the budget
/// counts in) at a char boundary — never splitting a multi-byte UTF-8
/// sequence. Returns `(slice, was_cut)`; `was_cut` is true iff the slice is
/// STRICTLY shorter than the full body (a body that happens to be exactly
/// `cap` bytes is not "cut" — the whole thing was shown).
fn guidance_truncate(body: &str, cap: usize) -> (String, bool) {
    if body.len() <= cap {
        return (body.to_string(), false);
    }
    let mut end = cap;
    while end > 0 && !body.is_char_boundary(end) {
        end -= 1;
    }
    (body[..end].to_string(), true)
}

/// The exact truncation marker appended when a section is cut — a fixed
/// format so the guardrail test can match it literally.
fn guidance_marker(path: &str, anchor: &str) -> String {
    format!("… [truncated — fetch {path}#{anchor}]")
}

/// One `sections[]` entry's JSON shape, shared by the budgeted path and the
/// `section` (full, no-budget) path.
#[allow(clippy::too_many_arguments)]
fn guidance_section_json(
    cand: &GuidanceCandidate,
    text: String,
    truncated: bool,
    ref_only: bool,
    note: Option<&str>,
) -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert("uid".into(), json!(cand.uid));
    obj.insert("name".into(), json!(cand.name));
    obj.insert("path".into(), json!(cand.path));
    obj.insert("anchor".into(), json!(cand.anchor));
    obj.insert("provenance".into(), json!(provenance_name(cand.provenance)));
    obj.insert("confidence".into(), json!(cand.confidence));
    obj.insert("text".into(), json!(text));
    obj.insert("truncated".into(), json!(truncated));
    obj.insert("ref_only".into(), json!(ref_only));
    if let Some(n) = note {
        obj.insert("note".into(), json!(n));
    }
    Value::Object(obj)
}

/// The budget-mechanics core: iterate `ordered` (already sorted), taking a
/// slice of each section's body at a char boundary; append the truncation
/// marker when a section is cut; stop CONSUMING budget once exhausted but
/// ALWAYS emit a `ref_only` entry for every remaining candidate so nothing is
/// invisible. Returns the section JSON values plus the total bytes actually
/// taken (`budget_used`).
///
/// **C1 fix (review):** the marker's own bytes are reserved and charged
/// BEFORE slicing — `cap = min(1200, remaining).saturating_sub(marker.len())`,
/// `remaining -= slice.len() + marker.len()` whenever the marker is actually
/// used — so `budget_used`/`remaining` account for exactly what lands in
/// `text`. The old code sliced against the full `min(1200, remaining)` cap
/// and appended the marker ON TOP, uncounted: on a real repo with long
/// path/anchor strings the reviewer measured a **5,492-byte** response
/// against the 4,800 covenant (a leak that scales with path length, worse
/// the longer the repo's paths are).
///
/// **C2 fix (review):** reserving marker space can itself collapse `cap` to
/// (near) zero, and separately a multibyte-starting body can back off to an
/// EMPTY slice at a small-but-nonzero cap (`guidance_truncate`'s char-
/// boundary search never overshoots forward). Either way, a non-empty body
/// that yields an empty slice must NOT emit a marker-only "section" that
/// silently eats budget for zero content (the old code did exactly this —
/// `budget: 1` against a real repo returned 1,663 bytes of pure markers,
/// zero content, `budget_used: 0`). The fix: empty slice + non-empty body ⇒
/// `ref_only: true`, NO marker, and `remaining` forced to 0 so every
/// remaining candidate degrades the same honest way instead of each taking
/// its own free marker.
fn guidance_budgeted_sections(
    ctx: &ToolCtx,
    ordered: Vec<GuidanceCandidate>,
    budget: usize,
) -> (Vec<Value>, usize) {
    let mut remaining = budget;
    let mut budget_used = 0usize;
    let mut sections = Vec::with_capacity(ordered.len());
    for cand in ordered {
        if remaining == 0 {
            sections.push(guidance_section_json(
                &cand,
                String::new(),
                false,
                true,
                None,
            ));
            continue;
        }
        match guidance_read_body(ctx, &cand) {
            Err(note) => {
                sections.push(guidance_section_json(
                    &cand,
                    String::new(),
                    false,
                    false,
                    Some(note),
                ));
            }
            Ok(body) if body.is_empty() => {
                // A legitimately empty body (e.g. a zero-line span) — nothing
                // to slice, nothing to charge, no marker possible.
                sections.push(guidance_section_json(
                    &cand,
                    String::new(),
                    false,
                    false,
                    None,
                ));
            }
            Ok(body) => {
                let marker = guidance_marker(&cand.path, &cand.anchor);
                // C1: reserve the marker's bytes BEFORE slicing.
                let cap = GUIDANCE_SECTION_CAP
                    .min(remaining)
                    .saturating_sub(marker.len());
                let (slice, cut) = guidance_truncate(&body, cap);
                if slice.is_empty() {
                    // C2: zero progress possible (cap collapsed to 0 from
                    // marker reservation, or a multibyte-starting body
                    // couldn't fit even one char at this cap) — ref-only,
                    // no marker, treat the budget as genuinely exhausted.
                    sections.push(guidance_section_json(
                        &cand,
                        String::new(),
                        false,
                        true,
                        None,
                    ));
                    remaining = 0;
                } else if cut {
                    let taken = slice.len() + marker.len();
                    remaining -= taken;
                    budget_used += taken;
                    sections.push(guidance_section_json(
                        &cand,
                        format!("{slice}{marker}"),
                        true,
                        false,
                        None,
                    ));
                } else {
                    remaining -= slice.len();
                    budget_used += slice.len();
                    sections.push(guidance_section_json(&cand, slice, false, false, None));
                }
            }
        }
    }
    (sections, budget_used)
}

/// The `guidance` tool: `{ symbol?, file?, budget?: number=4800, section?:
/// string }` → `{ target: {uid,name,kind}, sections: [...], budget_used,
/// note? }`. Exactly one of `symbol`/`file` is required. Ordering: a live
/// spec description (contract targets only) → incoming `Documents` → incoming
/// `Mentions`, each confidence desc then uid asc. `section` (an anchor)
/// returns that ONE candidate's FULL body — no budget/truncation applied —
/// or `NotFound` when no candidate carries that anchor. Never touches the
/// graph structure with body text: bodies are read fresh from disk every call.
fn tool_guidance(graph: &Graph, ctx: &ToolCtx, args: &Value) -> Result<Value, ToolError> {
    let symbol_arg = opt_str(args, "symbol")?;
    let file_arg = opt_str(args, "file")?;
    let (target_json, mut candidates) = match (symbol_arg, file_arg) {
        (Some(_), Some(_)) => {
            return Err(ToolError::BadArgs(
                "provide exactly one of `symbol` or `file`, not both".into(),
            ))
        }
        (None, None) => {
            return Err(ToolError::BadArgs(
                "missing `symbol` or `file` (guidance needs exactly one)".into(),
            ))
        }
        (Some(symbol), None) => {
            let node = match resolve_or_candidates(graph, args, symbol, "uid")? {
                NodeOrCandidates::Node(n) => n,
                NodeOrCandidates::Candidates(c) => return Ok(candidates_payload(symbol, &c, &[])),
            };
            let candidates = guidance_candidates_for_symbol(graph, ctx, &node);
            (node_json(&node), candidates)
        }
        (None, Some(file)) => {
            let candidates = guidance_candidates_for_file(graph, file);
            (
                json!({ "uid": file, "name": file, "kind": "File" }),
                candidates,
            )
        }
    };

    // `section` (an anchor): return that ONE candidate FULL — no budget.
    if let Some(anchor) = opt_str(args, "section")? {
        let cand = candidates
            .iter()
            .find(|c| c.anchor == anchor)
            .ok_or_else(|| ToolError::NotFound(format!("section `{anchor}`")))?;
        let (text, note) = match guidance_read_body(ctx, cand) {
            Ok(body) => (body, None),
            Err(n) => (String::new(), Some(n)),
        };
        let budget_used = text.len();
        let section = guidance_section_json(cand, text, false, false, note);
        return Ok(json!({
            "target": target_json,
            "sections": [section],
            "budget_used": budget_used,
        }));
    }

    // Ordering: tier asc (description, Documents, Mentions), confidence desc,
    // uid asc (determinism).
    candidates.sort_by(|a, b| {
        a.tier
            .cmp(&b.tier)
            .then_with(|| {
                b.confidence
                    .partial_cmp(&a.confidence)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| a.uid.cmp(&b.uid))
    });

    let budget = match args.get("budget") {
        None | Some(Value::Null) => GUIDANCE_DEFAULT_BUDGET,
        Some(v) => v
            .as_u64()
            .ok_or_else(|| ToolError::BadArgs("`budget` must be a non-negative integer".into()))?
            as usize,
    };

    // Bare ctx (no repo_root, no member_roots at all) → every disk-backed
    // candidate will honestly degrade to "body unavailable" below; ALSO surface
    // a top-level note so the caller sees the root cause once, not just N
    // per-entry notes. Only fires when it is actually relevant (at least one
    // candidate needs disk access — an all-inline result, e.g. a bare
    // description hit, is unaffected).
    let bare_ctx_with_disk_candidates = ctx.repo_root.is_none()
        && ctx.member_roots.is_empty()
        && candidates
            .iter()
            .any(|c| matches!(c.body, GuidanceBody::Disk { .. }));

    let (sections, budget_used) = guidance_budgeted_sections(ctx, candidates, budget);
    let sections_empty = sections.is_empty();

    let mut out = serde_json::Map::new();
    out.insert("target".into(), target_json);
    out.insert("sections".into(), json!(sections));
    out.insert("budget_used".into(), json!(budget_used));
    if sections_empty {
        out.insert("note".into(), json!("no documentation found"));
    } else if bare_ctx_with_disk_candidates {
        out.insert(
            "note".into(),
            json!("no repo root configured — bodies unavailable (refs only)"),
        );
    }
    Ok(Value::Object(out))
}

// ── schemas ─────────────────────────────────────────────────────────────────────

/// The 9 tools' MCP `tools/list` descriptors (name + description + inputSchema).
pub fn tool_schemas() -> Value {
    json!([
        {
            "name": "context",
            "description": "The 360-degree view of one symbol: callers, callees, imports, members, container, producers, consumers, produces, consumes, the infra wiring (assumes/assumed_by, routes_to/routed_from, runs/run_by), and the data-plane ORM mapping (mapped_by/maps_to) — e.g. an IamRole's assumed_by lists the Lambdas that assume it; a Table's mapped_by lists the ORM model classes that map to it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "symbol": { "type": "string", "description": "Identifier (fqn preferred, else name) to inspect." }
                },
                "required": ["symbol"]
            }
        },
        {
            "name": "impact",
            "description": "Reverse blast radius: everything that depends on the symbol within `depth` hops. Recall-biased. Contract- and infra-aware by default — follows producer→operation→consumer across the contract plane, and the infra wiring Assumes/Routes/Runs (e.g. an IamRole reaches the Lambdas that assume it and their downstream reach). Set `include_contracts:false` and/or `include_infra:false` to narrow the blast radius. For a member-bearing target (class/struct/enum/interface/table) whose own `affected` is empty because dependents hang off its MEMBERS (a method has callers; a column is referenced) — not the type — the result carries `members_with_dependents: [{uid,name,kind}]` listing the members that DO have dependents, so the zero is never a misleading 'nothing depends on this'. Pin one and re-run `impact` on it. Absent when `affected` is non-empty or the container is genuinely dead.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "symbol": { "type": "string", "description": "Identifier whose dependents to compute." },
                    "uid": { "type": "string", "description": "Pin one candidate when `symbol` resolves to several nodes (an ambiguous symbol returns `{ambiguous:true, candidates:[…]}` — re-run with the chosen candidate's `uid`)." },
                    "depth": { "type": "integer", "minimum": 0, "description": "Max reverse-traversal depth (default 5)." },
                    "min_confidence": { "type": "number", "minimum": 0.0, "maximum": 1.0, "description": "Drop paths below this confidence (default 0.0)." },
                    "include_contracts": { "type": "boolean", "description": "Follow the contract plane (producer→operation→consumer), surfacing cross-plane/cross-repo consumers. Default true." },
                    "include_infra": { "type": "boolean", "description": "Follow the infra plane (incoming Assumes/Routes/Runs), so an IamRole reaches its assuming Lambdas and a handler module reaches its Lambda. Default true." }
                },
                "required": ["symbol"]
            }
        },
        {
            "name": "explain",
            "description": "Why is B in A's blast radius? The evidence chain — the exact sequence of edges from the changed target to the affected node, each with its kind, provenance, and confidence, plus the running confidence that produces the number `impact` reports (the consistency invariant: the final running confidence equals impact's confidence for that node). Honest: an unreachable affected node returns `reachable:false` (not in the blast radius); `target == affected` returns an empty chain at confidence 1.0; AMBIGUOUS hops are marked. Uses the same depth/include_contracts/include_infra toggles as `impact`.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "symbol": { "type": "string", "description": "The changed target (fqn preferred, else name). `target` is accepted as an alias." },
                    "affected": { "type": "string", "description": "The affected node whose presence in the blast radius to explain (fqn preferred, else name)." },
                    "uid": { "type": "string", "description": "Pin the TARGET when it resolves to several nodes (an ambiguous end returns `{ambiguous:true, ambiguous_end, candidates:[…]}` — re-run with the chosen `uid`)." },
                    "affected_uid": { "type": "string", "description": "Pin the AFFECTED node when it resolves to several nodes." },
                    "depth": { "type": "integer", "minimum": 0, "description": "Max reverse-traversal depth (default 5) — must match the impact run being explained." },
                    "min_confidence": { "type": "number", "minimum": 0.0, "maximum": 1.0, "description": "Drop paths below this confidence (default 0.0)." },
                    "include_contracts": { "type": "boolean", "description": "Follow the contract plane (producer→operation→consumer). Default true." },
                    "include_infra": { "type": "boolean", "description": "Follow the infra plane (incoming Assumes/Routes/Runs). Default true." }
                },
                "required": ["symbol", "affected"]
            }
        },
        {
            "name": "query",
            "description": "Lexical search over node name, fully-qualified name, and path (case-insensitive substring).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "text": { "type": "string", "description": "Substring to search for." }
                },
                "required": ["text"]
            }
        },
        {
            "name": "blast",
            "description": "The pre-edit blast radius of a FILE (not a single symbol): the symbols the file defines across all planes, the aggregated reverse blast radius of changing them (the same dedupe/order as detect_changes), and the risk level (LOW < 5 affected; MEDIUM 5–15; HIGH > 15; CRITICAL on contract surface or cross-repo) with reasons. Run it BEFORE editing a file to see what depends on it. A file with no indexed symbols returns an honest empty report (a `note` says so) — never a fabricated all-clear. Reports — it never gates.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "file": { "type": "string", "description": "The file to assess, repo-relative (e.g. `src/foo.ts`)." }
                },
                "required": ["file"]
            }
        },
        {
            "name": "detect_changes",
            "description": "The mechanical pre-commit check: git-diff the working tree (or the staged index) against HEAD, derive the changed symbols PER PLANE (code functions/classes; contract GraphQL fields / API operations; infra CFN/SAM resources), aggregate the reverse blast radius of every removed/modified symbol over the loaded graph, and assign a risk level (LOW < 5 affected; MEDIUM 5–15; HIGH > 15; CRITICAL on contract surface or cross-repo) with human-readable reasons. Reports — it never gates. Needs the server to know the repo root (launch with `--db <repo>/.strata/graph.duckdb` or `--repo <path>`).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "staged": { "type": "boolean", "description": "Diff the staged index (`git diff --cached HEAD`) instead of the working tree. Default false." }
                }
            }
        },
        {
            "name": "rename",
            "description": "Graph-aware, confidence-tagged multi-file rename — the safe alternative to find-and-replace. Resolves the symbol to one code node (Function/Method/Class/Interface; several matches → a candidate list, pin one with `uid`), edits the identifier ONLY in files the graph implicates (the definition file + files connected by a call/import edge — a same-named identifier in an unrelated file is never touched), tags each edit with the implicating edge's confidence, and is DRY-RUN by default (returns the edit set without writing). Set `apply:true` to write the edits atomically. A repo-wide name collision refuses unless `force:true`. Needs the server to know the repo root.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "symbol": { "type": "string", "description": "The code symbol to rename (fqn preferred, else name)." },
                    "new_name": { "type": "string", "description": "The new identifier." },
                    "apply": { "type": "boolean", "description": "Write the edits to disk. Default false (dry run — returns the plan only)." },
                    "uid": { "type": "string", "description": "Pin one candidate when the symbol resolves to several code nodes." },
                    "force": { "type": "boolean", "description": "Proceed even if a repo-wide symbol is already named `new_name`. Default false." }
                },
                "required": ["symbol", "new_name"]
            }
        },
        {
            "name": "search_docs",
            "description": "Lexical (tantivy, deterministic — no ML/embeddings) full-text search over the knowledge plane's indexed docs: markdown section bodies, doc comments, and OpenAPI/GraphQL spec descriptions. Replaces manual doc-grepping for \"how do we…?\"/\"is there guidance on X?\" questions. Every hit is a labeled TERM MATCH, never a summary or an answer — it names which query terms actually hit (`matched_terms`) and a highlighted snippet, so it is always explainable. Missing or corrupt index (never indexed yet, or `strata index` has not run since) returns an honest `{results: [], note: \"no docs index — run strata index\"}` rather than an error.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Search text (tantivy query syntax over section/doc-comment/description text and names)." },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 25, "description": "Max results (default 5, hard-capped at 25)." }
                },
                "required": ["query"]
            }
        },
        {
            "name": "guidance",
            "description": "Token-budgeted digest of what the repo knows about a symbol or file: its own doc comment, the docs that document/mention it, and — for an ApiOperation/GraphqlField — its spec description RE-EXTRACTED LIVE from the spec file (never stale). Ordering: description (contract targets) → Documents (own doc comment) → Mentions, each confidence desc. Bodies are sliced from disk at query time (never stored in the graph): default total budget 4800 chars (~1,200 tokens), 1200 chars per section — a cut section gets a `… [truncated — fetch {path}#{anchor}]` marker and budget-exhausted sections still appear as `ref_only:true` refs (nothing is invisible). Pass `section` (an anchor) to fetch ONE section's FULL body with no budget applied. Honest degradation throughout: an unreadable/missing file yields `note: \"body unavailable\"` on that entry, never an error; no repo root configured degrades every disk-backed entry the same way plus a top-level note.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "symbol": { "type": "string", "description": "The symbol to summarize (fqn preferred, else name). Exactly one of `symbol`/`file` is required." },
                    "file": { "type": "string", "description": "Aggregate over this file's symbols instead of one symbol (repo-relative path). Exactly one of `symbol`/`file` is required." },
                    "uid": { "type": "string", "description": "Pin one candidate when `symbol` resolves to several nodes (an ambiguous symbol returns `{ambiguous:true, candidates:[…]}` — re-run with the chosen candidate's `uid`)." },
                    "budget": { "type": "integer", "minimum": 0, "description": "Total character budget across all sections (default 4800). Ignored when `section` is given." },
                    "section": { "type": "string", "description": "An anchor (from a prior `guidance`/`context`/`search_docs` result) — return that ONE section's FULL body, uncapped, no budget applied." }
                }
            }
        }
    ])
}

/// The graph's node-kind and edge-kind vocabularies, for the `strata://schema` resource.
pub fn graph_schema_json() -> Value {
    let node_kinds = [
        NodeKind::Repo,
        NodeKind::Package,
        NodeKind::File,
        NodeKind::Module,
        NodeKind::Class,
        NodeKind::Interface,
        NodeKind::Function,
        NodeKind::Method,
        NodeKind::ApiOperation,
        NodeKind::GraphqlField,
        NodeKind::LambdaFn,
        NodeKind::IamRole,
        NodeKind::AppSyncApi,
        NodeKind::AppSyncResolver,
        NodeKind::AppSyncDataSource,
        NodeKind::CloudResource,
        NodeKind::Table,
        NodeKind::Column,
        NodeKind::CloudAction,
        // Knowledge plane (K2): ingested markdown docs and their sections.
        NodeKind::Doc,
        NodeKind::DocSection,
    ];
    let edge_kinds = [
        EdgeKind::Defines,
        EdgeKind::MemberOf,
        EdgeKind::Imports,
        EdgeKind::Calls,
        EdgeKind::Extends,
        EdgeKind::Implements,
        EdgeKind::Produces,
        EdgeKind::Consumes,
        EdgeKind::Assumes,
        EdgeKind::Runs,
        EdgeKind::Routes,
        EdgeKind::Contains,
        EdgeKind::HasColumn,
        EdgeKind::ForeignKey,
        EdgeKind::Reads,
        EdgeKind::Writes,
        // MapsTo: an ORM model class maps to the Table it persists to (model→table).
        EdgeKind::MapsTo,
        // IAM permission-gap (D2): a role Grants a CloudAction; code RequiresPermission it.
        EdgeKind::Grants,
        EdgeKind::RequiresPermission,
        // Knowledge plane (K2): Doc—Contains→DocSection (never impact-traversed),
        // and DocSection—Mentions→anything a section's refs resolved to.
        EdgeKind::Documents,
        EdgeKind::Mentions,
    ];
    json!({
        "node_kinds": node_kinds.iter().map(|k| kind_name(*k)).collect::<Vec<_>>(),
        "edge_kinds": edge_kinds.iter().map(|k| edge_kind_name(*k)).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use strata_core::{Confidence, Edge, Node, Provenance, Span, Uid};

    fn node(uid: &str, name: &str) -> Node {
        Node {
            uid: Uid(uid.into()),
            kind: NodeKind::Function,
            name: name.into(),
            fqn: name.into(),
            path: format!("{uid}.ts"),
            span: Span::default(),
            provenance: Provenance::Extracted,
            confidence: Confidence::new(1.0),
        }
    }

    fn calls(src: &str, dst: &str) -> Edge {
        Edge {
            src: Uid(src.into()),
            dst: Uid(dst.into()),
            kind: EdgeKind::Calls,
            provenance: Provenance::Inferred,
            confidence: Confidence::new(0.9),
        }
    }

    /// A graph where `bar` calls `foo`.
    fn bar_calls_foo() -> Graph {
        let mut g = Graph::new();
        g.add_node(node("foo", "foo"));
        g.add_node(node("bar", "bar"));
        g.add_edge(calls("bar", "foo"));
        g
    }

    // ── contract-plane context fixtures (dogfood fix) ──

    /// A node with an explicit kind (the `node()` helper is always `Function`).
    fn node_kind(uid: &str, name: &str, kind: NodeKind) -> Node {
        Node {
            kind,
            ..node(uid, name)
        }
    }

    /// An edge of an explicit kind between two uids.
    fn edge(src: &str, dst: &str, kind: EdgeKind) -> Edge {
        Edge {
            src: Uid(src.into()),
            dst: Uid(dst.into()),
            kind,
            provenance: Provenance::Inferred,
            confidence: Confidence::new(0.9),
        }
    }

    /// The canonical contract shape:
    ///   `lambda` (LambdaFn) —Produces→ `field` (GraphqlField) ←Consumes— `mod` (Module).
    fn lambda_produces_field_consumed_by_mod() -> Graph {
        let mut g = Graph::new();
        g.add_node(node_kind(
            "lambda",
            "PolicyOperationsFunction",
            NodeKind::LambdaFn,
        ));
        g.add_node(node_kind("field", "getPolicyStats", NodeKind::GraphqlField));
        g.add_node(node_kind("mod", "policies.ts", NodeKind::Module));
        g.add_edge(edge("lambda", "field", EdgeKind::Produces));
        g.add_edge(edge("mod", "field", EdgeKind::Consumes));
        g
    }

    /// Names from a context bucket, in payload order.
    fn names(v: &Value, bucket: &str) -> Vec<String> {
        v[bucket]
            .as_array()
            .unwrap_or_else(|| panic!("bucket `{bucket}` must be a present array; got {v}"))
            .iter()
            .map(|n| n["name"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn context_field_shows_producer_and_consumer() {
        let g = lambda_produces_field_consumed_by_mod();
        let v = call_tool(&g, "context", &json!({ "symbol": "getPolicyStats" })).unwrap();

        // Incoming PRODUCES → the implementing Lambda is the producer.
        assert_eq!(names(&v, "producers"), vec!["PolicyOperationsFunction"]);
        // Incoming CONSUMES → the frontend module querying it is the consumer.
        assert_eq!(names(&v, "consumers"), vec!["policies.ts"]);
        // A schema field produces/consumes nothing outward.
        assert!(names(&v, "produces").is_empty());
        assert!(names(&v, "consumes").is_empty());
        // The code-plane buckets are empty for a schema field.
        assert!(names(&v, "callers").is_empty());
        assert!(names(&v, "callees").is_empty());
    }

    #[test]
    fn context_lambda_shows_outgoing_produces() {
        let g = lambda_produces_field_consumed_by_mod();
        let v = call_tool(
            &g,
            "context",
            &json!({ "symbol": "PolicyOperationsFunction" }),
        )
        .unwrap();

        // Outgoing PRODUCES → the field it implements.
        assert_eq!(names(&v, "produces"), vec!["getPolicyStats"]);
        // The Lambda is no one's producer/consumer and consumes nothing.
        assert!(names(&v, "producers").is_empty());
        assert!(names(&v, "consumers").is_empty());
        assert!(names(&v, "consumes").is_empty());
    }

    #[test]
    fn context_module_shows_outgoing_consumes() {
        let g = lambda_produces_field_consumed_by_mod();
        let v = call_tool(&g, "context", &json!({ "symbol": "policies.ts" })).unwrap();

        // Outgoing CONSUMES → the operation/field it calls.
        assert_eq!(names(&v, "consumes"), vec!["getPolicyStats"]);
        // The module is no one's producer/consumer and produces nothing.
        assert!(names(&v, "producers").is_empty());
        assert!(names(&v, "consumers").is_empty());
        assert!(names(&v, "produces").is_empty());
    }

    /// **I5 review: the dispatch-level seam guardrail.** `ContextResult.docs`
    /// itself stays fully covered by `strata-core`'s own unit tests AND the
    /// real-pipeline `knowledge_linking.rs` integration test — but NONE of
    /// those exercise the JSON DISPATCH seam (`tool_context`'s `"docs":
    /// ctx.docs.iter().map(doc_ref_json)...` line). Deleting that one line
    /// leaves every other K6 test green (they don't call `context` at all).
    /// This test closes that gap: it asserts the `"docs"` key is present in
    /// the ACTUAL JSON `call_tool` returns, with the fixture's one ref.
    ///
    /// Revert-checked (not just written): temporarily deleting the `"docs":
    /// …` line from `tool_context` makes this test fail with a `.expect()`
    /// panic on the missing key (confirmed locally, then restored) — see
    /// the task report's "I5 revert-check" section.
    #[test]
    fn context_dispatch_payload_carries_the_docs_bucket() {
        let mut g = Graph::new();
        g.add_node(node("target", "target"));
        g.add_node(node_kind("sec", "Heading", NodeKind::DocSection));
        g.add_edge(edge("sec", "target", EdgeKind::Mentions));
        let v = call_tool(&g, "context", &json!({ "symbol": "target" })).unwrap();
        let docs = v["docs"]
            .as_array()
            .expect("the \"docs\" key must be a JSON array in the context dispatch payload");
        assert_eq!(docs.len(), 1, "the fixture's one Mentions ref: {v}");
        assert_eq!(docs[0]["name"], "Heading");
    }

    // ── infra-plane context buckets on the MCP dispatch (Slice 10, B1a) ──
    //
    // The same six buckets the core surfaces must appear in the context JSON the
    // MCP/CLI/GUI share: assumes/assumed_by, routes_to/routed_from, runs/run_by.

    /// `fn1, fn2 —Assumes→ role`; `resolver —Routes→ ds —Routes→ fn1`;
    /// `fn1 —Runs→ handlerModule`. Exercises every infra bucket from both ends.
    fn infra_wired_graph() -> Graph {
        let mut g = Graph::new();
        g.add_node(node_kind("role", "UserRole", NodeKind::IamRole));
        g.add_node(node_kind("fn1", "UserFunction", NodeKind::LambdaFn));
        g.add_node(node_kind("fn2", "PyFunction", NodeKind::LambdaFn));
        g.add_node(node_kind("ds", "UserDS", NodeKind::AppSyncDataSource));
        g.add_node(node_kind(
            "resolver",
            "GetUserResolver",
            NodeKind::AppSyncResolver,
        ));
        g.add_node(node_kind("handlerModule", "user.ts", NodeKind::Module));
        g.add_edge(edge("fn1", "role", EdgeKind::Assumes));
        g.add_edge(edge("fn2", "role", EdgeKind::Assumes));
        g.add_edge(edge("resolver", "ds", EdgeKind::Routes));
        g.add_edge(edge("ds", "fn1", EdgeKind::Routes));
        g.add_edge(edge("fn1", "handlerModule", EdgeKind::Runs));
        g
    }

    #[test]
    fn context_role_emits_assumed_by_bucket() {
        let g = infra_wired_graph();
        let v = call_tool(&g, "context", &json!({ "symbol": "UserRole" })).unwrap();
        // The role's assuming Lambdas, sorted by uid (fn1, fn2).
        assert_eq!(names(&v, "assumed_by"), vec!["UserFunction", "PyFunction"]);
        assert!(names(&v, "assumes").is_empty(), "a role assumes nothing");
    }

    #[test]
    fn context_datasource_emits_routes_buckets() {
        let g = infra_wired_graph();
        let v = call_tool(&g, "context", &json!({ "symbol": "UserDS" })).unwrap();
        assert_eq!(names(&v, "routed_from"), vec!["GetUserResolver"]);
        assert_eq!(names(&v, "routes_to"), vec!["UserFunction"]);
    }

    #[test]
    fn context_module_emits_run_by_bucket() {
        let g = infra_wired_graph();
        let v = call_tool(&g, "context", &json!({ "symbol": "user.ts" })).unwrap();
        assert_eq!(names(&v, "run_by"), vec!["UserFunction"]);
        assert!(names(&v, "runs").is_empty());
    }

    // ── data-plane context bucket on the MCP dispatch (Slice 25, D3, M2b) ──
    //
    // A Table's `mapped_by` lists the ORM model classes that map to it; a model
    // class's `maps_to` is its Table. Surfaced through the same one dispatch.

    #[test]
    fn context_table_emits_mapped_by_bucket() {
        // UserModel —MapsTo→ users (Table). The table's mapped_by lists the model;
        // the model's maps_to is the table.
        let mut g = Graph::new();
        g.add_node(node_kind("users", "users", NodeKind::Table));
        g.add_node(node_kind("UserModel", "User", NodeKind::Class));
        g.add_edge(edge("UserModel", "users", EdgeKind::MapsTo));

        let table = call_tool(&g, "context", &json!({ "symbol": "users" })).unwrap();
        assert_eq!(
            names(&table, "mapped_by"),
            vec!["User"],
            "a table's mapped_by lists the ORM model that maps to it"
        );
        assert!(
            names(&table, "maps_to").is_empty(),
            "a table maps to nothing outward"
        );

        let model = call_tool(&g, "context", &json!({ "symbol": "User" })).unwrap();
        assert_eq!(names(&model, "maps_to"), vec!["users"]);
        assert!(names(&model, "mapped_by").is_empty());
    }

    #[test]
    fn context_field_has_all_six_infra_buckets_present() {
        // Every node's context surfaces all six infra buckets, present as arrays
        // (the GUI/CLI render a fixed bucket set; absence would crash them).
        let g = infra_wired_graph();
        let v = call_tool(&g, "context", &json!({ "symbol": "UserFunction" })).unwrap();
        for bucket in [
            "assumes",
            "assumed_by",
            "routes_to",
            "routed_from",
            "runs",
            "run_by",
        ] {
            assert!(
                v.get(bucket).map(Value::is_array).unwrap_or(false),
                "infra bucket `{bucket}` must be a present array, got {:?}",
                v.get(bucket)
            );
        }
        // The Lambda's own view: assumes the role, routed_from the DS, runs the module.
        assert_eq!(names(&v, "assumes"), vec!["UserRole"]);
        assert_eq!(names(&v, "routed_from"), vec!["UserDS"]);
        assert_eq!(names(&v, "runs"), vec!["user.ts"]);
    }

    #[test]
    fn context_unlinked_field_has_four_empty_contract_buckets() {
        // The honesty case: a GraphqlField with no PRODUCES/CONSUMES edges (a dead
        // schema field) must still surface all four contract buckets, present and
        // empty — `producers (0) / consumers (0)` is the dead-surface signal.
        let mut g = Graph::new();
        g.add_node(node_kind(
            "dead",
            "getActiveGeneralPolicies",
            NodeKind::GraphqlField,
        ));
        let v = call_tool(
            &g,
            "context",
            &json!({ "symbol": "getActiveGeneralPolicies" }),
        )
        .unwrap();

        for bucket in ["producers", "consumers", "produces", "consumes"] {
            assert!(
                v.get(bucket).map(Value::is_array).unwrap_or(false),
                "bucket `{bucket}` must be PRESENT as an array, got {:?}",
                v.get(bucket)
            );
            assert!(
                names(&v, bucket).is_empty(),
                "unlinked field bucket `{bucket}` must be empty"
            );
        }
    }

    #[test]
    fn impact_includes_caller() {
        let g = bar_calls_foo();
        let v = call_tool(&g, "impact", &json!({ "symbol": "foo" })).unwrap();
        let affected = v["affected"].as_array().unwrap();
        let names: Vec<&str> = affected
            .iter()
            .map(|a| a["name"].as_str().unwrap())
            .collect();
        assert!(
            names.contains(&"bar"),
            "impact(foo) must include bar; got {names:?}"
        );
        assert_eq!(v["target"]["name"], "foo");
    }

    #[test]
    fn impact_affected_carries_will_break_label() {
        // bar reaches foo cleanly at 0.9 ≥ the 0.40 floor ⇒ will_break: true. The
        // field is ADDITIVE: the pre-existing keys keep their values and order.
        let g = bar_calls_foo();
        let v = call_tool(&g, "impact", &json!({ "symbol": "foo" })).unwrap();
        let bar = v["affected"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["name"] == "bar")
            .expect("bar is affected");
        assert_eq!(
            bar["will_break"],
            json!(true),
            "the additive will_break field is present and true for a clean, high-confidence dependent"
        );
        // Additive-only: the pre-existing fields are unchanged.
        assert!(bar["uid"].is_string());
        assert!(bar["depth"].is_u64());
        assert!(bar["confidence"].is_number());
        assert_eq!(bar["ambiguous"], json!(false));
        // K7 fix F1: `kind` is now present too — a plain code dependent reports
        // its real NodeKind ("Function" here), same vocabulary as `node_json`.
        assert_eq!(bar["kind"], "Function");
    }

    #[test]
    fn impact_affected_kind_lets_a_caller_recognize_a_doc_dependent() {
        // K7 fix F1 (the serious reviewer finding): `will_break` is confidence/
        // ambiguous-only — it has NO idea a dependent is a doc. A DocSection that
        // documents `foo` at a real Extracted 0.95 (doc-comment strength) reaches
        // impact(foo) and is mechanically labelled will_break:true, exactly like a
        // code caller would be. Only the new `kind` field lets a caller (the agent
        // steering, or a human) recognize this is a doc and downgrade the reading
        // to "needs review" instead of trusting a false "WILL BREAK" — see
        // docs/src/reference/mcp.md and the STEERING Always Do block.
        let mut g = bar_calls_foo();
        g.add_node(Node {
            uid: Uid("doc|r|g.md|g.md#about-foo|".into()),
            kind: NodeKind::DocSection,
            name: "g.md#about-foo".into(),
            fqn: "g.md#g.md#about-foo".into(),
            path: "g.md".into(),
            span: Span::default(),
            provenance: Provenance::Extracted,
            confidence: Confidence::new(1.0),
        });
        g.add_edge(Edge {
            src: Uid("doc|r|g.md|g.md#about-foo|".into()),
            dst: Uid("foo".into()),
            kind: EdgeKind::Documents,
            provenance: Provenance::Extracted,
            confidence: Confidence::new(0.95),
        });

        let v = call_tool(&g, "impact", &json!({ "symbol": "foo" })).unwrap();
        let affected = v["affected"].as_array().unwrap();

        let doc = affected
            .iter()
            .find(|a| a["name"] == "g.md#about-foo")
            .expect("the DocSection reaches impact(foo) via Documents");
        assert_eq!(doc["kind"], "DocSection");
        assert_eq!(
            doc["will_break"],
            json!(true),
            "will_break stays MECHANICAL (confidence/ambiguous only, unchanged by \
             this fix) — a high-confidence Documents edge mechanically labels the \
             doc WILL BREAK even though it never can; `kind` is what lets a caller \
             apply the doc-kind downgrade instead of trusting the bool blindly"
        );

        // The plain code dependent (bar) is unaffected by the doc addition.
        let bar = affected
            .iter()
            .find(|a| a["name"] == "bar")
            .expect("bar is still affected");
        assert_eq!(bar["kind"], "Function");
        assert_eq!(bar["will_break"], json!(true));
    }

    // ── include_contracts on the impact tool (the one-dispatch-path fix) ──
    //
    // A producer→field←consumer contract shape: `Calls`→producer, producer
    // `Produces`→field, consumerModule `Consumes`→field. `impact(producer)`
    // reaches the consumerModule ONLY via the contract plane, so toggling
    // `include_contracts` on the tool args flips whether it appears.

    /// `Function —Calls→ producer —Produces→ Field ←Consumes— consumerModule`.
    /// Impact on `producer` surfaces `consumerModule` iff contracts are on.
    fn producer_field_consumer() -> Graph {
        let mut g = Graph::new();
        g.add_node(node_kind(
            "producer",
            "PolicyOperationsFunction",
            NodeKind::LambdaFn,
        ));
        g.add_node(node_kind("field", "getPolicyStats", NodeKind::GraphqlField));
        g.add_node(node_kind("consumerModule", "policies.ts", NodeKind::Module));
        g.add_edge(edge("producer", "field", EdgeKind::Produces));
        g.add_edge(edge("consumerModule", "field", EdgeKind::Consumes));
        g
    }

    /// Names in the impact `affected` array for `symbol`, with the given args
    /// merged onto `{symbol}` (so a test can add `include_contracts`).
    fn impact_affected_names(g: &Graph, symbol: &str, extra: Value) -> Vec<String> {
        let mut args = json!({ "symbol": symbol });
        if let Some(obj) = extra.as_object() {
            for (k, v) in obj {
                args[k] = v.clone();
            }
        }
        let res = call_tool(g, "impact", &args).unwrap();
        res["affected"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["name"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn impact_include_contracts_false_excludes_consumer() {
        let g = producer_field_consumer();
        let names = impact_affected_names(
            &g,
            "PolicyOperationsFunction",
            json!({ "include_contracts": false }),
        );
        assert!(
            !names.iter().any(|n| n == "policies.ts"),
            "include_contracts:false must NOT surface the cross-plane consumer; got {names:?}"
        );
    }

    #[test]
    fn impact_include_contracts_true_includes_consumer() {
        let g = producer_field_consumer();
        let names = impact_affected_names(
            &g,
            "PolicyOperationsFunction",
            json!({ "include_contracts": true }),
        );
        assert!(
            names.iter().any(|n| n == "policies.ts"),
            "include_contracts:true must surface the cross-plane consumer; got {names:?}"
        );
    }

    #[test]
    fn impact_include_contracts_omitted_defaults_true_includes_consumer() {
        // The default-pinning case: with NO include_contracts arg the tool must
        // behave as include_contracts:true (the engine default), so the consumer
        // is in the blast radius.
        let g = producer_field_consumer();
        let names = impact_affected_names(&g, "PolicyOperationsFunction", json!({}));
        assert!(
            names.iter().any(|n| n == "policies.ts"),
            "omitted include_contracts must default to true and surface the consumer; got {names:?}"
        );
    }

    // ── include_infra on the impact tool (Slice 10, B1b) ──
    //
    // A role ←Assumes— Lambda shape: impact(role) reaches the Lambda ONLY via the
    // infra plane, so toggling `include_infra` on the tool args flips whether it
    // appears. Mirrors the include_contracts precedent (incl. the omitted-arg pin).

    /// `Lambda —Assumes→ Role`. impact(Role) surfaces the Lambda iff infra is on.
    fn role_assumed_by_lambda() -> Graph {
        let mut g = Graph::new();
        g.add_node(node_kind("role", "UserRole", NodeKind::IamRole));
        g.add_node(node_kind("lambda", "UserFunction", NodeKind::LambdaFn));
        g.add_edge(edge("lambda", "role", EdgeKind::Assumes));
        g
    }

    #[test]
    fn impact_include_infra_false_excludes_assuming_lambda() {
        let g = role_assumed_by_lambda();
        let names = impact_affected_names(&g, "UserRole", json!({ "include_infra": false }));
        assert!(
            !names.iter().any(|n| n == "UserFunction"),
            "include_infra:false must NOT surface the assuming Lambda; got {names:?}"
        );
    }

    #[test]
    fn impact_include_infra_true_includes_assuming_lambda() {
        let g = role_assumed_by_lambda();
        let names = impact_affected_names(&g, "UserRole", json!({ "include_infra": true }));
        assert!(
            names.iter().any(|n| n == "UserFunction"),
            "include_infra:true must surface the assuming Lambda; got {names:?}"
        );
    }

    #[test]
    fn impact_include_infra_omitted_defaults_true_includes_lambda() {
        // The default-pinning case: with NO include_infra arg the tool must behave
        // as include_infra:true (the engine default), so the Lambda is reached.
        let g = role_assumed_by_lambda();
        let names = impact_affected_names(&g, "UserRole", json!({}));
        assert!(
            names.iter().any(|n| n == "UserFunction"),
            "omitted include_infra must default to true and surface the Lambda; got {names:?}"
        );
    }

    #[test]
    fn impact_rejects_non_bool_include_infra() {
        let g = role_assumed_by_lambda();
        let err = call_tool(
            &g,
            "impact",
            &json!({ "symbol": "UserRole", "include_infra": "yes" }),
        )
        .unwrap_err();
        assert!(
            matches!(err, ToolError::BadArgs(_)),
            "non-bool include_infra is bad args"
        );
    }

    // ── explain tool (the evidence chain on the dispatch seam) ──────────────────
    //
    // The engine is exhaustively tested in strata-core; these pin the *dispatch*:
    // the serialized chain shape, the honest unreachable payload, and resolution
    // (ambiguous target → Ambiguous, unknown → NotFound) matching impact.

    #[test]
    fn explain_serializes_the_contract_chain() {
        // producer —Produces→ field ←Consumes— consumerModule. explain(producer,
        // consumerModule) returns a 2-hop chain (Produces then Consumes) with the
        // running confidence and the consistency-matched overall confidence.
        let g = producer_field_consumer();
        let v = call_tool(
            &g,
            "explain",
            &json!({ "symbol": "PolicyOperationsFunction", "affected": "policies.ts" }),
        )
        .unwrap();
        assert_eq!(v["reachable"], json!(true));
        assert_eq!(v["target"]["name"], "PolicyOperationsFunction");
        assert_eq!(v["affected"]["name"], "policies.ts");
        let hops = v["hops"].as_array().unwrap();
        assert_eq!(hops.len(), 2, "producer→field→consumer: {hops:?}");
        assert_eq!(hops[0]["edge_kind"], "Produces");
        assert_eq!(hops[0]["to"], "field");
        assert_eq!(hops[1]["edge_kind"], "Consumes");
        assert_eq!(hops[1]["to"], "consumerModule");
        // Each hop carries provenance + running confidence; the last running ==
        // the overall confidence (the consistency invariant, through the tool).
        assert!(hops[0]["provenance"].is_string());
        assert!(hops[1]["running_confidence"].is_number());
        let overall = v["confidence"].as_f64().unwrap();
        let last_running = hops[1]["running_confidence"].as_f64().unwrap();
        assert!(
            (overall - last_running).abs() < 1e-9,
            "overall confidence must equal the final hop's running confidence"
        );
        // And it equals what the impact tool reports for the same node.
        let imp = call_tool(
            &g,
            "impact",
            &json!({ "symbol": "PolicyOperationsFunction" }),
        )
        .unwrap();
        let imp_conf = imp["affected"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["name"] == "policies.ts")
            .expect("consumer is affected")["confidence"]
            .as_f64()
            .unwrap();
        assert!(
            (overall - imp_conf).abs() < 1e-6,
            "explain tool's confidence {overall} must equal the impact tool's {imp_conf}"
        );
    }

    #[test]
    fn explain_unreachable_is_an_honest_negative_not_an_error() {
        // `island` is an isolated node — it does not depend on `foo` at all, so
        // it is not in foo's blast radius and there is nothing to explain.
        let mut g = bar_calls_foo();
        g.add_node(node("island", "island"));
        let v = call_tool(
            &g,
            "explain",
            &json!({ "symbol": "foo", "affected": "island" }),
        )
        .unwrap();
        assert_eq!(
            v["reachable"],
            json!(false),
            "an unreachable affected node is reachable:false, not an error"
        );
        assert!(
            v["reason"]
                .as_str()
                .unwrap()
                .contains("not in foo's blast radius"),
            "the honest reason names the absence; got {v}"
        );
        assert!(v.get("hops").is_none(), "no chain when unreachable");
    }

    #[test]
    fn explain_target_alias_resolves_like_symbol() {
        // `target` is accepted as an alias for `symbol` (the changed node).
        let g = bar_calls_foo();
        let v = call_tool(
            &g,
            "explain",
            &json!({ "target": "foo", "affected": "bar" }),
        )
        .unwrap();
        assert_eq!(v["reachable"], json!(true));
        let hops = v["hops"].as_array().unwrap();
        assert_eq!(hops.len(), 1, "bar →Calls→ foo is one hop");
        assert_eq!(hops[0]["edge_kind"], "Calls");
        assert_eq!(hops[0]["from"], "foo");
        assert_eq!(hops[0]["to"], "bar");
    }

    #[test]
    fn explain_unknown_target_is_not_found() {
        let g = bar_calls_foo();
        let err = call_tool(
            &g,
            "explain",
            &json!({ "symbol": "zzz", "affected": "foo" }),
        )
        .unwrap_err();
        assert!(matches!(err, ToolError::NotFound(s) if s == "zzz"));
    }

    #[test]
    fn explain_ambiguous_target_returns_candidates_not_error() {
        // An ambiguous TARGET no longer dead-ends: explain returns the candidate
        // list (mirroring context/impact) so the agent can pin one with `uid`,
        // and names which end is ambiguous.
        let mut g = Graph::new();
        g.add_node(node("u1", "dup"));
        g.add_node(node("u2", "dup"));
        g.add_node(node("a", "a"));
        let v = call_tool(&g, "explain", &json!({ "symbol": "dup", "affected": "a" })).unwrap();
        assert_eq!(v["ambiguous"], true);
        assert_eq!(v["symbol"], "dup");
        assert_eq!(v["ambiguous_end"], "target");
        let cands = v["candidates"].as_array().unwrap();
        assert_eq!(cands.len(), 2);
        assert_eq!(cands[0]["uid"], "u1");
        assert_eq!(cands[1]["uid"], "u2");
    }

    #[test]
    fn explain_ambiguous_affected_returns_candidates_not_error() {
        // The OTHER end: a unique target but an ambiguous `affected` likewise
        // returns candidates (ambiguous_end: "affected"), never an error.
        let mut g = Graph::new();
        g.add_node(node("t", "t"));
        g.add_node(node("u1", "dup"));
        g.add_node(node("u2", "dup"));
        let v = call_tool(&g, "explain", &json!({ "symbol": "t", "affected": "dup" })).unwrap();
        assert_eq!(v["ambiguous"], true);
        assert_eq!(v["symbol"], "dup");
        assert_eq!(v["ambiguous_end"], "affected");
        assert_eq!(v["candidates"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn explain_target_uid_pin_resolves_the_node() {
        // Pinning the ambiguous target by uid resolves it and runs explain
        // normally (affected `a` calls one of the dups → reachable).
        let mut g = Graph::new();
        g.add_node(node("u1", "dup"));
        g.add_node(node("u2", "dup"));
        g.add_node(node("a", "a"));
        g.add_edge(calls("a", "u1")); // a depends on the u1 `dup`
        let v = call_tool(
            &g,
            "explain",
            &json!({ "symbol": "dup", "affected": "a", "uid": "u1" }),
        )
        .unwrap();
        assert_eq!(v["reachable"], json!(true));
        assert_eq!(v["target"]["uid"], "u1");
        let hops = v["hops"].as_array().unwrap();
        assert_eq!(hops.len(), 1, "a →Calls→ u1 is one hop");
    }

    #[test]
    fn explain_affected_uid_pin_resolves_the_node() {
        // Pinning the ambiguous affected by `affected_uid`.
        let mut g = Graph::new();
        g.add_node(node("t", "t"));
        g.add_node(node("u1", "dup"));
        g.add_node(node("u2", "dup"));
        g.add_edge(calls("u1", "t")); // the u1 `dup` depends on t
        let v = call_tool(
            &g,
            "explain",
            &json!({ "symbol": "t", "affected": "dup", "affected_uid": "u1" }),
        )
        .unwrap();
        assert_eq!(v["reachable"], json!(true));
        assert_eq!(v["affected"]["uid"], "u1");
    }

    #[test]
    fn explain_unknown_uid_pin_is_not_found() {
        let mut g = Graph::new();
        g.add_node(node("u1", "dup"));
        g.add_node(node("u2", "dup"));
        g.add_node(node("a", "a"));
        let err = call_tool(
            &g,
            "explain",
            &json!({ "symbol": "dup", "affected": "a", "uid": "nope" }),
        )
        .unwrap_err();
        assert!(matches!(err, ToolError::NotFound(s) if s == "nope"));
    }

    #[test]
    fn explain_missing_affected_is_bad_args() {
        let g = bar_calls_foo();
        let err = call_tool(&g, "explain", &json!({ "symbol": "foo" })).unwrap_err();
        assert!(matches!(err, ToolError::BadArgs(_)));
    }

    #[test]
    fn context_returns_caller_and_callee_buckets() {
        let g = bar_calls_foo();
        // context of foo: bar is a caller, callees empty.
        let v = call_tool(&g, "context", &json!({ "symbol": "foo" })).unwrap();
        let callers: Vec<&str> = v["callers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["name"].as_str().unwrap())
            .collect();
        assert_eq!(callers, vec!["bar"]);
        assert!(v["callees"].as_array().unwrap().is_empty());

        // context of bar: foo is a callee, callers empty.
        let v2 = call_tool(&g, "context", &json!({ "symbol": "bar" })).unwrap();
        let callees: Vec<&str> = v2["callees"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["name"].as_str().unwrap())
            .collect();
        assert_eq!(callees, vec!["foo"]);
        assert!(v2["callers"].as_array().unwrap().is_empty());
    }

    #[test]
    fn query_returns_name_matches() {
        let g = bar_calls_foo();
        let v = call_tool(&g, "query", &json!({ "text": "foo" })).unwrap();
        let matches: Vec<&str> = v["matches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["name"].as_str().unwrap())
            .collect();
        assert_eq!(matches, vec!["foo"]);
    }

    // ── blast tool (the pre-edit file blast radius on the dispatch seam) ──
    //
    // The engine is exhaustively tested in strata-index (incl. the parity with the
    // detect_changes aggregation); these pin the *dispatch*: the serialized
    // BlastReport shape, and the honest empty report for an unindexed file.

    #[test]
    fn blast_tool_serializes_the_file_blast_report() {
        // bar calls foo, both in their own files (node() sets path == "<uid>.ts").
        // blast("foo.ts") must report foo as a defined symbol and bar as affected.
        let g = bar_calls_foo();
        let v = call_tool(&g, "blast", &json!({ "file": "foo.ts" })).unwrap();
        assert_eq!(v["file"], "foo.ts");
        let symbols = v["symbols"].as_array().unwrap();
        assert!(
            symbols.iter().any(|s| s["fqn"] == "foo"),
            "blast lists the file's symbol foo; got {symbols:?}"
        );
        let affected = v["affected"].as_array().unwrap();
        assert!(
            affected.iter().any(|a| a["name"] == "bar"),
            "blast surfaces the dependent bar; got {affected:?}"
        );
        assert!(v["risk"]["level"].is_string(), "carries a risk level");
    }

    #[test]
    fn blast_tool_unindexed_file_is_an_honest_empty_report() {
        // A file the graph knows nothing about → an explicit empty report with a
        // note (never a fabricated all-clear).
        let g = bar_calls_foo();
        let v = call_tool(&g, "blast", &json!({ "file": "brand/new.ts" })).unwrap();
        assert!(v["symbols"].as_array().unwrap().is_empty());
        assert!(v["affected"].as_array().unwrap().is_empty());
        assert_eq!(v["risk"]["level"], "LOW");
        assert!(
            v["note"].as_str().unwrap().contains("no indexed symbols"),
            "the empty report must carry the honest note; got {v}"
        );
    }

    #[test]
    fn blast_tool_missing_file_arg_is_bad_args() {
        let g = bar_calls_foo();
        let err = call_tool(&g, "blast", &json!({})).unwrap_err();
        assert!(matches!(err, ToolError::BadArgs(_)));
    }

    #[test]
    fn unknown_symbol_is_not_found() {
        let g = bar_calls_foo();
        let err = call_tool(&g, "impact", &json!({ "symbol": "zzz" })).unwrap_err();
        assert!(matches!(err, ToolError::NotFound(s) if s == "zzz"));
    }

    #[test]
    fn impact_returns_candidates_when_ambiguous_not_error() {
        // The headline fix: an ambiguous symbol no longer dead-ends with a bare
        // count. impact mirrors context — `{ambiguous, symbol, candidates}` — so
        // the agent can pin one with `uid` (NOT a ToolError::Ambiguous).
        let mut g = Graph::new();
        // Both nodes have fqn "dup" (node() sets fqn == name), so the fqn tier
        // itself returns two candidates → ambiguous.
        g.add_node(node("u1", "dup"));
        g.add_node(node("u2", "dup"));
        let v = call_tool(&g, "impact", &json!({ "symbol": "dup" })).unwrap();
        assert_eq!(v["ambiguous"], true);
        assert_eq!(v["symbol"], "dup");
        let cands = v["candidates"].as_array().unwrap();
        assert_eq!(cands.len(), 2);
        // Each candidate carries the uid/name/kind/path node view (sorted by uid).
        assert_eq!(cands[0]["uid"], "u1");
        assert_eq!(cands[0]["kind"], "Function");
        assert!(cands[0]["name"].is_string() && cands[0]["path"].is_string());
        assert_eq!(cands[1]["uid"], "u2");
        // The candidates payload is NOT an impact result: no `affected`/`target`.
        assert!(v.get("affected").is_none());
        assert!(v.get("target").is_none());
    }

    #[test]
    fn impact_uid_pin_resolves_the_node_and_runs_impact() {
        // With a `uid` pin the ambiguity is resolved straight from the graph and
        // impact runs normally on that exact node.
        let mut g = Graph::new();
        g.add_node(node("u1", "dup"));
        g.add_node(node("u2", "dup"));
        g.add_node(node("caller", "caller"));
        g.add_edge(calls("caller", "u1")); // caller depends on the u1 `dup`
        let v = call_tool(&g, "impact", &json!({ "symbol": "dup", "uid": "u1" })).unwrap();
        // A real impact result for u1 (not a candidates payload).
        assert_eq!(v["target"]["uid"], "u1");
        let names: Vec<&str> = v["affected"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["name"].as_str().unwrap())
            .collect();
        assert!(
            names.contains(&"caller"),
            "impact on the pinned u1 must include its dependent caller; got {names:?}"
        );
    }

    // ── impact members_with_dependents: structured member-dep surfacing ──
    //
    // A member-bearing target whose own blast radius is empty must NOT look dead:
    // the impact tool JSON carries `members_with_dependents` (uid/name/kind) so an
    // agent can pin one and re-run. Mirrors the candidates pattern: a structured
    // field, populated only on the zero-direct case. The normal (non-empty-affected)
    // result shape is unchanged.

    /// `Widget` (Class) —Defines→ `render` (Method); `caller` —Calls→ `render`. So
    /// impact(Widget) is zero-direct but `render` has a dependent.
    fn widget_with_member_caller() -> Graph {
        let mut g = Graph::new();
        g.add_node(node_kind("widget", "Widget", NodeKind::Class));
        g.add_node(node_kind("render", "render", NodeKind::Method));
        g.add_node(node("caller", "caller"));
        g.add_edge(edge("widget", "render", EdgeKind::Defines));
        g.add_edge(calls("caller", "render"));
        g
    }

    #[test]
    fn impact_surfaces_members_with_dependents_on_zero_direct_case() {
        let g = widget_with_member_caller();
        let v = call_tool(&g, "impact", &json!({ "symbol": "Widget" })).unwrap();
        // Zero direct dependents on the type itself.
        assert!(
            v["affected"].as_array().unwrap().is_empty(),
            "the type itself has no direct dependents; got {v}"
        );
        // The structured field lists the member that HAS a dependent.
        let members = v["members_with_dependents"]
            .as_array()
            .unwrap_or_else(|| panic!("members_with_dependents must be a present array; got {v}"));
        assert_eq!(members.len(), 1, "exactly one member has a dependent");
        assert_eq!(members[0]["uid"], "render");
        assert_eq!(members[0]["name"], "render");
        assert_eq!(
            members[0]["kind"], "Method",
            "the member carries its node kind so an agent can act on it"
        );
    }

    #[test]
    fn impact_members_with_dependents_absent_when_affected_non_empty() {
        // The normal path: a target with direct dependents must NOT carry the field
        // (it is the zero-direct fallback only) — the result shape is unchanged.
        let g = bar_calls_foo();
        let v = call_tool(&g, "impact", &json!({ "symbol": "foo" })).unwrap();
        assert!(
            !v["affected"].as_array().unwrap().is_empty(),
            "foo has a direct dependent (bar)"
        );
        assert!(
            v.get("members_with_dependents").is_none(),
            "members_with_dependents must be absent on the non-empty-affected path; got {v}"
        );
    }

    #[test]
    fn impact_dead_container_has_no_members_with_dependents() {
        // A container with a member that has NO caller is genuinely dead: zero
        // affected AND no members_with_dependents (honest — dead = dead).
        let mut g = Graph::new();
        g.add_node(node_kind("Dead", "Dead", NodeKind::Class));
        g.add_node(node_kind("noop", "noop", NodeKind::Method));
        g.add_edge(edge("Dead", "noop", EdgeKind::Defines));
        let v = call_tool(&g, "impact", &json!({ "symbol": "Dead" })).unwrap();
        assert!(v["affected"].as_array().unwrap().is_empty());
        // Absent OR empty is acceptable; it must never list a phantom member.
        let absent_or_empty = v
            .get("members_with_dependents")
            .map(|m| m.as_array().map(|a| a.is_empty()).unwrap_or(false))
            .unwrap_or(true);
        assert!(
            absent_or_empty,
            "a dead container must surface no member-dependents; got {v}"
        );
    }

    #[test]
    fn impact_unknown_uid_pin_is_not_found() {
        // A `uid` that is not in the graph is a clear NotFound — never a silent
        // fall-back to name resolution (which would risk picking the wrong node).
        let mut g = Graph::new();
        g.add_node(node("u1", "dup"));
        g.add_node(node("u2", "dup"));
        let err = call_tool(&g, "impact", &json!({ "symbol": "dup", "uid": "nope" })).unwrap_err();
        assert!(matches!(err, ToolError::NotFound(s) if s == "nope"));
    }

    #[test]
    fn impact_rejects_non_string_uid() {
        let g = bar_calls_foo();
        let err = call_tool(&g, "impact", &json!({ "symbol": "foo", "uid": 7 })).unwrap_err();
        assert!(
            matches!(err, ToolError::BadArgs(_)),
            "non-string uid is bad args"
        );
    }

    #[test]
    fn context_returns_candidates_when_ambiguous() {
        let mut g = Graph::new();
        g.add_node(node("u1", "dup"));
        g.add_node(node("u2", "dup"));
        let v = call_tool(&g, "context", &json!({ "symbol": "dup" })).unwrap();
        assert_eq!(v["ambiguous"], true);
        assert_eq!(v["candidates"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn unknown_tool_name_is_bad_args() {
        let g = bar_calls_foo();
        let err = call_tool(&g, "frobnicate", &json!({})).unwrap_err();
        assert!(matches!(err, ToolError::BadArgs(_)));
    }

    #[test]
    fn missing_required_arg_is_bad_args() {
        let g = bar_calls_foo();
        let err = call_tool(&g, "query", &json!({})).unwrap_err();
        assert!(matches!(err, ToolError::BadArgs(_)));
    }

    #[test]
    fn tool_schemas_lists_the_nine_object_schemas() {
        let schemas = tool_schemas();
        let arr = schemas.as_array().unwrap();
        assert_eq!(arr.len(), 9);
        let names: Vec<&str> = arr.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(
            names,
            vec![
                "context",
                "impact",
                "explain",
                "query",
                "blast",
                "detect_changes",
                "rename",
                "search_docs",
                "guidance"
            ]
        );
        for t in arr {
            assert_eq!(t["inputSchema"]["type"], "object");
            assert!(t["inputSchema"]["properties"].is_object());
        }
    }

    // ── detect_changes dispatch (ctx-less error + ctx-aware happy path) ──
    //
    // The ctx-less `call_tool` cannot reach a working tree, so `detect_changes`
    // through it must be a clear, actionable error — never a guessed empty report.
    // The ctx-aware `call_tool_ctx` over a real tempdir git repo serializes the
    // ChangeReport. (The engine itself is exhaustively tested in
    // strata-index/tests/detect_changes.rs; this pins the *dispatch* seam.)

    #[test]
    fn detect_changes_without_ctx_is_a_clear_error() {
        let g = Graph::new();
        let err = call_tool(&g, "detect_changes", &json!({})).unwrap_err();
        match err {
            ToolError::BadArgs(msg) => assert!(
                msg.contains("needs a repo root"),
                "ctx-less detect_changes must name the missing repo root; got: {msg}"
            ),
            other => panic!("expected BadArgs, got {other:?}"),
        }
    }

    #[test]
    fn detect_changes_rejects_non_bool_staged() {
        let g = Graph::new();
        let ctx = ToolCtx {
            repo_root: Some(std::path::PathBuf::from("/tmp")),
            ..ToolCtx::default()
        };
        let err =
            call_tool_ctx(&g, &ctx, "detect_changes", &json!({ "staged": "yes" })).unwrap_err();
        assert!(
            matches!(err, ToolError::BadArgs(_)),
            "non-bool staged is bad args"
        );
    }

    #[test]
    fn detect_changes_through_ctx_serializes_a_report() {
        use std::process::Command;
        // A real tempdir git repo: commit a baseline, then modify a function body
        // in the working tree. The dispatch must surface the ChangeReport shape.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .output()
                .expect("spawn git");
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        git(&["config", "commit.gpgsign", "false"]);
        std::fs::write(dir.join("a.ts"), "export function f() { return 1; }\n").unwrap();
        git(&["add", "-A"]);
        git(&[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "baseline",
        ]);
        // Modify the working tree.
        std::fs::write(dir.join("a.ts"), "export function f() { return 2; }\n").unwrap();

        let g = Graph::new();
        let ctx = ToolCtx {
            repo_root: Some(dir.to_path_buf()),
            ..ToolCtx::default()
        };
        let v = call_tool_ctx(&g, &ctx, "detect_changes", &json!({})).unwrap();
        // The serialized ChangeReport shape: scope + a changed symbol `f` + a risk.
        assert_eq!(v["scope"], "working");
        assert!(
            v["risk"]["level"].is_string(),
            "report carries a risk level"
        );
        let syms = v["symbols"].as_array().unwrap();
        assert!(
            syms.iter().any(|s| s["key"] == "f"),
            "the modified function f must be a changed symbol; got {syms:?}"
        );
    }

    // ── rename dispatch (ctx-less error + ctx-aware dry-run plan) ──
    //
    // The engine is exhaustively tested in strata-index/tests/rename.rs; these
    // pin the *dispatch* seam: the ctx-less path errors clearly, and the ctx-aware
    // path serializes the RenameOutcome.

    #[test]
    fn rename_without_ctx_is_a_clear_error() {
        let g = Graph::new();
        let err =
            call_tool(&g, "rename", &json!({ "symbol": "foo", "new_name": "bar" })).unwrap_err();
        match err {
            ToolError::BadArgs(msg) => assert!(
                msg.contains("needs a repo root"),
                "ctx-less rename must name the missing repo root; got: {msg}"
            ),
            other => panic!("expected BadArgs, got {other:?}"),
        }
    }

    #[test]
    fn rename_through_ctx_serializes_a_dry_run_plan() {
        // A hand-built graph (one Function node `helper` in a.ts) + a matching file
        // on disk; the dispatch must surface a dry-run Plan (no write) with edits.
        // (The engine is exhaustively tested over the real indexer in
        // strata-index/tests/rename.rs; this only pins the dispatch seam.)
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::write(
            dir.join("a.ts"),
            "export function helper() { return 1; }\nexport function caller() { return helper(); }\n",
        )
        .unwrap();
        let mut g = Graph::new();
        // A Function node whose `path` is the real a.ts so the engine reads it.
        g.add_node(Node {
            uid: Uid("ts|app|a.ts|helper|()".into()),
            kind: NodeKind::Function,
            name: "helper".into(),
            fqn: "helper".into(),
            path: "a.ts".into(),
            span: Span::default(),
            provenance: Provenance::Extracted,
            confidence: Confidence::new(1.0),
        });

        let ctx = ToolCtx {
            repo_root: Some(dir.to_path_buf()),
            ..ToolCtx::default()
        };
        let v = call_tool_ctx(
            &g,
            &ctx,
            "rename",
            &json!({ "symbol": "helper", "new_name": "assist" }),
        )
        .unwrap();
        assert_eq!(v["outcome"], "plan", "a resolvable target yields a plan");
        assert_eq!(v["applied"], false, "dry-run by default — nothing written");
        let edits = v["edits"].as_array().unwrap();
        assert!(!edits.is_empty(), "the plan must list edits");
        // The file on disk is unchanged (dry run).
        assert!(
            std::fs::read_to_string(dir.join("a.ts"))
                .unwrap()
                .contains("function helper()"),
            "dry-run rename must not write"
        );
    }

    // ── search_docs dispatch (K5) ──
    //
    // The engine (writer + schema) is exhaustively tested in
    // `strata-index/src/docs_index.rs` and `strata-index/tests/docs_index.rs`
    // (including the real `index_repo` → `.strata/docs.idx` wiring); these pin
    // the *dispatch* seam: `search_docs` reads `ctx.repo_root`/`ctx.member_roots`
    // and never touches `graph`, a missing/corrupt index degrades honestly, and
    // an estate merge across several indices is deterministic.

    /// Build a real on-disk `docs.idx` at `<root>/.strata/docs.idx` from
    /// `entries` — the same writer `index_repo` calls, so this test fixture is
    /// byte-for-byte the shape `search_docs` reads in production.
    fn write_test_docs_index(root: &std::path::Path, entries: &[strata_index::DocsIndexEntry]) {
        let strata_dir = root.join(".strata");
        std::fs::create_dir_all(&strata_dir).unwrap();
        strata_index::write_docs_index(&strata_dir, entries).unwrap();
    }

    fn section_entry(uid: &str, anchor: &str, body: &str) -> strata_index::DocsIndexEntry {
        strata_index::DocsIndexEntry {
            uid: uid.to_string(),
            name: "Retry policy".to_string(),
            path: "README.md".to_string(),
            anchor: anchor.to_string(),
            kind: strata_index::DocsEntryKind::Section,
            body: body.to_string(),
        }
    }

    #[test]
    fn search_docs_returns_capped_labeled_hits() {
        let tmp = tempfile::tempdir().unwrap();
        write_test_docs_index(
            tmp.path(),
            &[section_entry(
                "doc|r|README.md|README.md#retry-policy|",
                "retry-policy",
                "Always use exponential backoff.",
            )],
        );
        let g = Graph::new();
        let ctx = ToolCtx {
            repo_root: Some(tmp.path().to_path_buf()),
            ..ToolCtx::default()
        };
        let v = call_tool_ctx(&g, &ctx, "search_docs", &json!({ "query": "backoff" })).unwrap();
        let results = v["results"].as_array().unwrap();
        assert!(results.len() <= 5, "default limit is 5: {results:?}");
        assert!(!results.is_empty(), "a real hit must come back");
        assert_eq!(results[0]["kind"], "section");
        assert_eq!(results[0]["anchor"], "retry-policy");
        assert!(results[0]["snippet"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .contains("backoff"));
        assert!(results[0]["matched_terms"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t == "backoff"));
        // Never labeled as anything but a term match.
        assert!(v.get("note").is_none());
    }

    /// Review finding (Important, empirically reproduced): `matched_terms`
    /// used to test substring containment on lowercased raw text, so a query
    /// term that happens to be a substring of a real (different) token was
    /// wrongly reported as matched — `"category".contains("cat")` is `true`,
    /// but the token `"cat"` never actually occurs. A doc whose body is ONLY
    /// "category", queried with "category cat", must still be found (it DOES
    /// contain the token "category") but `matched_terms` must be `["category"]`
    /// only — never `"cat"`.
    #[test]
    fn search_docs_matched_terms_uses_token_equality_not_substring_containment() {
        let tmp = tempfile::tempdir().unwrap();
        write_test_docs_index(
            tmp.path(),
            &[section_entry(
                "doc|r|README.md|README.md#h|",
                "h",
                "category",
            )],
        );
        let g = Graph::new();
        let ctx = ToolCtx {
            repo_root: Some(tmp.path().to_path_buf()),
            ..ToolCtx::default()
        };
        let v =
            call_tool_ctx(&g, &ctx, "search_docs", &json!({ "query": "category cat" })).unwrap();
        let results = v["results"].as_array().unwrap();
        assert_eq!(
            results.len(),
            1,
            "the doc must still be found — it DOES contain the token \"category\": {results:?}"
        );
        let matched: Vec<&str> = results[0]["matched_terms"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t.as_str().unwrap())
            .collect();
        assert_eq!(
            matched,
            vec!["category"],
            "\"cat\" must NOT be reported as matched — \"category\".contains(\"cat\") is true \
             but the TOKEN \"cat\" never occurs (the old substring-containment bug); got {matched:?}"
        );
    }

    #[test]
    fn search_docs_without_index_is_empty_not_error() {
        let g = Graph::new();
        let ctx = ToolCtx::default();
        let v = call_tool_ctx(&g, &ctx, "search_docs", &json!({ "query": "x" })).unwrap();
        assert_eq!(v["results"].as_array().unwrap().len(), 0);
        assert_eq!(v["note"], "no docs index — run strata index");
    }

    #[test]
    fn search_docs_on_a_configured_but_never_indexed_repo_root_is_also_empty_not_error() {
        // `repo_root` IS set, but no `strata index` ever ran there — the
        // "corrupt/missing index" degrade path, not the ctx-less path.
        let tmp = tempfile::tempdir().unwrap();
        let g = Graph::new();
        let ctx = ToolCtx {
            repo_root: Some(tmp.path().to_path_buf()),
            ..ToolCtx::default()
        };
        let v = call_tool_ctx(&g, &ctx, "search_docs", &json!({ "query": "x" })).unwrap();
        assert_eq!(v["results"].as_array().unwrap().len(), 0);
        assert_eq!(v["note"], "no docs index — run strata index");
    }

    #[test]
    fn search_docs_treats_a_corrupt_index_directory_the_same_as_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let strata_dir = tmp.path().join(".strata");
        // A `docs.idx` directory that exists but is NOT a valid tantivy index
        // (no meta.json etc.) — must degrade, never panic/error.
        std::fs::create_dir_all(strata_dir.join("docs.idx")).unwrap();
        std::fs::write(strata_dir.join("docs.idx").join("garbage.txt"), b"nope").unwrap();

        let g = Graph::new();
        let ctx = ToolCtx {
            repo_root: Some(tmp.path().to_path_buf()),
            ..ToolCtx::default()
        };
        let v = call_tool_ctx(&g, &ctx, "search_docs", &json!({ "query": "x" })).unwrap();
        assert_eq!(v["results"].as_array().unwrap().len(), 0);
        assert_eq!(v["note"], "no docs index — run strata index");
    }

    #[test]
    fn search_docs_limit_is_capped_at_twenty_five_even_when_more_is_requested() {
        let tmp = tempfile::tempdir().unwrap();
        let entries: Vec<strata_index::DocsIndexEntry> = (0..40)
            .map(|i| {
                section_entry(
                    &format!("doc|r|README.md|README.md#s{i}|"),
                    &format!("s{i}"),
                    "widgetterm appears in every section here",
                )
            })
            .collect();
        write_test_docs_index(tmp.path(), &entries);

        let g = Graph::new();
        let ctx = ToolCtx {
            repo_root: Some(tmp.path().to_path_buf()),
            ..ToolCtx::default()
        };
        let v = call_tool_ctx(
            &g,
            &ctx,
            "search_docs",
            &json!({ "query": "widgetterm", "limit": 1000 }),
        )
        .unwrap();
        assert_eq!(
            v["results"].as_array().unwrap().len(),
            25,
            "limit must be hard-capped at 25 regardless of what is requested"
        );
    }

    #[test]
    fn search_docs_invalid_query_syntax_is_bad_args_not_a_silent_empty_result() {
        let tmp = tempfile::tempdir().unwrap();
        write_test_docs_index(
            tmp.path(),
            &[section_entry(
                "doc|r|README.md|README.md#h|",
                "h",
                "hello world",
            )],
        );
        let g = Graph::new();
        let ctx = ToolCtx {
            repo_root: Some(tmp.path().to_path_buf()),
            ..ToolCtx::default()
        };
        // An unbalanced quote is invalid tantivy query syntax — this must be a
        // clear caller error, never silently reported as "no docs index".
        let err = call_tool_ctx(
            &g,
            &ctx,
            "search_docs",
            &json!({ "query": "\"unterminated" }),
        )
        .unwrap_err();
        assert!(matches!(err, ToolError::BadArgs(_)), "{err:?}");
    }

    #[test]
    fn search_docs_estate_mode_merges_member_roots_deterministically() {
        // Two separate member repos, each with their own docs.idx, both
        // mentioning the same term with the SAME score (identical single-term
        // content) — the merge must be deterministic: score desc, uid asc
        // tie-break, every run.
        let repo_a = tempfile::tempdir().unwrap();
        let repo_b = tempfile::tempdir().unwrap();
        write_test_docs_index(
            repo_a.path(),
            &[section_entry(
                "doc|b-repo|README.md|README.md#h|",
                "h",
                "quorumflag appears here",
            )],
        );
        write_test_docs_index(
            repo_b.path(),
            &[section_entry(
                "doc|a-repo|README.md|README.md#h|",
                "h",
                "quorumflag appears here too",
            )],
        );

        let g = Graph::new();
        let ctx = ToolCtx {
            repo_root: None,
            member_roots: vec![repo_a.path().to_path_buf(), repo_b.path().to_path_buf()],
        };
        let v1 = call_tool_ctx(&g, &ctx, "search_docs", &json!({ "query": "quorumflag" })).unwrap();
        let v2 = call_tool_ctx(&g, &ctx, "search_docs", &json!({ "query": "quorumflag" })).unwrap();
        assert_eq!(
            v1, v2,
            "the same query against the same estate must merge identically every run"
        );
        let results = v1["results"].as_array().unwrap();
        assert_eq!(results.len(), 2, "both members' hits must be merged");
        let uids: Vec<&str> = results.iter().map(|r| r["uid"].as_str().unwrap()).collect();
        assert_eq!(
            uids,
            vec![
                "doc|a-repo|README.md|README.md#h|",
                "doc|b-repo|README.md|README.md#h|"
            ],
            "equal-score hits must tie-break by uid ascending: {uids:?}"
        );
    }

    #[test]
    fn search_docs_never_dispatches_through_the_graph_at_all() {
        // A completely empty graph must not stop search_docs from finding a
        // real hit — proof it is genuinely graph-independent, per its own
        // `call_tool_ctx` doc comment.
        let tmp = tempfile::tempdir().unwrap();
        write_test_docs_index(
            tmp.path(),
            &[section_entry(
                "doc|r|README.md|README.md#h|",
                "h",
                "zephyrqueue content",
            )],
        );
        let g = Graph::new();
        assert_eq!(g.node_count(), 0, "the graph really is empty");
        let ctx = ToolCtx {
            repo_root: Some(tmp.path().to_path_buf()),
            ..ToolCtx::default()
        };
        let v = call_tool_ctx(&g, &ctx, "search_docs", &json!({ "query": "zephyrqueue" })).unwrap();
        assert_eq!(v["results"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn graph_schema_lists_node_and_edge_kinds() {
        let s = graph_schema_json();
        let nodes: Vec<&str> = s["node_kinds"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        let edges: Vec<&str> = s["edge_kinds"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(nodes.contains(&"Function"));
        assert!(nodes.contains(&"Module"));
        assert!(nodes.contains(&"ApiOperation"));
        assert!(nodes.contains(&"GraphqlField"));
        assert!(nodes.contains(&"LambdaFn"));
        assert!(nodes.contains(&"CloudResource"));
        assert!(nodes.contains(&"Table"));
        assert!(nodes.contains(&"Column"));
        assert!(nodes.contains(&"CloudAction"));
        assert!(nodes.contains(&"Doc"));
        assert!(nodes.contains(&"DocSection"));
        assert_eq!(nodes.len(), 21);
        assert!(edges.contains(&"Calls"));
        assert!(edges.contains(&"Imports"));
        assert!(edges.contains(&"Produces"));
        assert!(edges.contains(&"Consumes"));
        assert!(edges.contains(&"Assumes"));
        assert!(edges.contains(&"Runs"));
        assert!(edges.contains(&"Routes"));
        assert!(edges.contains(&"Contains"));
        assert!(edges.contains(&"HasColumn"));
        assert!(edges.contains(&"ForeignKey"));
        assert!(edges.contains(&"Reads"));
        assert!(edges.contains(&"Writes"));
        assert!(edges.contains(&"MapsTo"));
        assert!(edges.contains(&"Grants"));
        assert!(edges.contains(&"RequiresPermission"));
        assert!(edges.contains(&"Documents"));
        assert!(edges.contains(&"Mentions"));
        assert_eq!(edges.len(), 21);
    }

    /// Guard against the advertised edge-kind vocabulary silently drifting from the
    /// `EdgeKind` enum (the bug that left `MapsTo` off the `strata://schema` resource
    /// after it was added). Every variant the graph can emit must appear in the
    /// published list — add it to `graph_schema_json` when you add a variant.
    #[test]
    fn graph_schema_advertises_every_edge_kind() {
        let advertised: Vec<String> = graph_schema_json()["edge_kinds"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_owned())
            .collect();
        // The full EdgeKind vocabulary. Adding a variant to `model.rs` without adding
        // it here (and to `graph_schema_json`) must fail this test.
        let all = [
            EdgeKind::Defines,
            EdgeKind::MemberOf,
            EdgeKind::Imports,
            EdgeKind::Calls,
            EdgeKind::Extends,
            EdgeKind::Implements,
            EdgeKind::Produces,
            EdgeKind::Consumes,
            EdgeKind::Assumes,
            EdgeKind::Runs,
            EdgeKind::Routes,
            EdgeKind::Contains,
            EdgeKind::HasColumn,
            EdgeKind::ForeignKey,
            EdgeKind::Reads,
            EdgeKind::Writes,
            EdgeKind::MapsTo,
            EdgeKind::Grants,
            EdgeKind::RequiresPermission,
            EdgeKind::Documents,
            EdgeKind::Mentions,
        ];
        for kind in all {
            let name = edge_kind_name(kind);
            assert!(
                advertised.contains(&name),
                "edge kind {name:?} missing from the strata://schema vocabulary"
            );
        }
        assert_eq!(
            advertised.len(),
            all.len(),
            "the advertised edge-kind list and the EdgeKind vocabulary must match exactly"
        );
    }

    // ── guidance (K6) ────────────────────────────────────────────────────────

    /// A DocSection [`Node`] with an explicit `path#anchor` fqn shape — `node()`
    /// conflates name==fqn, which does not fit a DocSection's naming, so
    /// guidance's own tests build these directly.
    fn doc_section(uid: &str, path: &str, anchor: &str, start_line: u32, end_line: u32) -> Node {
        Node {
            uid: Uid(uid.into()),
            kind: NodeKind::DocSection,
            name: format!("Section {anchor}"),
            fqn: format!("{path}#{anchor}"),
            path: path.into(),
            span: Span {
                start_line,
                start_col: 0,
                end_line,
                end_col: 0,
            },
            provenance: Provenance::Extracted,
            confidence: Confidence::new(1.0),
        }
    }

    fn doc_edge(src: &str, dst: &str, kind: EdgeKind, prov: Provenance, conf: f32) -> Edge {
        Edge {
            src: Uid(src.into()),
            dst: Uid(dst.into()),
            kind,
            provenance: prov,
            confidence: Confidence::new(conf),
        }
    }

    /// Write `content` at `<root>/<rel>`, creating parent directories.
    fn write_body_file(root: &std::path::Path, rel: &str, content: &str) {
        let full = root.join(rel);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(full, content).unwrap();
    }

    /// A LONG, realistic-scale repo-relative path — deliberately not a short
    /// synthetic one (C1 review): the truncation marker's cost is
    /// `27 (fixed text) + path.len() + anchor.len()` bytes, and a short path
    /// makes that overhead trivial enough to hide a leak. This one alone is
    /// ~74 bytes.
    fn long_src_path() -> String {
        "packages/very-long-service-directory-name-for-realism/src/handlers/lib.rs".to_string()
    }
    /// ~76 bytes, mirroring a real exported symbol's fully-qualified doc
    /// anchor rather than a two-word placeholder.
    fn long_doc_comment_anchor() -> String {
        "doc:aVeryLongAndDescriptiveExportedSymbolNameMirroringRealWorldConventions".to_string()
    }
    /// ~89 bytes.
    fn long_docs_path() -> String {
        "docs/architecture/decisions/very-long-adr-directory-name-for-testing-realism/guide.md"
            .to_string()
    }

    /// **The budget guardrail (C1/C2 review).** One doc comment (Documents) +
    /// three Mentions sections, each ~2000 bytes on disk (well over the
    /// 1200/section cap), all at LONG paths/anchors (~150-190 bytes of marker
    /// overhead PER section — see [`long_src_path`]/[`long_doc_comment_anchor`]/
    /// [`long_docs_path`]) — pins the exact contract the plan's Global
    /// Constraints table specifies: own doc comment first, default total
    /// budget holds EXACTLY (no marker slack — the old accounting would have
    /// overshot by roughly 4 × marker_len ≈ 700+ bytes here, landing well
    /// past 4800, matching the shape of the reviewer's real-repo measurement
    /// of 5,492B), fat sections truncate with the exact marker.
    fn fixture_with_fat_docs() -> (Graph, ToolCtx, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let src_path = long_src_path();
        let doc_anchor = long_doc_comment_anchor();
        let docs_path = long_docs_path();
        // The doc-comment section's body lives at `src_path` line 2; the
        // three Mentions sections' bodies live at `docs_path` lines 1-3 — one
        // line each, so `slice_body_lines` returns exactly that many bytes
        // (no `\n` join overhead).
        write_body_file(
            tmp.path(),
            &src_path,
            &format!("pub fn alphaOne() {{}}\n{}\n", "x".repeat(2000)),
        );
        write_body_file(
            tmp.path(),
            &docs_path,
            &format!(
                "{}\n{}\n{}\n",
                "a".repeat(2000),
                "b".repeat(2000),
                "c".repeat(2000)
            ),
        );

        let mut g = Graph::new();
        g.add_node(node_kind("alphaOne", "alphaOne", NodeKind::Function));
        let doc_comment_uid = format!("doc|kt|{src_path}|{src_path}#{doc_anchor}|");
        g.add_node(doc_section(&doc_comment_uid, &src_path, &doc_anchor, 2, 2));
        let m_high_uid = format!("doc|kt|{docs_path}|{docs_path}#m-high|");
        g.add_node(doc_section(&m_high_uid, &docs_path, "m-high", 1, 1));
        let m_mid_uid = format!("doc|kt|{docs_path}|{docs_path}#m-mid|");
        g.add_node(doc_section(&m_mid_uid, &docs_path, "m-mid", 2, 2));
        let m_low_uid = format!("doc|kt|{docs_path}|{docs_path}#m-low|");
        g.add_node(doc_section(&m_low_uid, &docs_path, "m-low", 3, 3));
        g.add_edge(doc_edge(
            &doc_comment_uid,
            "alphaOne",
            EdgeKind::Documents,
            Provenance::Extracted,
            0.95,
        ));
        g.add_edge(doc_edge(
            &m_high_uid,
            "alphaOne",
            EdgeKind::Mentions,
            Provenance::Inferred,
            0.80,
        ));
        g.add_edge(doc_edge(
            &m_mid_uid,
            "alphaOne",
            EdgeKind::Mentions,
            Provenance::Inferred,
            0.70,
        ));
        g.add_edge(doc_edge(
            &m_low_uid,
            "alphaOne",
            EdgeKind::Mentions,
            Provenance::Ambiguous,
            0.35,
        ));

        let ctx = ToolCtx {
            repo_root: Some(tmp.path().to_path_buf()),
            member_roots: Vec::new(),
        };
        (g, ctx, tmp)
    }

    #[test]
    fn guidance_orders_by_tier_and_respects_budget() {
        let (graph, ctx, _tmp) = fixture_with_fat_docs();
        let v = call_tool_ctx(&graph, &ctx, "guidance", &json!({"symbol": "alphaOne"})).unwrap();
        let sections = v["sections"].as_array().unwrap();
        assert_eq!(sections.len(), 4, "got {sections:?}");
        assert!(
            sections[0]["anchor"].as_str().unwrap().starts_with("doc:"),
            "own doc comment first: {sections:?}"
        );
        let total: usize = sections
            .iter()
            .map(|s| s["text"].as_str().unwrap().len())
            .sum();
        // C1: tightened to the EXACT covenant, no marker slack. Measured:
        // total == 4800 exactly (4 sections × 1200 bytes, content+marker
        // packed to fill each section's own cap). The four markers here are
        // 174/118/117/117 bytes (526 total) — under the OLD accounting
        // (marker appended ON TOP of a full 1200-byte content slice,
        // uncounted) this exact fixture would have produced 4800 + 526 =
        // 5326 bytes, the same order of magnitude as the reviewer's
        // real-repo measurement of 5,492B.
        assert!(
            total <= 4800,
            "default budget holds EXACTLY (no marker slack), got {total}"
        );
        assert!(
            sections.iter().any(|s| s["truncated"] == true),
            "fat sections truncate with a marker: {sections:?}"
        );
        assert!(sections
            .iter()
            .any(|s| s["text"].as_str().unwrap().contains("[truncated — fetch")));
        // budget_used now includes marker bytes for a cut section (C1) — it
        // must still never exceed the requested budget.
        assert!(v["budget_used"].as_u64().unwrap() <= 4800);
    }

    /// **C2 review.** `budget: 1` against sections whose bodies START with a
    /// multi-byte (CJK) character: the old code's `cap = min(1200,
    /// remaining)` (here, 1) landed inside the FIRST character's byte
    /// sequence, so `guidance_truncate`'s char-boundary backoff walked all
    /// the way down to an EMPTY slice — `slice.is_empty()` but `cut` was
    /// still `true` (0 < body.len()), so the OLD code appended a marker
    /// anyway: a "section" with a marker but ZERO content, `remaining`
    /// unchanged at 1 (nothing was ever subtracted for an empty slice), so
    /// EVERY subsequent candidate repeated the same free-marker "storm" (the
    /// reviewer measured 1,663B of pure markers on a real repo at
    /// `budget: 1`). The fix must instead degrade every one of these to a
    /// clean `ref_only` entry — tiny, honest, no markers, `budget_used: 0`.
    #[test]
    fn guidance_multibyte_body_at_tiny_budget_never_storms_free_markers() {
        let tmp = tempfile::tempdir().unwrap();
        // Emoji ("😀", 4 bytes/char in UTF-8) bodies — every char boundary is
        // a multiple of 4, so no cap in [1,3] can ever land on one.
        write_body_file(
            tmp.path(),
            "docs/g.md",
            &format!("{}\n{}\n", "😀".repeat(500), "😀".repeat(500)),
        );
        let mut g = Graph::new();
        g.add_node(node_kind("target", "target", NodeKind::Function));
        g.add_node(doc_section(
            "doc|r|docs/g.md|docs/g.md#one",
            "docs/g.md",
            "one",
            1,
            1,
        ));
        g.add_node(doc_section(
            "doc|r|docs/g.md|docs/g.md#two",
            "docs/g.md",
            "two",
            2,
            2,
        ));
        g.add_edge(doc_edge(
            "doc|r|docs/g.md|docs/g.md#one",
            "target",
            EdgeKind::Mentions,
            Provenance::Inferred,
            0.80,
        ));
        g.add_edge(doc_edge(
            "doc|r|docs/g.md|docs/g.md#two",
            "target",
            EdgeKind::Mentions,
            Provenance::Inferred,
            0.70,
        ));
        let ctx = ToolCtx {
            repo_root: Some(tmp.path().to_path_buf()),
            member_roots: Vec::new(),
        };
        let v = call_tool_ctx(
            &g,
            &ctx,
            "guidance",
            &json!({"symbol": "target", "budget": 1}),
        )
        .unwrap();
        let sections = v["sections"].as_array().unwrap();
        assert_eq!(sections.len(), 2, "got {sections:?}");
        assert_eq!(v["budget_used"], 0, "zero progress ⇒ zero spent: {v}");
        for s in sections {
            assert_eq!(s["ref_only"], true, "every entry is ref-only: {s:?}");
            assert_eq!(s["text"], "", "no free marker-only content: {s:?}");
            assert_eq!(s["truncated"], false, "never falsely marked cut: {s:?}");
            assert!(
                !s["text"].as_str().unwrap().contains("[truncated"),
                "NO markers at all — that was the storm: {s:?}"
            );
        }
        // The whole response stays tiny — no marker text for either section.
        let response_len = serde_json::to_string(&v).unwrap().len();
        assert!(
            response_len < 500,
            "tiny response, no marker storm: {response_len} bytes"
        );
    }

    #[test]
    fn guidance_orders_documents_before_mentions_by_confidence_desc() {
        // Short bodies (no truncation noise) so the ORDER itself is pinned
        // precisely: Documents (own doc comment) always first, then Mentions
        // sorted by confidence desc.
        let tmp = tempfile::tempdir().unwrap();
        write_body_file(tmp.path(), "src/a.ts", "short\n");
        write_body_file(tmp.path(), "docs/g.md", "short\nshort\nshort\n");
        let mut g = Graph::new();
        g.add_node(node_kind("target", "target", NodeKind::Function));
        g.add_node(doc_section(
            "doc|r|src/a.ts|src/a.ts#doc:target|",
            "src/a.ts",
            "doc:target",
            1,
            1,
        ));
        g.add_node(doc_section(
            "doc|r|docs/g.md|docs/g.md#hi",
            "docs/g.md",
            "hi",
            1,
            1,
        ));
        g.add_node(doc_section(
            "doc|r|docs/g.md|docs/g.md#mid",
            "docs/g.md",
            "mid",
            2,
            2,
        ));
        g.add_node(doc_section(
            "doc|r|docs/g.md|docs/g.md#lo",
            "docs/g.md",
            "lo",
            3,
            3,
        ));
        g.add_edge(doc_edge(
            "doc|r|src/a.ts|src/a.ts#doc:target|",
            "target",
            EdgeKind::Documents,
            Provenance::Extracted,
            0.95,
        ));
        g.add_edge(doc_edge(
            "doc|r|docs/g.md|docs/g.md#hi",
            "target",
            EdgeKind::Mentions,
            Provenance::Inferred,
            0.80,
        ));
        g.add_edge(doc_edge(
            "doc|r|docs/g.md|docs/g.md#mid",
            "target",
            EdgeKind::Mentions,
            Provenance::Inferred,
            0.70,
        ));
        g.add_edge(doc_edge(
            "doc|r|docs/g.md|docs/g.md#lo",
            "target",
            EdgeKind::Mentions,
            Provenance::Ambiguous,
            0.35,
        ));
        let ctx = ToolCtx {
            repo_root: Some(tmp.path().to_path_buf()),
            member_roots: Vec::new(),
        };
        let v = call_tool_ctx(&g, &ctx, "guidance", &json!({"symbol": "target"})).unwrap();
        let anchors: Vec<&str> = v["sections"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["anchor"].as_str().unwrap())
            .collect();
        assert_eq!(
            anchors,
            vec!["doc:target", "hi", "mid", "lo"],
            "{anchors:?}"
        );
    }

    #[test]
    fn guidance_file_mode_aggregates_across_the_files_symbols_deduping_by_max_confidence() {
        let tmp = tempfile::tempdir().unwrap();
        write_body_file(tmp.path(), "src/multi.ts", "one\ntwo\n");
        write_body_file(tmp.path(), "docs/g.md", "shared section body\n");
        let mut g = Graph::new();
        g.add_node(Node {
            path: "src/multi.ts".into(),
            ..node_kind("fnA", "fnA", NodeKind::Function)
        });
        g.add_node(Node {
            path: "src/multi.ts".into(),
            ..node_kind("fnB", "fnB", NodeKind::Function)
        });
        g.add_node(doc_section(
            "doc|r|docs/g.md|docs/g.md#shared",
            "docs/g.md",
            "shared",
            1,
            1,
        ));
        // The SAME section mentions both fnA (low conf) and fnB (high conf) —
        // the file-level aggregation must dedupe to ONE entry keeping the max.
        g.add_edge(doc_edge(
            "doc|r|docs/g.md|docs/g.md#shared",
            "fnA",
            EdgeKind::Mentions,
            Provenance::Inferred,
            0.70,
        ));
        g.add_edge(doc_edge(
            "doc|r|docs/g.md|docs/g.md#shared",
            "fnB",
            EdgeKind::Mentions,
            Provenance::Inferred,
            0.80,
        ));
        let ctx = ToolCtx {
            repo_root: Some(tmp.path().to_path_buf()),
            member_roots: Vec::new(),
        };
        let v = call_tool_ctx(&g, &ctx, "guidance", &json!({"file": "src/multi.ts"})).unwrap();
        let sections = v["sections"].as_array().unwrap();
        assert_eq!(sections.len(), 1, "deduped to one section: {sections:?}");
        assert!((sections[0]["confidence"].as_f64().unwrap() - 0.80).abs() < 1e-6);
        assert_eq!(v["target"]["name"], "src/multi.ts");
    }

    // ── C3 review: absolute-path matching ──────────────────────────────────

    #[test]
    fn node_in_file_matches_component_boundary_suffix_both_ways() {
        assert!(node_in_file("src/a.ts", "src/a.ts"), "exact match");
        assert!(
            node_in_file("src/a.ts", "/repo/src/a.ts"),
            "a stored relative path matches an absolute one ending in it"
        );
        assert!(
            node_in_file("/repo/src/a.ts", "src/a.ts"),
            "and the reverse direction"
        );
        assert!(
            !node_in_file("a.ts", "schema_a.ts"),
            "must be a path-COMPONENT boundary, not a bare suffix"
        );
        assert!(
            !node_in_file("", "src/a.ts"),
            "empty node_path matches nothing"
        );
    }

    #[test]
    fn guidance_file_mode_matches_an_absolute_path_the_same_as_relative() {
        // C3: the OLD exact `n.path == file` match meant an absolute `--file`
        // found nothing even though the SAME repo-relative node exists —
        // the PreToolUse hook always passes an absolute `tool_input.file_path`.
        let tmp = tempfile::tempdir().unwrap();
        write_body_file(tmp.path(), "src/a.ts", "one\n");
        write_body_file(tmp.path(), "docs/g.md", "mentions a.ts\n");
        let mut g = Graph::new();
        g.add_node(Node {
            path: "src/a.ts".into(),
            ..node_kind("target", "target", NodeKind::Function)
        });
        g.add_node(doc_section(
            "doc|r|docs/g.md|docs/g.md#h",
            "docs/g.md",
            "h",
            1,
            1,
        ));
        g.add_edge(doc_edge(
            "doc|r|docs/g.md|docs/g.md#h",
            "target",
            EdgeKind::Mentions,
            Provenance::Inferred,
            0.80,
        ));
        let ctx = ToolCtx {
            repo_root: Some(tmp.path().to_path_buf()),
            member_roots: Vec::new(),
        };
        let absolute = tmp.path().join("src/a.ts");
        let v = call_tool_ctx(
            &g,
            &ctx,
            "guidance",
            &json!({"file": absolute.to_str().unwrap()}),
        )
        .unwrap();
        let sections = v["sections"].as_array().unwrap();
        assert_eq!(
            sections.len(),
            1,
            "an absolute --file path must find the same section a relative one does: {v}"
        );
        assert_eq!(sections[0]["anchor"], "h");
    }

    #[test]
    fn guidance_section_arg_returns_one_full_body_with_no_budget_applied() {
        let tmp = tempfile::tempdir().unwrap();
        let fat = "y".repeat(3000); // well over the 1200 per-section cap
        write_body_file(tmp.path(), "docs/g.md", &format!("{fat}\n"));
        let mut g = Graph::new();
        g.add_node(node_kind("target", "target", NodeKind::Function));
        g.add_node(doc_section(
            "doc|r|docs/g.md|docs/g.md#full",
            "docs/g.md",
            "full",
            1,
            1,
        ));
        g.add_edge(doc_edge(
            "doc|r|docs/g.md|docs/g.md#full",
            "target",
            EdgeKind::Mentions,
            Provenance::Inferred,
            0.80,
        ));
        let ctx = ToolCtx {
            repo_root: Some(tmp.path().to_path_buf()),
            member_roots: Vec::new(),
        };
        let v = call_tool_ctx(
            &g,
            &ctx,
            "guidance",
            &json!({"symbol": "target", "section": "full"}),
        )
        .unwrap();
        let sections = v["sections"].as_array().unwrap();
        assert_eq!(sections.len(), 1);
        assert_eq!(
            sections[0]["text"].as_str().unwrap().len(),
            3000,
            "full, uncapped body"
        );
        assert_eq!(sections[0]["truncated"], false);
    }

    #[test]
    fn guidance_unknown_section_anchor_is_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let mut g = Graph::new();
        g.add_node(node_kind("target", "target", NodeKind::Function));
        let ctx = ToolCtx {
            repo_root: Some(tmp.path().to_path_buf()),
            member_roots: Vec::new(),
        };
        let err = call_tool_ctx(
            &g,
            &ctx,
            "guidance",
            &json!({"symbol": "target", "section": "nope"}),
        )
        .unwrap_err();
        assert!(matches!(err, ToolError::NotFound(_)), "{err:?}");
    }

    #[test]
    fn guidance_budget_arg_overrides_the_default() {
        let tmp = tempfile::tempdir().unwrap();
        write_body_file(
            tmp.path(),
            "docs/g.md",
            &format!("{}\n{}\n", "a".repeat(500), "b".repeat(500)),
        );
        let mut g = Graph::new();
        g.add_node(node_kind("target", "target", NodeKind::Function));
        g.add_node(doc_section(
            "doc|r|docs/g.md|docs/g.md#one",
            "docs/g.md",
            "one",
            1,
            1,
        ));
        g.add_node(doc_section(
            "doc|r|docs/g.md|docs/g.md#two",
            "docs/g.md",
            "two",
            2,
            2,
        ));
        g.add_edge(doc_edge(
            "doc|r|docs/g.md|docs/g.md#one",
            "target",
            EdgeKind::Mentions,
            Provenance::Inferred,
            0.80,
        ));
        g.add_edge(doc_edge(
            "doc|r|docs/g.md|docs/g.md#two",
            "target",
            EdgeKind::Mentions,
            Provenance::Inferred,
            0.70,
        ));
        let ctx = ToolCtx {
            repo_root: Some(tmp.path().to_path_buf()),
            member_roots: Vec::new(),
        };
        let v = call_tool_ctx(
            &g,
            &ctx,
            "guidance",
            &json!({"symbol": "target", "budget": 400}),
        )
        .unwrap();
        let sections = v["sections"].as_array().unwrap();
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0]["anchor"], "one");
        assert_eq!(sections[0]["truncated"], true);
        assert_eq!(sections[0]["ref_only"], false);
        // Budget exhausted by section 1 (400 bytes taken of a 400 budget) — the
        // second section is a ref_only entry, never invisible.
        assert_eq!(sections[1]["anchor"], "two");
        assert_eq!(sections[1]["ref_only"], true);
        assert_eq!(sections[1]["text"], "");
        assert_eq!(v["budget_used"], 400);
    }

    /// Minor (c) review: the error message for a non-numeric `budget` must
    /// name the exact expected shape, matching `depth`'s sibling message.
    #[test]
    fn guidance_non_numeric_budget_names_the_expected_shape() {
        let g = bar_calls_foo();
        let err =
            call_tool(&g, "guidance", &json!({"symbol": "foo", "budget": "lots"})).unwrap_err();
        match err {
            ToolError::BadArgs(msg) => assert_eq!(msg, "`budget` must be a non-negative integer"),
            other => panic!("expected BadArgs, got {other:?}"),
        }
    }

    /// Minor (d) review: estate-mode's honest degradation when NO configured
    /// member root's basename matches the section's own uid `package` field —
    /// `resolve_root_for_repo` must refuse to guess (never read a DIFFERENT
    /// repo's file under the same relative path), so the section degrades to
    /// "body unavailable" rather than risking a WRONG body. (Full estate
    /// root-name mapping — matching by manifest-declared name rather than
    /// directory basename — is a named follow-up, out of K6's scope; this
    /// test only pins that the CURRENT mismatch case degrades honestly.)
    #[test]
    fn guidance_estate_mode_basename_mismatch_degrades_to_body_unavailable_never_a_wrong_body() {
        let wrong_a = tempfile::tempdir().unwrap();
        let wrong_b = tempfile::tempdir().unwrap();
        // A file at the SAME relative path exists under a wrong root, with
        // DIFFERENT content — if resolve_root_for_repo ever guessed wrong,
        // this test would catch it reading the WRONG body, not just a
        // missing one.
        write_body_file(
            wrong_a.path(),
            "docs/g.md",
            "WRONG CONTENT — must never be returned\n",
        );
        let mut g = Graph::new();
        g.add_node(node_kind("target", "target", NodeKind::Function));
        g.add_node(doc_section(
            "doc|realrepo|docs/g.md|docs/g.md#h|",
            "docs/g.md",
            "h",
            1,
            1,
        ));
        g.add_edge(doc_edge(
            "doc|realrepo|docs/g.md|docs/g.md#h|",
            "target",
            EdgeKind::Mentions,
            Provenance::Inferred,
            0.80,
        ));
        let ctx = ToolCtx {
            repo_root: None,
            member_roots: vec![wrong_a.path().to_path_buf(), wrong_b.path().to_path_buf()],
        };
        let v = call_tool_ctx(&g, &ctx, "guidance", &json!({"symbol": "target"})).unwrap();
        let sections = v["sections"].as_array().unwrap();
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0]["text"], "", "never a wrong body: {v}");
        assert_eq!(sections[0]["note"], "body unavailable");
        assert!(
            !sections[0]["text"]
                .as_str()
                .unwrap()
                .contains("WRONG CONTENT"),
            "must never leak a different repo's file content: {v}"
        );
    }

    #[test]
    fn guidance_missing_file_on_disk_is_body_unavailable_never_an_error() {
        let tmp = tempfile::tempdir().unwrap(); // "docs/gone.md" never written.
        let mut g = Graph::new();
        g.add_node(node_kind("target", "target", NodeKind::Function));
        g.add_node(doc_section(
            "doc|r|docs/gone.md|docs/gone.md#h",
            "docs/gone.md",
            "h",
            1,
            1,
        ));
        g.add_edge(doc_edge(
            "doc|r|docs/gone.md|docs/gone.md#h",
            "target",
            EdgeKind::Mentions,
            Provenance::Inferred,
            0.80,
        ));
        let ctx = ToolCtx {
            repo_root: Some(tmp.path().to_path_buf()),
            member_roots: Vec::new(),
        };
        let v = call_tool_ctx(&g, &ctx, "guidance", &json!({"symbol": "target"})).unwrap();
        let sections = v["sections"].as_array().unwrap();
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0]["text"], "");
        assert_eq!(sections[0]["truncated"], false);
        assert_eq!(
            sections[0]["ref_only"], false,
            "attempted, not budget-skipped"
        );
        assert_eq!(sections[0]["note"], "body unavailable");
        // A file-level problem, not a bare-ctx problem — no top-level note.
        assert!(v.get("note").is_none());
    }

    #[test]
    fn guidance_with_no_repo_root_at_all_degrades_to_refs_with_a_top_level_note() {
        let mut g = Graph::new();
        g.add_node(node_kind("target", "target", NodeKind::Function));
        g.add_node(doc_section(
            "doc|r|docs/g.md|docs/g.md#h",
            "docs/g.md",
            "h",
            1,
            1,
        ));
        g.add_edge(doc_edge(
            "doc|r|docs/g.md|docs/g.md#h",
            "target",
            EdgeKind::Mentions,
            Provenance::Inferred,
            0.80,
        ));
        // Bare ctx: no repo_root, no member_roots — "bare-db" per the design's
        // self-review bar.
        let v = call_tool(&g, "guidance", &json!({"symbol": "target"})).unwrap();
        let sections = v["sections"].as_array().unwrap();
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0]["text"], "");
        assert_eq!(sections[0]["note"], "body unavailable");
        assert_eq!(
            v["note"], "no repo root configured — bodies unavailable (refs only)",
            "a bare ctx must explain itself at the top level too: {v}"
        );
    }

    #[test]
    fn guidance_no_docs_found_is_an_honest_empty_result_not_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let mut g = Graph::new();
        g.add_node(node_kind("lonely", "lonely", NodeKind::Function));
        let ctx = ToolCtx {
            repo_root: Some(tmp.path().to_path_buf()),
            member_roots: Vec::new(),
        };
        let v = call_tool_ctx(&g, &ctx, "guidance", &json!({"symbol": "lonely"})).unwrap();
        assert!(v["sections"].as_array().unwrap().is_empty());
        assert_eq!(v["note"], "no documentation found");
    }

    #[test]
    fn guidance_requires_exactly_one_of_symbol_or_file() {
        let g = bar_calls_foo();
        let neither = call_tool(&g, "guidance", &json!({})).unwrap_err();
        assert!(matches!(neither, ToolError::BadArgs(_)));
        let both =
            call_tool(&g, "guidance", &json!({"symbol": "foo", "file": "foo.ts"})).unwrap_err();
        assert!(matches!(both, ToolError::BadArgs(_)));
    }

    #[test]
    fn guidance_unknown_symbol_is_not_found() {
        let g = bar_calls_foo();
        let err = call_tool(&g, "guidance", &json!({"symbol": "vanished"})).unwrap_err();
        assert!(matches!(err, ToolError::NotFound(_)), "{err:?}");
    }

    #[test]
    fn guidance_ambiguous_symbol_returns_candidates_not_an_error() {
        let mut g = Graph::new();
        g.add_node(Node {
            fqn: "a.dup".into(),
            ..node_kind("u1", "dup", NodeKind::Function)
        });
        g.add_node(Node {
            fqn: "b.dup".into(),
            ..node_kind("u2", "dup", NodeKind::Function)
        });
        let v = call_tool(&g, "guidance", &json!({"symbol": "dup"})).unwrap();
        assert_eq!(v["ambiguous"], true);
        assert_eq!(v["candidates"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn guidance_contract_operation_gets_the_live_description_as_the_first_section() {
        let tmp = tempfile::tempdir().unwrap();
        write_body_file(
            tmp.path(),
            "openapi.json",
            r#"{
  "openapi": "3.0.0",
  "paths": {
    "/users": {
      "get": {
        "operationId": "getUser",
        "summary": "Fetch a user by id."
      }
    }
  }
}"#,
        );
        write_body_file(tmp.path(), "docs/g.md", "a mention\n");

        let mut g = Graph::new();
        let op_uid = "contract|repo|openapi.json|getUser|";
        g.add_node(Node {
            uid: Uid(op_uid.into()),
            kind: NodeKind::ApiOperation,
            name: "getUser".into(),
            fqn: "getUser".into(),
            path: "/users".into(),
            span: Span::default(),
            provenance: Provenance::Extracted,
            confidence: Confidence::new(1.0),
        });
        g.add_node(doc_section(
            "doc|r|docs/g.md|docs/g.md#h",
            "docs/g.md",
            "h",
            1,
            1,
        ));
        g.add_edge(doc_edge(
            "doc|r|docs/g.md|docs/g.md#h",
            op_uid,
            EdgeKind::Mentions,
            Provenance::Inferred,
            0.95, // deliberately HIGHER than the description's fixed 1.0 tier
                  // never matters — tier always wins over confidence.
        ));
        let ctx = ToolCtx {
            repo_root: Some(tmp.path().to_path_buf()),
            member_roots: Vec::new(),
        };
        let v = call_tool_ctx(&g, &ctx, "guidance", &json!({"symbol": "getUser"})).unwrap();
        let sections = v["sections"].as_array().unwrap();
        assert_eq!(sections.len(), 2, "got {sections:?}");
        assert_eq!(sections[0]["anchor"], "description");
        assert_eq!(sections[0]["text"], "Fetch a user by id.");
        assert_eq!(sections[0]["provenance"], "Extracted");
        assert_eq!(sections[1]["anchor"], "h");
    }

    #[test]
    fn guidance_reachable_via_the_ctx_less_call_tool_entry_point_too() {
        // Unlike detect_changes/rename, guidance never HARD errors without a
        // repo root — it degrades honestly (see the bare-ctx test above). This
        // pins that it is dispatched at all through the ctx-less `call_tool`.
        let g = bar_calls_foo();
        let v = call_tool(&g, "guidance", &json!({"symbol": "foo"})).unwrap();
        assert!(v.get("sections").is_some());
    }
}
