# StrataGraph

**Cross-boundary code intelligence you can trust.** StrataGraph builds a knowledge graph of your codebase that crosses the boundaries other tools stop at: code, API contracts (GraphQL/OpenAPI), and cloud infrastructure (SAM/CloudFormation). So "what breaks if I change this?" gets a *complete* answer: the Lambda that implements a field, the frontend that queries it, and everything in between, across every repo in your estate.

Built for **reliable accuracy**: every edge carries provenance and a calibrated confidence band, ambiguity is reported as ambiguity, and the linker never invents a relationship it can't defend. Accuracy reports with CI-enforced floors live in [`docs/accuracy/`](docs/accuracy/).

## Quickstart

```bash
# 1. Build (Rust toolchain required)
cargo build --release            # → target/release/strata

# 2. Index your repo
strata index .                   # writes .strata/graph.duckdb

# 3. Ask questions
strata query "getPolicy"                 # find symbols/fields/resources
strata context "Query.getPolicyStats"    # 360° view: producers, consumers, callers…
strata impact "Query.getPolicyStats"     # blast radius across code, contract, and infra planes
```

### Multi-repo estates

Declare your repos once, then query the linked whole; impact crosses repo boundaries through shared contracts:

```toml
# strata.workspace.toml
name = "my-estate"
[[repos]]
name = "backend"
path = "../backend"
[[repos]]
name = "frontend"
path = "../frontend"
```

```bash
strata index --workspace strata.workspace.toml
strata impact "Query.getUser" --workspace strata.workspace.toml
```

## Wire it into your coding agent (one command)

```bash
strata init claude    # Claude Code: MCP server, steering rules, skills, scoped hooks
strata init kiro      # Kiro: MCP server, steering, lifecycle hooks
```

This installs a strictly-governed kit: your agent **must assess blast radius before modifying anything**, checks every plane the target touches (a schema field's producers *and* consumers, not just callers), treats confidence bands as trust policy (≥0.9 act / 0.4–0.8 verify / <0.4 say "unknown"; never present uncertain impact as certain), and flags dead contract surface. Installs are idempotent and merge-safe: existing CLAUDE.md content, MCP servers, and hooks (including a GitNexus setup) are preserved byte-for-byte. Hooks are project-scoped and silent-when-clean, with no nagging in repos that don't use StrataGraph.

## Desktop app

A Tauri-based GUI for search, 360° context, impact tables, and a WebGL graph view (plane-colored nodes, confidence-weighted edges, blast-radius tinting):

```bash
cd apps/strata-desktop && npm run tauri build
open ../../target/release/bundle/macos/strata-desktop.app   # then "Open Project Folder…"
```

## MCP server

```bash
strata mcp --db .strata/graph.duckdb              # single repo
strata mcp --workspace strata.workspace.toml      # linked estate
```

Tools: `query`, `context` (with `producers`/`consumers`/`produces`/`consumes` buckets), `impact` (contract-aware by default; `include_contracts:false` for code-only). The server serves the graph loaded at startup; the agent-kit hooks keep the on-disk index fresh as you edit.

## Design

The full design doc (provenance/confidence bands, the plane model, accuracy methodology, roadmap) is at [`docs/strata-design.md`](docs/strata-design.md).

## Status

Active development. Merged: code plane for TypeScript/JavaScript (Tree-sitter + SCIP), Python, Rust, and C#; OpenAPI + GraphQL + gRPC contract planes with cross-repo estates; AWS infrastructure plane (SAM/CFN/Terraform → AppSync → contract); SQL data plane; desktop GUI; agent integration (Claude Code + Kiro); and the `detect-changes`, `rename`, and `blast` tools. In progress: IAM permission-gap detection. See the design doc's roadmap.

## License

StrataGraph is **source available** under the [Functional Source License v1.1, Apache 2.0 Future License (FSL-1.1-ALv2)](LICENSE.md). The whole suite is here: the `strata` binary and all its tools, the MCP server, the desktop app, the agent kit, and multi-repo estates (`--workspace`). You may read, run, modify, self-host and redistribute it for any purpose other than competing with us, and **two years after each release that release converts to Apache 2.0**. Roadmap capabilities (org-scale hosted estates, history, collaboration, governance) will land here under the same terms.

A managed/hosted service may be offered commercially in future, for teams that would rather not self-host. The license keeps that option open while the source stays readable and free for any non-competing use. See [docs/commercialisation/](docs/commercialisation/README.md).

"StrataGraph" and the logo are trademarks; forks may not use them to identify themselves (see [TRADEMARK.md](TRADEMARK.md)). Contributions are welcome under the DCO (see [CONTRIBUTING.md](CONTRIBUTING.md)).
