---
name: strata-contracts-and-infra
description: "Use when changing a GraphQL field / API operation / schema, or working with Lambdas and cloud resources, to find producers, consumers, and dead contract surface. Examples: \"who implements this field?\", \"who consumes this operation?\", \"is this schema field dead?\""
---

# StrataGraph: Contracts & Infra

The contract and infra planes are why StrataGraph sees what grep can't: a Lambda *produces* a GraphQL field; a frontend module *consumes* an operation. Changing a field without checking both sides is how cross-plane breakage ships.

## When to Use

- Editing a GraphQL field / API operation / schema file.
- "Who implements (produces) this field?" / "Who queries (consumes) it?"
- "Is this field dead?": zero producers and zero consumers.
- Touching a Lambda or cloud resource and needing its produced/consumed surface.

## Workflow

```
1. query({ text })          → resolve the field/operation/Lambda
2. context({ symbol })      → producers + consumers (and produces/consumes)
3. impact({ symbol })       → full cross-plane blast radius before editing
4. dead-surface check: producers (0) AND consumers (0) → flag as likely dead
```

## Checklist

```
- [ ] context() read BEFORE editing any schema/contract file
- [ ] producers bucket inspected: who implements it (which Lambda/resolver)
- [ ] consumers bucket inspected: who queries it (which frontend/module)
- [ ] 0 producers AND 0 consumers → reported as likely-dead surface
- [ ] cross-repo consumers noted; contract surface → warn and pause
```

## Understanding Output

On a **field/operation**: `producers` = implementers (Lambda/resolver), `consumers` = callers (frontend/module). On a **Lambda/module**: `produces` = fields it implements, `consumes` = operations it calls. All four buckets are always present, so `producers (0) / consumers (0)` is a real, readable signal, not a missing answer.

## Worked Example A: live field

```
context({ symbol: "getPolicyStats" })
→ producers (1): PolicyOperationsFunction
→ consumers (1): frontend/policies.ts
Live, cross-plane. Editing it affects both sides → report + pause.
```

## Worked Example B: dead-surface discovery

```
context({ symbol: "getActiveGeneralPolicies" })
→ producers (0)
→ consumers (0)
Zero producers AND zero consumers → likely DEAD schema surface.
Flag it to the user (candidate for removal); do NOT assume it is wired up.
```
