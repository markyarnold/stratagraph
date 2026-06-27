//! Minimal newline-delimited JSON-RPC 2.0 server over stdio implementing the
//! MCP lifecycle for the strata tool surface.
//!
//! This is intentionally hand-rolled rather than built on the official `rmcp`
//! SDK: `rmcp` is async/tokio-based, and the strata surface is a small, fully
//! synchronous request→response loop. The transport here is thin plumbing —
//! all the real work lives in [`crate::call_tool`], which is unit-tested
//! independently. The protocol wiring is itself testable via the synchronous
//! [`handle_message`] helper (no process/threads required).

use std::io::{BufRead, Write};

use serde_json::{json, Value};
use strata_core::Graph;

use crate::tools::{call_tool_ctx, graph_schema_json, tool_schemas, ToolCtx, ToolError};

/// The MCP protocol revision this server speaks.
pub const PROTOCOL_VERSION: &str = "2024-11-05";

/// The `strata://schema` resource URI.
const SCHEMA_URI: &str = "strata://schema";

/// Run the MCP server over stdio until the client closes stdin.
///
/// Reads newline-delimited JSON-RPC requests from stdin and writes
/// newline-delimited JSON-RPC responses to stdout. Notifications (no `id`) are
/// processed but produce no response, per JSON-RPC.
///
/// This is the ctx-less convenience over [`serve_stdio_with_ctx`] (a default,
/// empty [`ToolCtx`]), so the filesystem tools report a clear "needs a repo
/// root" error; the CLI's `mcp` command uses `serve_stdio_with_ctx` with the
/// derived repo root.
pub fn serve_stdio(graph: Graph) -> std::io::Result<()> {
    serve_stdio_with_ctx(graph, ToolCtx::default())
}

/// [`serve_stdio`] with an ambient [`ToolCtx`] (the repo root) threaded into
/// every `tools/call`, so the filesystem-touching tools (`detect_changes`) can
/// reach the working tree.
pub fn serve_stdio_with_ctx(graph: Graph, ctx: ToolCtx) -> std::io::Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let request: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                // Parse error: respond with a JSON-RPC parse error (id unknown → null).
                let resp = error_response(Value::Null, -32700, &format!("parse error: {e}"));
                write_line(&mut out, &resp)?;
                continue;
            }
        };

        if let Some(response) = handle_message_with_ctx(&graph, &ctx, &request) {
            write_line(&mut out, &response)?;
        }
    }
    Ok(())
}

/// A source the reloadable server can pull a fresh [`Graph`] from when the
/// on-disk index changes underneath it (Track E3, hot-reload).
///
/// The MCP server is a pure reader and its request loop is strictly serial, so
/// no locking is needed: the staleness check and the graph swap both happen
/// *between* requests, on the loop thread. This trait keeps `strata-mcp`
/// load-agnostic — the CLI supplies the concrete implementations (single-db and
/// estate) so this crate never depends on the indexer/store load internals.
///
/// Contract:
/// * [`changed`](GraphReloader::changed) is a **cheap** per-request check (a
///   `stat`/small read of a sidecar signal); it MUST NOT open the graph db.
/// * [`reload`](GraphReloader::reload) returns the new graph, or `Err` if the
///   reload could not complete (a missing, locked, or corrupt db). On `Err` the
///   server keeps serving the graph it already has and does **not** advance the
///   change signal, so the next `changed()` still reports a change and the
///   reload is retried. A reload is therefore all-or-nothing — a half-loaded
///   graph is never served.
pub trait GraphReloader {
    /// Has the on-disk index changed since the last *successful* load? Cheap;
    /// must not open the db.
    fn changed(&mut self) -> bool;
    /// Reload the graph from disk. `Err` ⇒ keep the current graph (degrade-safe).
    fn reload(&mut self) -> Result<Graph, String>;
}

/// One reloadable iteration: if the reloader reports a change, try to reload and
/// swap `graph` in place (on success) before handling the request; on a failed
/// reload, log and keep the current `graph`. Then dispatch the request exactly
/// as the static path does.
///
/// Factored out of the stdio loop so the swap-on-`Ok` / retain-on-`Err`
/// behaviour is unit-testable with a [`GraphReloader`] double and a plain
/// JSON-RPC `Value` — no real stdin or threads.
fn refresh_then_handle(
    graph: &mut Graph,
    reloader: &mut impl GraphReloader,
    ctx: &ToolCtx,
    request: &Value,
) -> Option<Value> {
    if reloader.changed() {
        match reloader.reload() {
            Ok(g) => {
                eprintln!(
                    "[mcp] reloaded graph: {} nodes / {} edges",
                    g.node_count(),
                    g.edge_count()
                );
                *graph = g;
            }
            Err(err) => eprintln!(
                "[mcp] reindex detected but reload failed (serving previous graph): {err}"
            ),
        }
    }
    handle_message_with_ctx(graph, ctx, request)
}

/// [`serve_stdio_with_ctx`], but the served graph **hot-reloads**: before each
/// request the server asks `reloader` whether the on-disk index changed and, if
/// so, swaps in the freshly-loaded graph (degrade-safe — a failed reload keeps
/// the previous graph; see [`GraphReloader`]). The loop is otherwise identical to
/// the static server, so the request/response behaviour is byte-for-byte the
/// same between reloads.
pub fn serve_stdio_reloadable(
    initial: Graph,
    mut reloader: impl GraphReloader,
    ctx: ToolCtx,
) -> std::io::Result<()> {
    let mut graph = initial;
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let request: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                let resp = error_response(Value::Null, -32700, &format!("parse error: {e}"));
                write_line(&mut out, &resp)?;
                continue;
            }
        };

        if let Some(response) = refresh_then_handle(&mut graph, &mut reloader, &ctx, &request) {
            write_line(&mut out, &response)?;
        }
    }
    Ok(())
}

fn write_line(out: &mut impl Write, value: &Value) -> std::io::Result<()> {
    let s = serde_json::to_string(value).expect("serialize JSON-RPC response");
    out.write_all(s.as_bytes())?;
    out.write_all(b"\n")?;
    out.flush()
}

/// Handle one JSON-RPC message against `graph`.
///
/// Returns `Some(response)` for requests (those with an `id`), or `None` for
/// notifications and for messages that warrant no reply. This synchronous shape
/// is what the integration tests drive directly.
///
/// Ctx-less convenience over [`handle_message_with_ctx`] (a default, empty
/// [`ToolCtx`]).
pub fn handle_message(graph: &Graph, request: &Value) -> Option<Value> {
    handle_message_with_ctx(graph, &ToolCtx::default(), request)
}

/// [`handle_message`] with an ambient [`ToolCtx`] threaded into `tools/call` so
/// the filesystem-touching tools can reach the repo root.
pub fn handle_message_with_ctx(graph: &Graph, ctx: &ToolCtx, request: &Value) -> Option<Value> {
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");
    let id = request.get("id").cloned();
    let params = request.get("params").cloned().unwrap_or(Value::Null);

    // Notifications have no `id` → never produce a response.
    let is_notification = id.is_none();

    match method {
        "initialize" => Some(success_response(id?, initialize_result())),
        "ping" => Some(success_response(id?, json!({}))),
        "tools/list" => Some(success_response(id?, json!({ "tools": tool_schemas() }))),
        "tools/call" => Some(tools_call(graph, ctx, id?, &params)),
        "resources/list" => Some(success_response(id?, resources_list())),
        "resources/read" => Some(resources_read(id?, &params)),
        // Lifecycle / unknown notifications (e.g. notifications/initialized): ignore.
        _ if is_notification => None,
        // Unknown *request* method.
        _ => Some(error_response(
            id?,
            -32601,
            &format!("method not found: {method}"),
        )),
    }
}

fn initialize_result() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": {
            "tools": {},
            "resources": {}
        },
        "serverInfo": {
            "name": "strata-mcp",
            "version": env!("CARGO_PKG_VERSION")
        }
    })
}

fn tools_call(graph: &Graph, ctx: &ToolCtx, id: Value, params: &Value) -> Value {
    let name = match params.get("name").and_then(Value::as_str) {
        Some(n) => n,
        None => return error_response(id, -32602, "missing tool `name`"),
    };
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));

    match call_tool_ctx(graph, ctx, name, &args) {
        Ok(result) => success_response(id, tool_content(&result, false)),
        Err(err) => {
            // Tool-level failure: a *successful* JSON-RPC response carrying an
            // error tool result (isError=true), per the MCP tools spec.
            let payload = json!({ "error": err.to_string(), "code": tool_error_code(&err) });
            success_response(id, tool_content(&payload, true))
        }
    }
}

/// Wrap a tool result value as MCP tool content (a text block of the JSON).
fn tool_content(result: &Value, is_error: bool) -> Value {
    let text = serde_json::to_string(result).unwrap_or_else(|_| "{}".into());
    json!({
        "content": [ { "type": "text", "text": text } ],
        "isError": is_error
    })
}

fn tool_error_code(err: &ToolError) -> &'static str {
    match err {
        ToolError::NotFound(_) => "not_found",
        ToolError::Ambiguous(_, _) => "ambiguous",
        ToolError::BadArgs(_) => "bad_args",
    }
}

fn resources_list() -> Value {
    json!({
        "resources": [
            {
                "uri": SCHEMA_URI,
                "name": "Strata graph schema",
                "description": "Node-kind and edge-kind vocabularies of the code graph.",
                "mimeType": "application/json"
            }
        ]
    })
}

fn resources_read(id: Value, params: &Value) -> Value {
    let uri = params.get("uri").and_then(Value::as_str).unwrap_or("");
    if uri != SCHEMA_URI {
        return error_response(id, -32602, &format!("unknown resource: {uri}"));
    }
    let text = serde_json::to_string(&graph_schema_json()).unwrap_or_else(|_| "{}".into());
    success_response(
        id,
        json!({
            "contents": [
                {
                    "uri": SCHEMA_URI,
                    "mimeType": "application/json",
                    "text": text
                }
            ]
        }),
    )
}

// ── JSON-RPC envelope helpers ────────────────────────────────────────────────────

fn success_response(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

#[cfg(test)]
mod tests {
    use super::*;
    use strata_core::{Confidence, Edge, EdgeKind, Node, NodeKind, Provenance, Span, Uid};

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

    fn bar_calls_foo() -> Graph {
        let mut g = Graph::new();
        g.add_node(node("foo", "foo"));
        g.add_node(node("bar", "bar"));
        g.add_edge(Edge {
            src: Uid("bar".into()),
            dst: Uid("foo".into()),
            kind: EdgeKind::Calls,
            provenance: Provenance::Inferred,
            confidence: Confidence::new(0.9),
        });
        g
    }

    fn req(id: i64, method: &str, params: Value) -> Value {
        json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
    }

    #[test]
    fn initialize_advertises_tools_capability() {
        let g = Graph::new();
        let resp = handle_message(&g, &req(1, "initialize", json!({}))).unwrap();
        assert_eq!(resp["id"], 1);
        assert_eq!(resp["result"]["protocolVersion"], PROTOCOL_VERSION);
        assert!(resp["result"]["capabilities"]["tools"].is_object());
        assert_eq!(resp["result"]["serverInfo"]["name"], "strata-mcp");
    }

    #[test]
    fn notifications_initialized_yields_no_response() {
        let g = Graph::new();
        let note = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
        assert!(handle_message(&g, &note).is_none());
    }

    #[test]
    fn tools_list_returns_the_seven_tools() {
        let g = Graph::new();
        let resp = handle_message(&g, &req(2, "tools/list", json!({}))).unwrap();
        let tools = resp["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 7);
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
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
    }

    #[test]
    fn tools_call_impact_returns_affected_payload() {
        let g = bar_calls_foo();
        let resp = handle_message(
            &g,
            &req(
                3,
                "tools/call",
                json!({ "name": "impact", "arguments": { "symbol": "foo" } }),
            ),
        )
        .unwrap();
        assert_eq!(resp["result"]["isError"], false);
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        let payload: Value = serde_json::from_str(text).unwrap();
        let names: Vec<&str> = payload["affected"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["name"].as_str().unwrap())
            .collect();
        assert!(
            names.contains(&"bar"),
            "impact via MCP must include bar; got {names:?}"
        );
    }

    #[test]
    fn tools_call_unknown_symbol_sets_is_error() {
        let g = bar_calls_foo();
        let resp = handle_message(
            &g,
            &req(
                4,
                "tools/call",
                json!({ "name": "impact", "arguments": { "symbol": "zzz" } }),
            ),
        )
        .unwrap();
        // JSON-RPC succeeds, but the tool result is flagged as an error.
        assert!(resp["result"]["isError"].as_bool().unwrap());
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("not found"));
    }

    #[test]
    fn resources_list_then_read_schema() {
        let g = Graph::new();
        let list = handle_message(&g, &req(5, "resources/list", json!({}))).unwrap();
        let resources = list["result"]["resources"].as_array().unwrap();
        assert_eq!(resources[0]["uri"], SCHEMA_URI);

        let read =
            handle_message(&g, &req(6, "resources/read", json!({ "uri": SCHEMA_URI }))).unwrap();
        let text = read["result"]["contents"][0]["text"].as_str().unwrap();
        let schema: Value = serde_json::from_str(text).unwrap();
        assert!(schema["node_kinds"].is_array());
        assert!(schema["edge_kinds"].is_array());
    }

    #[test]
    fn unknown_request_method_is_method_not_found() {
        let g = Graph::new();
        let resp = handle_message(&g, &req(7, "no/such/method", json!({}))).unwrap();
        assert_eq!(resp["error"]["code"], -32601);
    }

    // ── hot-reload seam (Track E3) ────────────────────────────────────────────

    /// A scripted [`GraphReloader`] test double: `changed()` returns the next
    /// value from `changed_script` (then `false` forever); `reload()` returns the
    /// next value from `reload_script`. It records how many times each was called
    /// so a test can prove the seam does not over-poll.
    struct ScriptedReloader {
        changed_script: std::collections::VecDeque<bool>,
        reload_script: std::collections::VecDeque<Result<Graph, String>>,
        changed_calls: usize,
        reload_calls: usize,
    }

    impl ScriptedReloader {
        fn new(changed: Vec<bool>, reload: Vec<Result<Graph, String>>) -> Self {
            ScriptedReloader {
                changed_script: changed.into(),
                reload_script: reload.into(),
                changed_calls: 0,
                reload_calls: 0,
            }
        }
    }

    impl GraphReloader for ScriptedReloader {
        fn changed(&mut self) -> bool {
            self.changed_calls += 1;
            self.changed_script.pop_front().unwrap_or(false)
        }
        fn reload(&mut self) -> Result<Graph, String> {
            self.reload_calls += 1;
            self.reload_script
                .pop_front()
                .unwrap_or_else(|| Err("script exhausted".into()))
        }
    }

    /// A second, distinguishable graph: `baz` (absent from `bar_calls_foo`).
    fn just_baz() -> Graph {
        let mut g = Graph::new();
        g.add_node(node("baz", "baz"));
        g
    }

    /// Drives a tools/call for `impact { symbol }` through the refresh-then-handle
    /// seam and returns the affected names (empty on a tool error).
    fn impact_names(resp: &Value) -> Vec<String> {
        let text = resp["result"]["content"][0]["text"].as_str().unwrap_or("");
        let payload: Value = serde_json::from_str(text).unwrap_or(Value::Null);
        payload["affected"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x["name"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn refresh_then_handle_swaps_graph_on_ok() {
        // Start serving the empty graph; the reloader reports a change once and
        // reloads into `bar_calls_foo`. The very next request must see the NEW
        // graph (impact foo → bar).
        let mut graph = Graph::new();
        let ctx = ToolCtx::default();
        let mut reloader = ScriptedReloader::new(vec![true], vec![Ok(bar_calls_foo())]);

        let req = req(
            1,
            "tools/call",
            json!({ "name": "impact", "arguments": { "symbol": "foo" } }),
        );
        let resp = refresh_then_handle(&mut graph, &mut reloader, &ctx, &req).unwrap();
        assert!(
            impact_names(&resp).contains(&"bar".to_string()),
            "after a successful reload the swapped-in graph must be served; got {resp:?}"
        );
        assert_eq!(reloader.reload_calls, 1, "reload happens exactly once");
    }

    #[test]
    fn refresh_then_handle_retains_graph_on_err() {
        // Serve `bar_calls_foo`; the reloader reports a change but reload FAILS.
        // The seam must keep serving the OLD graph (impact foo → bar still works)
        // and NOT advance — i.e. a subsequent changed()==true retries reload.
        let mut graph = bar_calls_foo();
        let ctx = ToolCtx::default();
        let mut reloader = ScriptedReloader::new(
            vec![true, true],
            vec![Err("locked db".into()), Ok(just_baz())],
        );

        let req_foo = req(
            1,
            "tools/call",
            json!({ "name": "impact", "arguments": { "symbol": "foo" } }),
        );
        let resp1 = refresh_then_handle(&mut graph, &mut reloader, &ctx, &req_foo).unwrap();
        assert!(
            impact_names(&resp1).contains(&"bar".to_string()),
            "a failed reload must retain the previously-served graph; got {resp1:?}"
        );

        // Next request: changed()==true again, reload now succeeds → swap to baz.
        // Proves the failed attempt did not advance the signal (it retried).
        let req_baz = req(
            2,
            "tools/call",
            json!({ "name": "context", "arguments": { "symbol": "baz" } }),
        );
        let resp2 = refresh_then_handle(&mut graph, &mut reloader, &ctx, &req_baz).unwrap();
        assert_eq!(
            resp2["result"]["isError"], false,
            "after the retry succeeds, the new graph (with baz) is served; got {resp2:?}"
        );
        assert_eq!(reloader.reload_calls, 2, "reload retried after the failure");
    }

    #[test]
    fn refresh_then_handle_no_change_does_not_reload() {
        // changed()==false ⇒ reload() must never be called; the served graph and
        // the request handling are exactly as the static path.
        let mut graph = bar_calls_foo();
        let ctx = ToolCtx::default();
        let mut reloader = ScriptedReloader::new(vec![false], vec![]);

        let req = req(
            1,
            "tools/call",
            json!({ "name": "impact", "arguments": { "symbol": "foo" } }),
        );
        let resp = refresh_then_handle(&mut graph, &mut reloader, &ctx, &req).unwrap();
        assert!(impact_names(&resp).contains(&"bar".to_string()));
        assert_eq!(reloader.changed_calls, 1, "changed() is checked once");
        assert_eq!(reloader.reload_calls, 0, "no reload when nothing changed");
    }
}
