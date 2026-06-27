# The desktop app

StrataGraph ships an optional desktop application (built with
[Tauri](https://tauri.app)) that puts a graph view and point-and-click
query / context / impact panels over the same engine the CLI and MCP server use.
It is a convenience layer, not a separate product; it shares the one dispatch
path, so it can never give you a different answer than `strata` on the command
line.

## Status: pre-release

Set expectations before you build it:

- It is **pre-release** and **macOS-focused**: that is where it is developed and
  run.
- Builds are **unsigned**. On macOS you may have to allow it past Gatekeeper the
  first time (right-click → Open, or approve it in System Settings → Privacy &
  Security).
- The CLI and MCP server are the supported, daily-driven surfaces. Reach for the
  desktop app when a visual graph helps; reach for the CLI when you want
  reproducibility.

## Build and run

The desktop app lives under `apps/strata-desktop/` and has its own Node/Tauri
toolchain (a JavaScript runtime plus the Tauri prerequisites for your platform).
From that directory:

```bash
cd apps/strata-desktop
npm install        # also installs the UI workspace (postinstall)
npm run dev        # launch in development mode
```

To produce a bundled application instead:

```bash
npm run build      # tauri build → a packaged app
```

`npm run dev` opens the app with live-reload, which is the quickest way to try it.

## Open a repository or an estate

From the app you open one of:

- **A project directory**: the app treats it the way the CLI would, resolving to
  `<dir>/.strata/graph.duckdb`. If that directory has **no index yet**, the app
  surfaces an **Index Now** affordance rather than failing; it can build the
  graph for you in place.
- **A graph file**: a `.strata/graph.duckdb` (or any DuckDB graph file) opened
  directly.
- **An estate**: a `strata.workspace.toml` workspace manifest, which loads the
  whole multi-repo estate. Per-repo load outcomes are shown, and a single broken
  repo is reported rather than crashing the load. (See
  [Multi-repo estates](estates.md).)

## What you get

- A **graph view** of nodes and their relationships across planes.
- **Query / context / impact panels**: the same operations as the CLI: search
  for a symbol, inspect its 360° context (callers, callees, producers, consumers,
  members), and see its blast radius with confidence and the WILL BREAK verdict.
- **In-app reindex**: rebuild the open graph from inside the app. It rebuilds
  exactly what you opened: a repo is re-indexed as that repo, and an estate
  re-links every repo (continuing past a broken one).
- The **engine id** in the footer, so you can confirm which engine produced the
  graph you are looking at: the same id `strata --version` prints.

## More

For the full desktop feature set and details, see the
[desktop reference](../reference/desktop.md).
