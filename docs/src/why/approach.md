# The StrataGraph approach

StrataGraph answers the three problems on the [previous page](problem.md) with one idea: build a **single cross-plane graph** of the whole system (code, contracts, infrastructure, and data), where every edge carries honest provenance and a calibrated confidence, and then run impact analysis *over that graph* so it follows dependencies across the seams other tools stop at. This page explains the shape of that answer and the philosophy behind it. For the mechanics, follow the links into [Concepts](../concepts/graph.md).

## One graph, many planes

StrataGraph builds a single queryable graph whose nodes and edges come from up to five **planes**:

| Plane | What it captures | Activates when |
|---|---|---|
| **Code** (always on) | Functions, classes, methods, modules, calls, imports, inheritance | always: the universal core |
| **Contract** | API operations, GraphQL fields, gRPC methods: the interface between components | OpenAPI/Swagger, GraphQL, or gRPC/protobuf specs are present |
| **Infrastructure** | Cloud resources: Lambdas, IAM roles, AppSync APIs, and the edges between them | CloudFormation/SAM or Terraform/Terragrunt is present |
| **Data** | Tables, columns, foreign keys, and the code that reads/writes them | SQL DDL or supported ORM models are present |

The code plane is built for every codebase. Each other plane is a **progressive enhancement**: it activates itself when StrataGraph detects its inputs and contributes nothing until then. A single-language repository with no specs, no schema, and no infrastructure gets a clean code graph and never encounters a contract or cloud concept. The same engine, run against a repository that *does* have those artefacts, lights up the corresponding planes automatically, with no configuration required to get started. (A fifth, **knowledge** plane for design docs and ADRs is described in the design doc as future work and is not built today.)

The power is that wherever planes coexist, they form **one** graph you traverse in a single query. See [The five planes](../concepts/planes.md) and [The cross-plane graph](../concepts/graph.md) for the full model.

## Cross-boundary impact: follow the chain, not just the calls

Because all four planes live in one graph, impact analysis can follow a dependency through edge families that a call-graph tool never has. The flagship traversals chain across boundaries:

- **Producer → operation → consumer.** A backend resolver `PRODUCES` a GraphQL field; a frontend `CONSUMES` it. StrataGraph follows that chain, so changing the resolver surfaces the consumer, even in another repository, even in another language.
- **Code → table.** A handler `READS`/`WRITES` a column; an ORM model `MAPS_TO` a table. Change the column and StrataGraph names the code that touches it.
- **Role → compute → reach.** An IAM role is `ASSUMES`-d by a Lambda, which `RUNS` a handler, which `PRODUCES` an operation. Change the role and StrataGraph shows its full operational footprint: the compute that depends on it and what that compute reaches.

The cross-repository unlock is **stable identity**: a contract operation has one canonical identity across the estate, so a producer in one repo and a consumer in another resolve to the *same* node and the edge between them is exact, not fuzzy. A [workspace manifest](../getting-started/estates.md) declares which repositories belong to one system. (Identity is deliberately conservative: two unrelated APIs that happen to share an operation key are kept as distinct nodes, never silently merged; merging requires an explicit declaration. This is detailed in [Cross-boundary impact](../concepts/cross-boundary.md).)

## Never confident-wrong

The differentiator is not just *reach*: it is reach you can trust. StrataGraph's core promise is that it is **never confident-wrong**: it does not present an uncertain result as a certain one. This is engineered, not aspirational, and it rests on three commitments.

### 1. Provenance and calibrated confidence on every edge

Every edge records *how* it was derived and *how much* to trust it. The bands:

| Provenance | Meaning | Confidence |
|---|---|---|
| **Resolved** | Derived by a compiler / language server with full symbol resolution (SCIP, for TypeScript) | compiler-grade |
| **Extracted** | Read directly from a deterministic source: an AST node, a spec file, DDL | high (≥ 0.90) |
| **Inferred** | Derived heuristically: a name match, a framework convention | medium (0.40–0.89) |
| **Ambiguous** | Multiple candidate targets, none confidently selected | low (< 0.40) |

An inference can never masquerade as a fact, because its band says otherwise. A confidence of 0.9 is meant to be right about 90% of the time, and that calibration is *measured* against compiler ground truth, not asserted; see the [accuracy reports](../accuracy/results.md). The full model is in [Confidence and provenance](../concepts/confidence.md).

### 2. Surface uncertainty; never silently drop a path

When resolution genuinely fails (dynamic dispatch, reflection, a string-built SQL query, a runtime-constructed ARN), StrataGraph does **not** guess and does **not** quietly omit the path. It emits an `Ambiguous` marker and surfaces the unresolved candidates. You can see this directly: ask `impact` for a symbol whose name has several definitions and StrataGraph lists every candidate rather than picking one for you.

```text
$ strata impact "new"
error: ambiguous symbol new: 19 candidates — pick one:
  rust|strata|crates/strata-core/src/graph.rs|Graph::new|   [Method]  new  (crates/strata-core/src/graph.rs)
  rust|strata|crates/strata-core/src/ids.rs|Uid::new|       [Method]  new  (crates/strata-core/src/ids.rs)
  rust|strata|crates/strata-core/src/model.rs|Confidence::new| [Method] new (crates/strata-core/src/model.rs)
  ...
```

A blast radius that silently skips a path it could not analyse is worse than one that admits it. StrataGraph always admits it. The `amb` column on `impact` results, and the `Ambiguous` band in `explain`, are how that admission reaches you.

### 3. Recall-biased, but honestly labelled

For blast radius, the unforgivable failure is a **missed** dependency (a real one the tool hid) because that is the one that ships a broken change. Over-flagging a safe dependency is a triage cost; missing a breaking one is an outage. So StrataGraph is **recall-biased**: `impact` surfaces *everything*, including low-confidence and ambiguous edges, and uses the confidence band to let you triage the noise rather than hiding it.

The **WILL BREAK** verdict is the label on this. An edge is labelled WILL BREAK when its confidence is at or above the will-break cutoff **and** it is not ambiguous. That cutoff is resolved by measurement, set at the lowest band whose measured precision crosses the bar (the Inferred floor, 0.40). The label governs presentation only; it does not filter the results. Everything below the bar is still shown, marked, so you decide. This is why `impact` returns 76 dependents for a function and tells you, per row, how much to trust each one; see [What breaks if I change this?](../guides/impact.md).

### Seeing the reasoning: `explain`

Because every edge is labelled, StrataGraph can show you the *evidence chain* behind any impact result: the visible form of never-confident-wrong. Ask `explain` why one symbol is in another's blast radius:

```text
$ strata explain "classify_risk" "call_tool"
Why classify_risk affects call_tool (conf 0.69, WILL BREAK):
  classify_risk       —CALLS (Extracted 0.95)→  blast_for_file        running 0.95
  blast_for_file      —CALLS (Inferred  0.80)→  tool_blast            running 0.76
  tool_blast          —CALLS (Extracted 0.95)→  call_tool_ctx         running 0.72
  call_tool_ctx       —CALLS (Extracted 0.95)→  call_tool             running 0.69
```

Each hop shows its edge kind, its provenance band, the per-edge confidence, and the running confidence (the weakest link so far) that produces the final number. Where one hop is `Inferred` rather than `Extracted`, you can see exactly which link is the soft one and decide whether to trust the path. (Output from indexing this repository; numbers will drift.)

## Deterministic first, model optional

One more principle underwrites the trust: the structural graph is built **without any language model** and is fully reproducible. Tree-sitter and compiler-grade resolvers do the extraction; the reliability-critical traversals are owned, tested Rust algorithms over an in-memory graph, covered by golden fixtures. Models are reserved for the optional knowledge plane and never create dependency edges. The graph you gate a deploy on is deterministic and content-addressed: run it twice and you get the same answer.

For the architecture behind this, see [Architecture](../project/architecture.md). For exactly which languages and formats are covered today, and where the bounds are, see [Languages and coverage](../concepts/coverage.md) and [Honest limitations](../accuracy/limitations.md).

---

Next: [Who StrataGraph is for](audience.md), covering how developers, AI agents, tech leads, and platform engineers each put this to work.
