//! Transport-independent MCP tool dispatch.
//!
//! [`call_tool`] maps a `(graph, tool name, args)` triple to a JSON result
//! payload — no IO, no MCP framing. This is the part that must be correct, and
//! it is exercised directly by unit tests without any live MCP client.

use std::path::PathBuf;

use serde_json::{json, Value};
use strata_core::{
    context, explain, impact, query, EdgeKind, Explanation, Graph, ImpactOptions, Node, NodeKind,
};
use strata_index::{blast_for_file, detect_changes, rename, ChangeScope, RenameOptions};

use crate::resolve::{resolve_symbol, ResolveOutcome};

/// The ambient context a tool call may need beyond the loaded graph.
///
/// Most tools (`context`/`impact`/`query`) are pure functions of the graph and
/// ignore this entirely. The filesystem-touching tools (`detect_changes`,
/// `rename`) need the repository root for git/IO; it lives here so the dispatch
/// signature stays uniform. [`Default`] is `repo_root: None` — the ctx-less
/// [`call_tool`] path, which makes those tools return a clear "needs a repo
/// root" error rather than guessing.
#[derive(Debug, Clone, Default)]
pub struct ToolCtx {
    /// The repository working directory, when the server knows it (derived from
    /// the `--db` path or an explicit `--repo`). `None` over the ctx-less path.
    pub repo_root: Option<PathBuf>,
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
/// ignore the ctx), and `detect_changes`/`rename` (need `ctx.repo_root`). Any
/// other name is [`ToolError::BadArgs`].
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
            json!({
                "uid": a.uid.as_str(),
                "name": a.name,
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

// ── schemas ─────────────────────────────────────────────────────────────────────

/// The 7 tools' MCP `tools/list` descriptors (name + description + inputSchema).
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
    fn tool_schemas_lists_the_seven_object_schemas() {
        let schemas = tool_schemas();
        let arr = schemas.as_array().unwrap();
        assert_eq!(arr.len(), 7);
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
                "rename"
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
        assert_eq!(nodes.len(), 19);
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
        assert_eq!(edges.len(), 19);
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
}
