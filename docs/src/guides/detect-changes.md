# Pre-commit change checks

**Goal:** right before you commit, get one mechanical answer to "what does this diff put at risk?", across every plane, in a single command. `detect-changes` git-diffs your work, derives the changed symbols per plane (code / contract / infra), aggregates the blast radius over the whole graph, and returns a risk level with reasons.

It is the pre-commit version of [`impact`](impact.md): instead of you running `impact` symbol-by-symbol, `detect-changes` does it for the entire diff at once. Like every reporting tool in StrataGraph it never gates: it always exits 0, so you decide what to do with the verdict.

## Steps

### 1. Run it before committing

With unstaged or uncommitted work in the tree:

```console
$ strata detect-changes
Changes (working scope)
  1 changed file(s):
    M  crates/strata-cli/src/lib.rs
  code symbols (1):
    modified  cmd_impact  (crates/strata-cli/src/lib.rs)
  affected (7):
    depth  conf  amb  verdict     name (path)
        1  0.95   no  WILL BREAK  cmd_impact_ambiguous_lists_candidates_with_uid_hint (crates/strata-cli/src/lib.rs)
        1  0.95   no  WILL BREAK  cmd_impact_dead_table_keeps_bare_message (crates/strata-cli/src/lib.rs)
        ...
        1  0.80   no  WILL BREAK  main (crates/strata-cli/src/main.rs)
Risk: MEDIUM — 7 affected
```

### 2. Or check exactly what's staged

If you stage changes selectively, check the index rather than the working tree:

```console
$ strata detect-changes --staged
```

`--staged` diffs `git diff --cached HEAD`, the precise set you're about to commit. Use it as the last step before `git commit`.

## How to read the result

### Changed symbols, per plane

`detect-changes` doesn't just list changed *files*; it resolves them to changed *symbols*, grouped by the plane each one lives in:

- **code symbols**: functions, methods, classes you added / modified / removed.
- **contract symbols**: GraphQL fields, API operations whose signature changed.
- **infra symbols**: Lambdas, roles, resolvers, data sources that changed.

Each is tagged `added`, `modified`, or `removed`. A `modified` contract or infra symbol is the loud one: it means the diff changed shared surface other planes depend on.

### The aggregated blast radius

The `affected` block is the union of every changed symbol's blast radius, de-duplicated, with each dependent's `depth`, `conf`, `amb`, and per-row `verdict`, read exactly as in the [impact guide](impact.md). The verdict is re-derived after aggregation: a dependent is **WILL BREAK** only when its best path is `conf ≥ 0.40` and not ambiguous, regardless of depth.

### The risk level

The footer rolls the whole diff into one word:

| Risk | Roughly means |
|---|---|
| LOW | < 5 affected: small, contained. |
| MEDIUM | 5–15 affected. |
| HIGH | > 15 affected, or many flows fanning out. |
| CRITICAL | touches contract surface, or crosses a repo boundary. |

A diff that only **adds** a new symbol is LOW by construction, because nothing depends on something that didn't exist yet:

```console
$ strata detect-changes
Changes (working scope)
  1 changed file(s):
    M  crates/strata-index/src/changes.rs
  code symbols (1):
       added  _docs_demo_symbol  (crates/strata-index/src/changes.rs)
  affected: (nothing in the loaded graph depends on these changes)
Risk: LOW — 0 affected
```

That same diff modifying an existing, widely-called symbol would land at MEDIUM or higher: same tool, the risk follows what the change actually touches.

## What HIGH / CRITICAL means, and using it as a gate

`detect-changes` reports; it does not block. The gate is your workflow rule on top of it:

- **LOW / MEDIUM**: proceed; the affected set is what you'd expect.
- **HIGH**: wide blast radius. Re-read the affected list; make sure every dependent is one you intend to touch. If anything's a surprise, `explain` it (see [the impact guide](impact.md)).
- **CRITICAL**: the diff crosses into contract or infra surface, or a security-sensitive path. **Stop and get direction before committing.** This is the case grep would never warn you about: a one-line change to a resolver or a schema field that fans out to consumers in other repos.

For an AI agent, this is a standing rule from the steering kit:

> MUST run `detect_changes` before committing. Read its risk and affected set, report them, and pause for direction on HIGH/CRITICAL; do NOT hand-run `impact` symbol-by-symbol when `detect_changes` does it across every plane in one call.

In an agent setup, the same check can be wired as a pre-commit hook so it runs automatically (see [the agent kit reference](../reference/agent-kit.md)).

## Notes

- It reads the **committed graph** (`.strata/graph.duckdb`). If you've made large changes, reindex first (`strata index .`) so the changed-symbol resolution and blast radius reflect current code; otherwise a brand-new symbol may not yet be in the graph.
- `--repo <path>` points it at a repository root other than the one inferred from `--db`.
- **In an estate**, `detect-changes` run from inside an enrolled member repo (after `strata index --workspace`) aggregates the blast radius across the **whole estate**, so it catches dependents in other repos. With neither flag it auto-resolves from the repo's estate marker; pass `--workspace <manifest>` to force estate mode or `--db` to force single-repo. See [Multi-repo estates](../getting-started/estates.md) and [Cross-repository impact](cross-repo.md).
- It always exits 0, so it is safe to run from any script without it failing your pipeline. The reporting-not-gating contract is deliberate; the decision stays with you.

## What to do next

- Surprised by the affected set? Drill into one row with `explain` from the [impact guide](impact.md).
- Changed a schema field or endpoint? Cross-check producers and consumers with [Is this schema field dead?](dead-surface.md).
- Full flag list: [CLI reference](../reference/cli.md); the MCP `detect_changes` tool: [MCP reference](../reference/mcp.md).
