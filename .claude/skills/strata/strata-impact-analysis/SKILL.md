---
name: strata-impact-analysis
description: "Use when the user wants to know what will break if they change something, or needs safety analysis before editing. Examples: \"is it safe to change X?\", \"what depends on this?\", \"what will break?\""
---

# StrataGraph: Impact Analysis

## When to Use

- "Is it safe to change this function / field / operation?"
- "What will break if I modify X?" / "Show me the blast radius."
- **Before any non-trivial edit, and before a rename**: always, per the steering rules.
- Before a commit, to confirm the change touches only what you intend.

## Workflow

```
1. query({ text })                    → resolve the exact symbol
2. impact({ symbol })                 → contract- & infra-aware blast radius
3. (optional) impact({ symbol, include_contracts: false, include_infra: false })
                                      → code-only radius, to separate planes
4. classify each dependent by its verdict + confidence; assign risk
5. (optional) explain({ symbol, affected }) → WHY a dependent is in the radius
6. report to the user; PAUSE if HIGH/CRITICAL or cross-repo
```

## Checklist

```
- [ ] impact() run BEFORE editing (not after)
- [ ] direct (d=1) dependents reviewed first, but read each row's verdict
- [ ] WILL BREAK only where will_break=true (conf >= 0.40 AND not ambiguous)
- [ ] confidence band applied per item (≥0.9 act / 0.4–0.8 verify / <0.4 unknown)
- [ ] ambiguous:true or <0.40 items called out as UNKNOWN, never as certain
- [ ] cross-plane / cross-repo consumers noted (contract-aware default)
- [ ] risk level assigned and reported; HIGH/CRITICAL → pause for direction
```

## Understanding Output

`impact` returns `affected[]`, each with `depth`, `confidence`, `ambiguous`, and the derived `will_break` verdict (the WILL BREAK / may-affect call: `confidence ≥ 0.40` AND not `ambiguous`, independent of depth):

| Depth (distance) | Meaning | Risk rubric |
|---|---|---|
| d=1 | direct dependent | LOW < 5 affected |
| d=2 | indirect dependent | MEDIUM 5–15 affected |
| d=3 | transitive dependent | HIGH > 15, or many flows |
| – | contract/infra path | CRITICAL: auth, payments, or contract surface |

**Verdict is per row, not per depth.** Each affected node carries a `will_break` flag (`WILL BREAK` when `confidence ≥ 0.40` AND not `ambiguous`, else `may affect`), computed independent of depth. A d=1 dependent that is ambiguous or below 0.40 is `may affect`, never a certain break; report it as UNKNOWN per the band policy.

| Confidence | Policy |
|---|---|
| ≥ 0.90 | Act on it. |
| 0.40 – 0.89 | Verify in the source before relying on it. |
| < 0.40 or `ambiguous: true` | UNKNOWN: say so explicitly; never present it as certain. |

**Zero direct dependents is NOT "dead" on a member-bearing node.** When `impact` on a class/struct/enum/interface/Table returns 0 DIRECT dependents but the result carries a non-empty `members_with_dependents`, the type is NOT dead: its methods/columns have dependents. The tool surfaces them (the CLI prints `0 dependents on X itself; N of its members have dependents: …`, MCP returns a `members_with_dependents` field, the desktop shows the same hint); run `impact` on a named member to see those dependents. This is the never-say-"nothing depends on this" guarantee in action, so report the members, never a bare "nothing affected."

To justify any single row, run `explain({ symbol, affected })`: it returns the evidence chain (each hop's edge kind, provenance, and confidence, plus the running confidence that yields impact's number), or `reachable: false` when the node is not actually in the radius. Use the SAME `include_contracts`/`include_infra` toggles you ran `impact` with, so the explained confidence matches the row.

## Worked Example: "What breaks if I change `Query.getPolicyStats`?"

```
impact({ symbol: "getPolicyStats" })
→ affected:
  d=1  conf 0.95  amb no   PolicyOperationsFunction (Lambda)     WILL BREAK
  d=1  conf 0.95  amb no   frontend/policies.ts (gql consumer)   WILL BREAK
  d=1  conf 0.30  amb yes  legacy/probe.ts (heuristic call)      may affect

Three DIRECT (d=1) dependents, but the verdict is per row, not from the
depth. The two 0.95 non-ambiguous rows are WILL BREAK → act. The third is
d=1 too, yet ambiguous at 0.30 → `may affect`: report it as UNKNOWN, never
as a certain break. Two of these cross into the contract plane (producer +
consumer) → report it and PAUSE for direction before editing the schema.
```

Compare `impact({ symbol: "getPolicyStats", include_contracts: false })`: the frontend consumer drops out, leaving the code-only radius, useful to see which dependents are cross-plane.
