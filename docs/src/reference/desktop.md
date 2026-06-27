# Desktop app

The reference for the StrataGraph desktop app (`strata-desktop`): the Tauri commands
its backend exposes, what each does, the views, and how to build and run it.
Grounded in `apps/strata-desktop/src-tauri/src/{lib,commands,subgraph}.rs` and the
UI in `apps/strata-desktop/ui/src/`.

The app opens a code graph into in-memory state and answers `query` / `context` /
`impact` over it through the **same** `strata_mcp::call_tool` dispatch the MCP
server and CLI use (so it can never give a different answer), plus a
desktop-specific `subgraph` feed for the graph renderer.

## Tauri commands

The backend registers five `#[tauri::command]`s (`lib.rs`,
`tauri::generate_handler!`). Each thin command wraps a plain, testable function in
`commands.rs`/`subgraph.rs`; every fallible path returns `Err(String)`.

| Command | Args | Returns | Purpose |
|---|---|---|---|
| `open` | `path: String` | `OpenInfo` | Open a graph file or estate manifest, replacing any loaded graph. |
| `reindex` |  | `OpenInfo` | Rebuild the currently-loaded graph the way the CLI's `index` does, then swap it in. |
| `index_path` | `path: String` | `OpenInfo` | Index a folder that has no index yet (the open-project dead-end), then open it. |
| `tool` | `name: String`, `args: Value` | `Value` | Run a `query` / `context` / `impact` tool over the loaded graph via `call_tool`. |
| `subgraph` | `uid: String`, `depth: u32`, `kinds: Option<Vec<String>>`, `planes: Option<Vec<String>>` | `SubgraphDto` | A bounded both-directions neighbourhood of a node for the renderer. |

### open

`open_path` routes by input (`commands.rs`):

1. **Directory**, in priority order: `<dir>/strata.workspace.toml` → open as an
   estate; else `<dir>/.strata/graph.duckdb` → open that DB (descriptor is the
   *directory*, so a reindex re-runs `index_repo`); else an actionable error
   carrying the `NO_INDEX::` marker prefix (the UI keys its **Index Now**
   affordance off this prefix structurally).
2. **File**: a `.toml` is opened as an estate manifest; anything else as a
   DuckDB graph file.

A non-existent DB path is surfaced as an error rather than silently creating an
empty database. Opening an estate with one broken repo still succeeds: the bad
repo is recorded `ok: false` and the others load.

`OpenInfo` carries `source` (a human description with a `db:`/`workspace:`
qualifier), `engine` (the `strata_core::ENGINE_ID` build id, shown so a stale app
is identifiable), `nodes`, `edges`, and `repos` (per-repo `{ name, ok, error? }`;
one synthetic entry for a single DB file, one per manifest repo for an estate).

### reindex / index_path

`reindex_source` rebuilds exactly what was opened, mirroring the CLI's
`index`/`index_workspace` (`ResolveMode::Auto`, no install):

- **Repo(dir)** → index `dir` into `dir/.strata/graph.duckdb`, then reload.
- **Estate(manifest)** → re-index each repo, continuing past per-repo failures;
  the per-repo ok/err shows in the reloaded `OpenInfo.repos`.
- **GraphFile(f)** → if `f` is a conventional `<root>/.strata/graph.duckdb`,
  reindex as that repo root; otherwise an actionable error (open the project
  folder instead).

Concurrency: indexing is single-flight: a second concurrent `reindex` /
`index_path` is rejected with `Indexing is already running.` (an RAII guard
released on every exit path), and the std mutex is never held across the blocking
work (the index runs on a blocking thread via `spawn_blocking`). The old graph
stays queryable until an atomic swap at the very end. `reindex` with nothing
loaded returns `Open a project first.`.

### tool

`run_tool` delegates to `strata_mcp::call_tool` and maps `ToolError` to
`Err(String)`. The graph-only tools available this way are `query`, `context`,
and `impact` (their payload shapes are documented in [MCP → Tools](mcp.md#tools)).

### subgraph

`compute_subgraph` (`subgraph.rs`) returns a `SubgraphDto`:

```
SubgraphDto { nodes: [SubgraphNode], edges: [SubgraphEdge], truncated: bool }
SubgraphNode { uid, name, kind, path, plane }
SubgraphEdge { src, dst, kind, provenance, confidence }
```

Behaviour:

- BFS expands edges in **both** directions (callers *and* callees, producers
  *and* consumers around the focus node).
- `depth` is clamped server-side to `MAX_DEPTH` (`3`); a client cannot request an
  unbounded walk.
- The node set is capped at `MAX_NODES` (`500`); hitting the cap stops the walk
  and sets `truncated`.
- `kinds` is an optional edge-kind filter (serde names, e.g. `"Calls"`,
  `"Produces"`) restricting which edges are followed and returned.
- `planes` is an optional plane filter (`"code"` / `"contract"` / `"infra"` /
  `"data"`) restricting which nodes are admitted and traversed.
- Each node's `plane` is derived server-side by `plane_of`: the renderer colours
  by `plane` and never re-derives it from `kind`. An unknown `uid` is an error,
  not an empty graph.

## Views

The UI (`ui/src/main.ts`) is a search-driven, three-tab inspector:

- **Search** → `query()` → a results list. Selecting a result drives the panels.
- **Context tab**: the selected node's callers/callees/imports/members and its
  contract/infra/data buckets.
- **Impact tab**: runs `impact` with depth, min-confidence, `include_contracts`,
  and `include_infra` controls (`impact-depth`, `impact-minconf`,
  `impact-contracts`, `impact-infra`).
- **Graph tab**: the WebGL graph renderer (`ui/src/graphview/`), with two modes:
  `neighborhood` (the `subgraph` feed) and `impact`. Shows a truncation indicator
  when the node cap is hit.

A folder opened with no index dead-ends to an **Index Now** banner (keyed off the
`NO_INDEX::` marker), which calls `index_path`. A **reindex** button rebuilds the
loaded graph; both show a busy state while indexing is in flight.

## Build and run

The app is a Tauri project: a Rust backend (`src-tauri/`) and a TypeScript UI
(`ui/`). The library entry point `run()` in `lib.rs` builds the Tauri app
(registering the `tauri_plugin_dialog` and the five commands) and is invoked by
`main.rs`. Use the standard Tauri toolchain for your platform to build and run it.
