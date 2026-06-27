---
name: strata-guide
description: "Use first when you are unsure which StrataGraph tool to reach for, or need the tool/plane/band reference. Examples: \"which tool do I use?\", \"what does StrataGraph index?\", \"how do I read confidence?\""
---

# StrataGraph: Guide

StrataGraph is a cross-plane code graph: **code** (functions, classes, imports, calls), **contract** (GraphQL fields, API operations), and **infra** (Lambdas, cloud resources), linked by producer/consumer and runs/routes edges. It answers "what breaks if I change X?" across all three, including links grep cannot see.

## When to Use

- You don't yet know which tool answers the question.
- You need the tool surface, the plane model, or the confidence-band policy.
- You're about to edit and want the safe-change protocol in one place.

## The three tools

- **`query({ text })`**: find the symbol by name / fqn / path (case-insensitive substring). Start here when you only have a name.
- **`context({ symbol })`**: the 360° view: `callers`, `callees`, `imports_in`/`imports_out`, `members`, `container`, and the contract buckets `producers` / `consumers` / `produces` / `consumes`.
- **`impact({ symbol, depth?, min_confidence?, include_contracts?, include_infra? })`**: reverse blast radius. Contract- and infra-aware by default (follows producer → operation → consumer, and Assumes/Routes/Runs); pass `include_contracts: false` and/or `include_infra: false` to narrow it.
- **`explain({ symbol, affected, … })`**: the evidence chain proving WHY `affected` is in `symbol`'s blast radius (per-edge provenance/confidence + the running confidence), or an honest `reachable: false`.

## Reading confidence (trust policy)

| Confidence | Policy |
|---|---|
| ≥ 0.90 | Act on it. |
| 0.40 – 0.89 | Verify in the source before relying on it. |
| < 0.40 or `ambiguous: true` | UNKNOWN: say so explicitly; never present it as certain. |

## Blast radius & risk

| Depth (distance) | Meaning | Risk rubric |
|---|---|---|
| d=1 | direct dependent | LOW < 5 affected |
| d=2 | indirect dependent | MEDIUM 5–15 affected |
| d=3 | transitive dependent | HIGH > 15, or many flows |
| – | contract/infra path | CRITICAL: auth, payments, or contract surface |

**Verdict is per row, not per depth.** Each affected node carries a `will_break` flag (`WILL BREAK` when `confidence ≥ 0.40` AND not `ambiguous`, else `may affect`), computed independent of depth. A d=1 dependent that is ambiguous or below 0.40 is `may affect`, never a certain break; report it as UNKNOWN per the band policy.

## Safe-change protocol

```
1. query(name)              → resolve the exact symbol
2. context(symbol)          → which planes does it touch?
3. impact(symbol)           → who breaks (d=1 / d=2, confidence, risk)
4. report to the user; PAUSE if HIGH/CRITICAL or cross-repo
5. change only the d=1 set the graph reports
```

> **Auto-reload:** the server hot-reloads the graph when the on-disk index changes (the edit hook's reindex, or a manual one), no session restart needed. The swap is degrade-safe: a reindex caught mid-write keeps the current graph and retries on the next call.
