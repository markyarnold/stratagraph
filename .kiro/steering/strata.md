<!-- strata:begin -->
# StrataGraph: Cross-Plane Code Intelligence

This repo is indexed by StrataGraph as **this repo** (3331 nodes, 26884 edges; planes present: code/contract/infra). The MCP tools below let you understand the code, assess blast radius across planes, and navigate safely.

## Always Do (MUST)

- **MUST act on the pre-edit blast radius the hook injects.** Before each file edit, a PreToolUse hook computes that file's blast radius and injects it as context (the same report as `strata blast <file>`). It is authoritative at edit time: read it, report the affected dependents and risk, and follow the rules below. Never edit past it without acting on what it shows.
- **MUST run `impact` on a symbol/field/operation BEFORE modifying it**, and report the blast radius to the user before proceeding: list the direct (d=1) and indirect (d=2) dependents with each one's `will_break` verdict (WILL BREAK only when `confidence ≥ 0.40` AND not `ambiguous`; depth does NOT decide it), its confidence, and a risk level (LOW / MEDIUM / HIGH / CRITICAL), then wait for direction.
- **MUST run `detect_changes` before committing.** It is the mechanical pre-commit check: it git-diffs your work, derives the changed symbols PER PLANE (code / contract / infra), aggregates the blast radius over the whole graph, and returns a risk level with reasons. Read its risk and affected set, report them, and pause for direction on HIGH/CRITICAL. Do NOT hand-run `impact` symbol-by-symbol when `detect_changes` does it across every plane in one call.
- **MUST check every plane the target touches.** A GraphQL field / API operation → `context` and read its `producers` (who implements it) and `consumers` (who queries it) buckets. A Lambda / handler / module → its `produces` / `consumes`. An ordinary exported symbol → `impact` for upstream dependents.
- **MUST warn and pause for direction** when the blast radius is HIGH or CRITICAL, when it crosses a repo boundary (estate), or when it touches contract surface consumed by another plane.
- **MUST treat confidence bands as trust policy:** ≥ 0.90 → act on it; 0.40–0.89 → verify in the source before relying on it; < 0.40 or `ambiguous: true` → treat as UNKNOWN and **say so explicitly; never present uncertain impact as certain.**
- **MUST flag likely-dead contract surface:** a field/operation with **0 producers AND 0 consumers** is probably dead, so call it out rather than treating it as live.

## Never Do

- **NEVER edit a schema/contract file** (GraphQL SDL, API definition) without first running `impact` (or `context`) on the affected operations and reporting who produces and consumes them.
- **NEVER rename a symbol with find-and-replace.** Run `impact` first, then update exactly the d=1 set the graph reports; grep-and-replace silently corrupts cross-file and cross-plane references.
- **NEVER ignore a HIGH or CRITICAL risk result**, and never proceed past one without explicit user direction.
- **NEVER claim "nothing depends on this" from grep alone.** The graph carries contract and infra links that grep cannot see (a Lambda producing a field; a frontend consuming an operation). When the graph is your evidence, say so.

## Tools (MCP)

- **`impact`** `{ symbol, depth?, min_confidence?, include_contracts?, include_infra? }`: reverse blast radius (everything that depends on `symbol`). Contract- and infra-aware by default: it follows producer → operation → consumer across the contract plane (so cross-plane and cross-repo consumers appear) and Assumes/Routes/Runs across the infra plane (so an IamRole reaches the Lambdas that assume it). Pass `include_contracts: false` and/or `include_infra: false` for a narrower radius.
- **`explain`** `{ symbol, affected, depth?, min_confidence?, include_contracts?, include_infra? }`: WHY is `affected` in `symbol`'s blast radius? Returns the evidence chain: each edge's kind/provenance/confidence and the running (accumulated) confidence that produces impact's number, or an honest `reachable: false` when it is not in the radius. The same toggles as `impact`, so the explained confidence matches the impact row.
- **`context`** `{ symbol }`: the 360° view of one symbol: `callers`, `callees`, `imports_in`/`imports_out`, `members`, `container`, and the contract buckets `producers` / `consumers` / `produces` / `consumes`.
- **`query`** `{ text }`: case-insensitive lexical search over name / fully-qualified name / path. Use it to find the exact symbol before `impact`/`context`.
- **`detect_changes`** `{ staged? }`: the pre-commit check: git-diffs the working tree (or the staged index) vs HEAD, derives the changed symbols per plane (code / contract / infra), aggregates the blast radius over the graph, and returns `{ files, symbols, affected, risk }` with risk reasons. Use it before committing instead of running `impact` per changed symbol.

> **Auto-reload (read this):** the MCP server now hot-reloads. When the on-disk index changes (the PostToolUse `strata index` hook, or a manual reindex) it swaps in the fresh graph before the next request, no session/server restart needed. The reload is degrade-safe: a reindex caught mid-write keeps the previous graph and retries, so a tool call never blocks or serves a half-loaded graph. It keys off `.strata/index.stamp`, falling back to the `graph.duckdb` mtime for indexes written before this feature. (Estate `--workspace` reloads the same way on a manifest or per-repo change.)

## Workflow hooks (Kiro)

Three lifecycle hooks enforce this protocol automatically:
- **strata-pre-edit**: before any file write, confirms you ran `impact` on every symbol/field about to change.
- **strata-pre-commit**: before a command that creates a git commit, runs `detect_changes` for the per-plane changed symbols, blast radius, and risk. It applies ONLY to commit commands — any other command (including strata's own `detect-changes`/`index` runs) proceeds untouched, so the hook can never loop on its own remediation.
- **strata-post-edit**: after a file edit, re-runs `strata index .` to keep the on-disk graph fresh (the MCP server hot-reloads it).

When in doubt: `query` to find the symbol → `context` for its plane buckets → `impact` before you change it → `detect_changes` before you commit.
<!-- strata:end -->
