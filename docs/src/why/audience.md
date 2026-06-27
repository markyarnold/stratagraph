# Who StrataGraph is for

StrataGraph is for anyone who has to answer "what breaks if I change this?" and needs the answer to be both complete and honest. That covers four roles, each of which uses the same graph for a different payoff. You will likely wear more than one of these hats; the tool is the same, the question differs.

## Developers

**The payoff: change with confidence, and understand code you have never seen.**

Before you touch a symbol, run `impact` on it and see exactly what depends on it (across files and repositories), each dependent ranked by distance and labelled with a calibrated confidence. You no longer rename a function and discover the breakage in CI; you see the d=1 (WILL BREAK) and d=2 (likely affected) sets up front and decide deliberately.

```text
$ strata impact "classify_risk"
Impact of classify_risk (crates/strata-index/src/changes.rs) — 76 affected:
  depth  conf  amb  verdict     name (path)
      1  0.95   no  WILL BREAK  detect_changes (crates/strata-index/src/changes.rs)
      2  0.76   no  WILL BREAK  tool_blast (crates/strata-mcp/src/tools.rs)
      ...
```

When you land in an unfamiliar repository, `context` gives you the 360° view of any symbol: its callers and callees, what it imports, and (where the planes are present) which contract operations it produces or consumes and which tables it touches. It is the fastest way to learn how a piece of code fits into the system rather than reading outward from it one file at a time. And when you do need to rename, [`rename`](../guides/rename.md) edits only the files the graph implicates, never a blind find-and-replace that corrupts cross-file or cross-plane references it could not see.

Start with [Your first queries](../getting-started/first-queries.md) and the [impact guide](../guides/impact.md).

## AI coding agents

**The payoff: a structural, cross-boundary blast-radius check before every edit, one that is honest about its own uncertainty.**

This is the role StrataGraph was built MCP-native for. An agent edits fast and often in code it has not seen; without structural knowledge it edits blind to blast radius (see [the problem](problem.md#problem-3-ai-coding-agents-edit-blind-to-blast-radius)). StrataGraph closes that gap two ways:

- **The [MCP server](../reference/mcp.md)** exposes `query`, `context`, `impact`, `explain`, `detect_changes`, and `rename` to the agent, so it can assess a change *before* committing to it and read the evidence chain behind any result.
- **The [agent kit](../getting-started/agent-kit.md)** (`strata init claude` or `strata init kiro`) installs a strictly-governed integration: steering rules that require the agent to assess blast radius before modifying anything, a pre-edit hook that injects a file's blast radius as context at edit time, and a `detect_changes` pre-commit check. The kit treats confidence bands as trust policy (act on ≥ 0.90, verify 0.40–0.89, treat < 0.40 or ambiguous as **unknown and say so**) so the agent never reports an uncertain impact as certain, and never claims "nothing depends on this" from a textual search alone.

The honesty discipline matters most here: an agent that confidently asserts safety is dangerous precisely when it is wrong. StrataGraph gives the agent a result it can *calibrate*, plus dead-surface flags so it can tell a safe deletion from a breaking one. (You saw the pre-edit hook in action while these very docs were written: each file edit was preceded by an injected blast-radius report.)

## Tech leads and reviewers

**The payoff: cross-boundary risk on a change set, and dead surface you can safely remove.**

When you review a change, `detect_changes` gives you the mechanical pre-commit picture: it diffs the working tree, derives the changed symbols per plane (code / contract / infrastructure), aggregates the blast radius over the whole graph, and returns a risk level with reasons. You get the cross-boundary view a per-file diff cannot give you (a one-line schema change flagged because it touches contract surface a separate frontend consumes) and a clear signal of when to pause for HIGH/CRITICAL risk. See [Pre-commit change checks](../guides/detect-changes.md).

For cleanup and architecture work, **dead-surface detection** earns its keep: a GraphQL field or API operation with **zero producers and zero consumers** is probably dead, and StrataGraph flags it rather than leaving you to prove a negative by hand. Removing genuinely dead contract surface is one of the safest high-value cleanups a lead can sign off on, and StrataGraph gives you the evidence. See [Is this schema field dead?](../guides/dead-surface.md).

## Platform and infrastructure engineers

**The payoff: blast radius across the infrastructure seam, showing what an IAM role, a Lambda, or a contract change actually reaches.**

This is the plane no comparable tool offers. When the infrastructure plane is active (CloudFormation/SAM or Terraform/Terragrunt present), StrataGraph links cloud resources to the code they run and the contracts they serve, so infrastructure changes get a real blast radius:

- **Change or remove an IAM role** and `impact` follows `ASSUMES` → `RUNS` → `PRODUCES` to show every Lambda that assumes it, the code each runs, and the operations that code serves: the role's full operational footprint before you apply. (This directly fixes the failure where a role's dependents are invisible until something breaks at runtime.)
- **Change a Lambda or handler module** and see its compute, the operations it produces, and the consumers downstream of those operations, across repositories.
- **Change a contract or a table** and trace it back through the infrastructure that implements it to the frontend that depends on it.

See [Cross-boundary impact](../concepts/cross-boundary.md) and the [plane walkthroughs](../guides/plane-walkthroughs.md).

### An honest note on infrastructure scope

StrataGraph is deliberate about what the infrastructure plane claims today. The AWS vertical (SAM/CloudFormation parsing, AppSync resolver → GraphQL field links, role/Lambda/handler edges) and Terraform/Terragrunt ingestion are built. Some capabilities described in the design are **deferred and labelled as such**, notably IAM *permission-gap* detection (reconciling the AWS actions code calls against what a role grants). Where a capability is not built, the docs say so plainly rather than implying it works; see [Honest limitations](../accuracy/limitations.md). The same honesty bar that governs the graph governs these docs.

---

Whichever role you are in, the next step is the same: [install StrataGraph](../getting-started/install.md) and [index your first repository](../getting-started/first-index.md). If you want the mechanics first, start with [Concepts](../concepts/graph.md).
