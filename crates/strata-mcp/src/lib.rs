//! strata-mcp: the MCP tool surface for the strata code graph.
//!
//! Two layers, deliberately separated so the logic is testable without a live
//! client:
//!
//! * [`call_tool`] / [`call_tool_ctx`] — transport-independent dispatch
//!   (`graph + name + args → JSON`) for the `context`, `impact`, `query`, and
//!   `detect_changes` tools, plus [`tool_schemas`] / [`graph_schema_json`]. This
//!   is the correctness-critical core. [`call_tool`] is the ctx-less convenience
//!   over [`call_tool_ctx`] with a default [`ToolCtx`].
//! * [`serve_stdio`] / [`handle_message`] — a minimal hand-rolled JSON-RPC 2.0
//!   server over stdio implementing the MCP lifecycle, which is thin plumbing on
//!   top of `call_tool_ctx`. The `*_with_ctx` variants thread a [`ToolCtx`]
//!   (the repo root) for the filesystem-touching tools.
//!
//! Symbol resolution ([`resolve_symbol`]) lives here (not in `strata-core`) and
//! is reused by `strata-cli`.

mod resolve;
mod server;
mod tools;

pub use resolve::{resolve_symbol, ResolveOutcome};
pub use server::{
    handle_message, handle_message_with_ctx, serve_stdio, serve_stdio_reloadable,
    serve_stdio_with_ctx, GraphReloader, PROTOCOL_VERSION,
};
pub use tools::{call_tool, call_tool_ctx, graph_schema_json, tool_schemas, ToolCtx, ToolError};
