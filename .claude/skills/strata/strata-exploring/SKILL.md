---
name: strata-exploring
description: "Use when the user asks how code works, wants the architecture, or to trace a flow across code/contract/infra. Examples: \"how does the policy flow work?\", \"what calls this?\", \"trace this operation end to end\""
---

# StrataGraph: Exploring

## When to Use

- "How does X work?" / "Walk me through the Y flow."
- "What calls this function?" / "What does this module import?"
- Tracing an operation across planes: frontend consumer → GraphQL field → producer Lambda.
- Prefer this over grep: the graph sees cross-file, cross-plane, and cross-repo edges grep misses.

## Guidance-first: check what the repo already knows

Before reading source in an unfamiliar area, ask the knowledge plane first — the repo may already explain it, cheaper than reconstructing it from code: run `guidance({ file: "<path>" })` for a token-budgeted digest of the doc comments and docs that cover that file, and `search_docs({ query: "<question>" })` for a lexical search across the whole indexed docs tree (markdown, doc comments, spec descriptions). Both are refs-first and explainable — never a summary or an answer — so treat what they return as a lead into the source, not a replacement for reading it. Doc guidance is repo knowledge, not ground truth: a stale or ambiguous mention is UNKNOWN, same trust policy as every other band.

## Workflow

```
0. guidance({ file })            → what does the repo already say about this file?
   search_docs({ query })         → or search the whole docs tree by question
1. query({ text: "concept" })   → find candidate symbols/fields
2. context({ symbol })           → callers/callees + producers/consumers + docs
3. follow the buckets outward     → walk the flow across planes
```

## Checklist

```
- [ ] for an unfamiliar file/area, try guidance()/search_docs() before reading source
- [ ] query() to locate the entry symbol (don't guess the fqn)
- [ ] context() to see callers (who reaches it) and callees (what it reaches)
- [ ] for a field/operation, read producers (implementers) + consumers (callers)
- [ ] follow produces/consumes to cross from code into the contract plane
- [ ] check the docs bucket for sections that document/mention the symbol
- [ ] state confidence honestly per the band policy
```

## Understanding Output

`context` buckets, by plane:
- **code:** `callers` (who calls it), `callees` (what it calls), `imports_in`/`imports_out`, `members`, `container`.
- **contract:** `producers` (who implements this field/op), `consumers` (who queries it); `produces`/`consumes` are the outgoing views from a Lambda/module.
- **knowledge:** `docs` — every doc section that documents (a doc comment) or mentions this symbol, refs only (`uid`, `name`, `anchor`, `provenance`, `confidence`); fetch a section's body with `guidance({ symbol, section: <anchor> })`.

| Confidence | Policy |
|---|---|
| ≥ 0.90 | Act on it. |
| 0.40 – 0.89 | Verify in the source before relying on it. |
| < 0.40 or `ambiguous: true` | UNKNOWN: say so explicitly; never present it as certain. |

## Worked Example: "How is `getPolicyStats` served?"

```
1. query({ text: "getPolicyStats" })
   → GraphqlField  Query.getPolicyStats

2. context({ symbol: "getPolicyStats" })
   → producers: PolicyOperationsFunction (Lambda)      [implements it]
   → consumers: frontend/policies.ts                   [queries it]

3. Flow: policies.ts ──query──> Query.getPolicyStats ──served by──> PolicyOperationsFunction
```

That single `context` crossed two planes (the frontend consumer and the producer Lambda) without reading a line of source.
