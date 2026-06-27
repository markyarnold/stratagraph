# What breaks if I change this?

**Goal:** before you touch a symbol (a function, a method, a class, a table column, a GraphQL field), find every dependent that could break, how certain StrataGraph is about each one, and an overall risk level. This is the core StrataGraph workflow; the rest of the guides are variations on it.

The tools are `impact` (reverse blast radius of a *symbol*) and `blast` (the same, for every symbol a *file* defines). Both report; they never gate.

## Steps

### 1. Find the exact symbol

`impact` takes a name. If you only know roughly what it's called, resolve it first with `query`:

```console
$ strata query classify_risk
1 match(es) for "classify_risk":
  classify_risk [Function] crates/strata-index/src/changes.rs
    rust|strata|crates/strata-index/src/changes.rs|classify_risk|
```

The third line is the symbol's **uid**, its stable identity. Keep it; you'll need it if the name turns out to be ambiguous (below).

### 2. Run impact

```console
$ strata impact cmd_impact
Impact of cmd_impact (crates/strata-cli/src/lib.rs) — 7 affected:
  depth  conf  amb  verdict     name (path)
      1  0.95   no  WILL BREAK  cmd_impact_ambiguous_lists_candidates_with_uid_hint (crates/strata-cli/src/lib.rs)
      1  0.95   no  WILL BREAK  cmd_impact_dead_table_keeps_bare_message (crates/strata-cli/src/lib.rs)
      1  0.95   no  WILL BREAK  cmd_impact_member_bearing_target_hints_at_member_dependents (crates/strata-cli/src/lib.rs)
      1  0.95   no  WILL BREAK  cmd_impact_truly_dead_container_keeps_bare_message (crates/strata-cli/src/lib.rs)
      1  0.95   no  WILL BREAK  cmd_impact_unknown_uid_is_symbol_not_found (crates/strata-cli/src/lib.rs)
      1  0.95   no  WILL BREAK  cmd_impact_with_uid_resolves_the_exact_node (crates/strata-cli/src/lib.rs)
      1  0.80   no  WILL BREAK  main (crates/strata-cli/src/main.rs)
```

Each row is one dependent. Read the columns left to right; they're the whole point.

## How to read the result

### `depth` is distance, not danger

`depth` is how many hops the dependent sits from your symbol: `1` calls it directly, `2` calls something that calls it, and so on (default max depth is 5, change it with `--depth`). Depth tells you how far the ripple travels; it does **not** decide whether a row breaks.

### `verdict` is per row: WILL BREAK vs may affect

The `verdict` column is the call that matters. A row is **WILL BREAK** only when its `conf` is at or above `0.40` **and** it is not ambiguous (`amb` is `no`). Everything else is **may affect**. This rule is independent of depth: a `d=1` direct dependent that is ambiguous or below 0.40 is `may affect`, never a certain break.

You can see both halves of that rule in one result:

```console
$ strata impact reload --uid 'rust|strata|crates/strata-cli/src/reload.rs|SingleDbReloader::reload|'
Impact of reload (crates/strata-cli/src/reload.rs) — 12 affected:
  depth  conf  amb  verdict     name (path)
      1  0.35  yes  may affect  single_db_reloader_degrades_safely_on_corrupt_db (crates/strata-cli/tests/hot_reload.rs)
      1  0.35  yes  may affect  single_db_reloader_picks_up_an_external_reindex (crates/strata-cli/tests/hot_reload.rs)
      2  0.33  yes  may affect  serve_stdio_reloadable (crates/strata-mcp/src/server.rs)
      ...
```

Every row here is `d=1` or `d=2` yet every verdict is `may affect`: these are instance-method calls (`x.reload()`) that StrataGraph resolved heuristically, so the paths are flagged ambiguous and capped below 0.40. Reporting them as certain breakage would be wrong: they are surfaced, but as "review this," not "this breaks."

### `conf` is a trust dial

`conf` is the accumulated confidence of the best path by which the dependent reaches your symbol. Apply the same band policy everywhere in StrataGraph:

- **≥ 0.90**: act on it.
- **0.40–0.89**: verify in the source before you rely on it.
- **< 0.40 or `amb` = yes**: treat as UNKNOWN; say so explicitly, never present it as certain.

See [Confidence and provenance](../concepts/confidence.md) for where these numbers come from.

### Assign a risk level and report it

Before editing, turn the table into one risk word and tell the user:

| Signal | Risk |
|---|---|
| < 5 affected | LOW |
| 5–15 affected | MEDIUM |
| > 15 affected, or many flows | HIGH |
| Reaches contract surface, or crosses a repo boundary | CRITICAL |

CRITICAL is the mechanical signal the engine emits: `detect_changes` / `blast` raise it on a changed-or-affected contract symbol or a cross-repo reach (see [the rubric in `mcp.md`](../reference/mcp.md#blast)). Reaching a sensitive area like auth or payments is *your* judgment to layer on top: treat it as CRITICAL when you assess it that way, but it is not part of the tool's rule.

If the result is HIGH or CRITICAL, or it crosses a repo boundary, stop and get direction before you change anything.

## Zero direct dependents is not "dead"

Run `impact` on a class, struct, enum, interface, or table and you may see zero affected, because `impact` walks *incoming* edges, while a type's methods and a table's columns hang off *outgoing* ones. StrataGraph catches this and tells you when the type is not actually dead:

```console
$ strata impact Graph
Impact of Graph (crates/strata-core/src/graph.rs) — 0 affected:
  0 dependents on Graph itself; 8 of its members have dependents: add_edge, add_node, edge_count, get_node, neighbors, … (+3 more)
  try: strata impact add_edge
```

`Graph` itself has no direct dependents, but eight of its methods do. The fix is exactly what the hint says: run `impact` on a member:

```console
$ strata impact add_edge
Impact of add_edge (crates/strata-core/src/graph.rs) — N affected:
  ...
```

If a member-bearing type truly has nothing (no direct dependents and no members with dependents) you get the bare line instead, and *that* is a real "nothing depends on this." Never read the misleading zero as dead without checking the members hint first.

## When the name is ambiguous

If a name resolves to several nodes, `impact` will not guess. It lists the candidates and stops:

```console
$ strata impact reload
error: ambiguous symbol reload: 4 candidates — pick one:
  rust|strata|crates/strata-cli/src/reload.rs|SingleDbReloader::reload|  [Method]  reload  (crates/strata-cli/src/reload.rs)
  rust|strata|crates/strata-cli/src/reload.rs|WorkspaceReloader::reload|  [Method]  reload  (crates/strata-cli/src/reload.rs)
  rust|strata|crates/strata-mcp/src/server.rs|GraphReloader::reload|  [Method]  reload  (crates/strata-mcp/src/server.rs)
  rust|strata|crates/strata-mcp/src/server.rs|tests::ScriptedReloader::reload|  [Method]  reload  (crates/strata-mcp/src/server.rs)
re-run with --uid <uid> (or a fully-qualified name) to disambiguate
```

Pick the candidate you mean and re-run with its uid:

```console
$ strata impact reload --uid 'rust|strata|crates/strata-cli/src/reload.rs|SingleDbReloader::reload|'
```

An ambiguous symbol is a signpost, not a dead end: the candidate list always carries the `--uid` you need.

## Why is *this* dependent in the radius?

When a single row surprises you, ask `explain` for the evidence chain between the two symbols:

```console
$ strata explain cmd_impact cmd_impact_dead_table_keeps_bare_message
Why cmd_impact affects cmd_impact_dead_table_keeps_bare_message (conf 0.95, WILL BREAK):
  cmd_impact  —CALLS (Extracted 0.95)→  cmd_impact_dead_table_keeps_bare_message    running 0.95
```

Each hop shows its edge kind, its provenance band, its confidence, and the running confidence that produces the number `impact` reported. If the affected symbol is not actually reachable, `explain` says so (`reachable: false`) rather than inventing a path. (Use the same `--no-contracts` / `--no-infra` toggles you ran `impact` with, so the explained confidence matches the row.)

## Blast a whole file at once

Editing a file? `blast` runs the same analysis over every symbol the file defines and aggregates one risk verdict for the edit:

```console
$ strata blast crates/strata-index/src/changes.rs
Editing crates/strata-index/src/changes.rs touches 61 symbol(s); blast radius 1332 affected — HIGH
  symbols (61):
    - aggregate_impact [Function]
    - classify_risk [Function]
    - detect_changes [Function]
    ...
  depth  conf  amb  verdict     name (path)
      ...
Risk: HIGH — 1332 affected
```

`blast` is what powers the pre-edit hook for AI agents (see [Pre-edit blast checks](pre-edit-blast.md)); the `--format agent` flag prints the token-lean version that hook injects.

## Narrowing the radius

`impact` follows the contract and infra planes by default, so cross-plane and cross-repo dependents show up automatically. To see a code-only radius (useful for telling apart "breaks in this codebase" from "breaks across a plane boundary") turn the planes off:

```console
$ strata impact getUser --no-contracts --no-infra
```

If the dependent count drops, those dependents were reaching your symbol *through* a contract or infra link that grep could never see. That difference is the subject of [Cross-boundary impact](../concepts/cross-boundary.md) and the [plane walkthroughs](plane-walkthroughs.md).

Other useful flags: `--depth N` (how far to walk) and `--min-confidence X` (drop paths below `X` entirely, rather than surfacing-and-flagging them). The full surface is in the [CLI reference](../reference/cli.md); the same operations are available to agents as MCP tools (see the [MCP reference](../reference/mcp.md)).

## What to do next

- Report d=1 and d=2 dependents, their confidence, and a risk level **before** you edit.
- Pause for direction on HIGH/CRITICAL or anything cross-repo.
- Changing a schema field or endpoint? Read [Is this schema field dead?](dead-surface.md) and check producers and consumers first.
- Renaming? Use [`strata rename`](rename.md), never find-and-replace.
- About to commit? Run [`detect-changes`](detect-changes.md) for the whole-diff version of this check.
