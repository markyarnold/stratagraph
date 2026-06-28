# StrataGraph

StrataGraph turns a codebase into a single queryable graph and answers one question precisely: **what breaks if I change this?** It crosses the boundaries that ordinary code-intelligence tools stop at: code, API contracts, cloud infrastructure, and the database. So the answer includes the Lambda that implements a GraphQL field, the frontend in another repository that queries it, and the table a handler reads, not just the functions in the file you are editing. Every relationship it reports carries provenance and a calibrated confidence, and where it cannot be sure it says so rather than guessing.

That last property is the differentiator: **cross-boundary impact that is never confident-wrong.** StrataGraph is recall-biased (it would rather show you a dependency that turns out to be safe than hide one), but it labels every edge honestly, marking the uncertain ones `Ambiguous` and surfacing the paths it could not resolve instead of silently dropping them. You can trust a high-confidence result and triage a low-confidence one, because the difference is measured, not decorative.

## What you can do with it

- **See the blast radius of a change before you make it**, across files, repositories, and planes. ([What breaks if I change this?](guides/impact.md))
- **Understand an unfamiliar codebase**, with the 360° context of any symbol, field, table, or cloud resource: who calls it, who produces it, who consumes it. ([Concepts: the cross-plane graph](concepts/graph.md))
- **Trace a request across boundaries**, from an IAM role to the Lambda that assumes it, to the code it runs, to the API operation it serves, to the frontend that calls it. ([Cross-boundary impact](concepts/cross-boundary.md))
- **Find dead contract surface**: a GraphQL field or API operation with no producer and no consumer is probably dead; StrataGraph flags it. ([Is this schema field dead?](guides/dead-surface.md))
- **Give your AI coding agent a blast-radius check before every edit**, via the MCP server and the one-command agent kit. ([Wire up your editor](getting-started/agent-kit.md))
- **Rename a symbol safely**: edits land only in the files the graph implicates, never a blind find-and-replace. ([Rename a symbol safely](guides/rename.md))

## A small taste

Ask what depends on a function, and StrataGraph returns the dependents ordered by distance, each with a confidence and an honest verdict:

```text
$ strata impact "classify_risk"
Impact of classify_risk (crates/strata-index/src/changes.rs) — 76 affected:
  depth  conf  amb  verdict     name (path)
      1  0.95   no  WILL BREAK  detect_changes (crates/strata-index/src/changes.rs)
      1  0.95   no  WILL BREAK  blast_for_file (crates/strata-index/src/changes.rs)
      2  0.76   no  WILL BREAK  tool_blast (crates/strata-mcp/src/tools.rs)
      4  0.69   no  WILL BREAK  call_tool (crates/strata-mcp/src/tools.rs)
      ...
```

`depth` is how many hops away the dependent is, `conf` is the calibrated confidence of the weakest edge on the path, and `amb` flags candidates StrataGraph could not resolve. When you want the *why*, ask `explain` for the evidence chain: every edge, its provenance, and the running confidence that produced the number:

```text
$ strata explain "classify_risk" "call_tool"
Why classify_risk affects call_tool (conf 0.69, WILL BREAK):
  classify_risk     —CALLS (Extracted 0.95)→  blast_for_file        running 0.95
  blast_for_file    —CALLS (Inferred  0.80)→  tool_blast            running 0.76
  tool_blast        —CALLS (Extracted 0.95)→  call_tool_ctx         running 0.72
  call_tool_ctx     —CALLS (Extracted 0.95)→  call_tool             running 0.69
```

(Counts and paths above come from indexing this repository; the exact numbers will drift as the code changes.)

## Where to go next

| If you want to… | Go to |
|---|---|
| Understand *why* StrataGraph exists and what it does differently | [Why StrataGraph: the problem](why/problem.md) → [the approach](why/approach.md) |
| Learn the mechanics: the graph, the planes, confidence | [Concepts](concepts/graph.md) |
| Install it and index your first repo | [Getting started: install](getting-started/install.md) |
| Do a specific task (impact, rename, dead-surface, cross-repo) | [Guides](guides/impact.md) |
| Look up a command, MCP tool, or the graph schema | [Reference: CLI](reference/cli.md) |
| See how accurate it is, and exactly where its limits are | [Accuracy and methodology](accuracy/methodology.md) |

StrataGraph runs fully offline as a single binary: a [CLI](reference/cli.md), an [MCP server](reference/mcp.md) for AI agents, a [desktop app](getting-started/desktop.md), and an [agent kit](getting-started/agent-kit.md) you install with one command. The full design rationale lives in the in-repo design document at `docs/strata-design.md`.

## License

StrataGraph is source available under the Functional Source License (FSL-1.1-ALv2). The whole suite is here and free to read, run, modify, self-host and redistribute for any non-competing purpose, including multi-repository estates, and each release becomes Apache 2.0 two years after it ships. A managed/hosted service may be offered commercially in future for teams that would rather not self-host; that sells operation, not the code.
