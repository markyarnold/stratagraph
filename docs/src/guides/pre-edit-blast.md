# Pre-edit blast checks for AI agents

**Goal:** make an AI coding agent *automatically* see a file's blast radius before it edits (and act on it) without the agent having to remember to ask. This guide is for agent setups (Claude Code, Kiro). It explains the pre-edit blast hook the [agent kit](../reference/agent-kit.md) installs, what the agent should do with what it injects, and the standing steering rules that make impact analysis a default rather than a thing you hope the model does.

If you just want to run the blast check yourself from the terminal, that's [`strata blast`](impact.md#blast-a-whole-file-at-once); this page is about wiring it into the edit loop.

## What the hook does

`strata init claude` installs a **PreToolUse** hook scoped to the `Edit`, `Write`, and `MultiEdit` tools. Before any of those runs, the hook:

1. Reads the target file path from the tool call.
2. Runs `strata blast <file> --format agent`. It passes no `--db`, so it auto-resolves the repo's context: the radius is estate-wide when the repo is an enrolled estate member (`strata index --workspace`), single-repo otherwise.
3. Injects the result as `additionalContext` on the tool call, so the blast radius lands in the model's context *right before* it makes the edit.

The injected block is the `agent` format, the same content as [`strata blast`](impact.md#blast-a-whole-file-at-once), trimmed for tokens:

```text
StrataGraph pre-edit blast — crates/strata-cli/src/reload.rs: 14 symbol(s) defined here, 1329 dependent(s) in the blast radius. Risk HIGH.
  symbols: <module> [Module], DbSignal [Class], SingleDbReloader [Class], ...
  top dependents (depth/conf/verdict):
    - changed (crates/strata-cli/src/reload.rs) d=1 conf=0.95 WILL BREAK
    - reload (crates/strata-cli/src/reload.rs) d=1 conf=0.95 AMBIGUOUS may affect
    - cmd_mcp_workspace (crates/strata-cli/src/lib.rs) d=1 conf=0.80 AMBIGUOUS may affect
    … and 1321 more
  risk reasons: 1329 affected
Before editing, run `impact`/`context` on the symbols above and report the blast radius. Treat confidence < 0.40 or `ambiguous` as UNKNOWN — never present it as certain. PAUSE for direction if risk is HIGH/CRITICAL, crosses a repo boundary, or touches contract surface.
```

## It is non-blocking and degrade-safe

The hook is built to be invisible until it has something useful to say, and to never get in the way:

- **Non-blocking.** It only emits `additionalContext` and always `exit 0`. It never returns a permission decision, so it can never halt or loop an edit. The agent stays in control; the hook just informs it.
- **Silent when there's nothing to say.** No `.strata/` directory (repo not indexed)? Exit 0, no output, no noise in a project that hasn't opted in. No file path in the tool call, an empty blast, or any `strata` error? Exit 0 silently. A failed blast check never blocks an edit.
- **Degrade-safe with a fallback.** If `jq` isn't installed (the hook uses it to read the file path and build the JSON), it still injects a **static advisory** ("run impact/context on the symbols in this file before editing, treat low confidence as UNKNOWN, pause on HIGH/CRITICAL") so the steering reminder rides along even without the computed numbers.

A companion **PostToolUse** hook re-runs `strata index` in the background after each edit, so the graph stays fresh for the next pre-edit check, and a **SessionStart** reminder nudges you to index if the repo isn't yet. Together they keep the loop self-maintaining.

## What the agent should do with it

The injected block is **authoritative at edit time**: it's the live blast radius for the exact file about to change. The agent should treat it as a checkpoint, not decoration:

1. **Read it and report it.** Surface the affected dependents and the risk to the user before applying the edit; don't edit past it silently.
2. **Apply the band policy per dependent.** `conf ≥ 0.90` → trust it; `0.40–0.89` → verify in source; `< 0.40` or `AMBIGUOUS` → UNKNOWN, say so. Never present an ambiguous or low-confidence row as a certain break. (See [Confidence and provenance](../concepts/confidence.md).)
3. **Pause on HIGH / CRITICAL, or anything cross-repo / cross-plane.** Get explicit direction before proceeding.
4. **Drill in when needed.** For a surprising dependent, run [`impact`](impact.md) or `explain` on the specific symbol; the injected block is the summary, the tools give the detail.

## The standing steering rules

The hook is the *mechanical* half. The kit also writes a steering block into `CLAUDE.md` (or `.kiro/steering/`), the *prose* half, that holds the agent to a small set of rules even when no hook fired. The load-bearing ones:

- **Run `impact` before modifying a symbol**, and report the blast radius (direct and indirect dependents, each one's verdict and confidence, an overall risk) before proceeding.
- **Run `detect_changes` before committing**: the per-plane, whole-diff check (see [Pre-commit change checks](detect-changes.md)).
- **Treat confidence bands as trust policy**: the ≥0.90 / 0.40–0.89 / <0.40 rule above.
- **Flag likely-dead contract surface**: 0 producers and 0 consumers (see [Is this schema field dead?](dead-surface.md)).
- **Never edit a schema/contract file** without first running `impact`/`context` on the affected operations.
- **Never rename with find-and-replace**: use [`strata rename`](rename.md).
- **Never claim "nothing depends on this" from grep alone.** The graph carries contract and infra links grep cannot see; when the graph is your evidence, say so.

These are the never-confident-wrong rules in operational form. The hook makes step one happen automatically; the steering makes the rest a default.

## A note on the auto-reload

The MCP server hot-reloads the graph when the on-disk index changes: the PostToolUse reindex, or a manual `strata index`, is picked up before the next tool call, no session restart. The swap is degrade-safe: a reindex caught mid-write keeps the previous graph and retries, so a tool call never blocks or serves a half-loaded graph. In practice the agent's `impact`/`context`/`blast` answers track the code as it edits it, within one reindex.

## Set it up

```console
$ strata init claude     # installs the MCP server, steering block, skills, and the hooks
$ strata index .         # build the graph the hooks read (init can do this for you)
```

Run `strata init` with no agent to list supported agents. Full details (exactly what files are written, how the managed blocks merge, the Kiro variant) are in [The agent kit](../reference/agent-kit.md) and [Wire up your editor](../getting-started/agent-kit.md).

## What to do next

- Understand the verdict/confidence columns the block reports: [What breaks if I change this?](impact.md).
- The commit-time companion check: [Pre-commit change checks](detect-changes.md).
- Why the graph sees what grep can't (the reason the "never claim nothing depends" rule exists): [Cross-boundary impact](../concepts/cross-boundary.md).
