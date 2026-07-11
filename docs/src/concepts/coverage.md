# Languages and coverage

This page is the honest capability matrix: which languages StrataGraph reads, which
planes it builds, and (for each) what is **supported** (an `Extracted`/`Resolved`
fact), what is **inferred** (a band-capped heuristic), and what is **deferred**
(not built). The deferrals here are deliberate product positioning, not
embarrassments to bury: StrataGraph's value rests on never claiming more than it can
prove (see [Confidence and provenance](confidence.md)), so being precise about the
edges of its knowledge is part of the guarantee, not a footnote to it.

Read this alongside [The five planes](planes.md) (what each plane does) and the
[Measured results](../accuracy/results.md) (the numbers behind the claims). Where
this page says "measured", the figures live in `docs/accuracy/`.

## Languages

Five languages across four grammars (TypeScript and JavaScript share one):

The table below has **four rows, one per grammar**, not one per language: the first row covers two of the five languages (TypeScript and JavaScript) because they share a single grammar.

| Language | Extensions | Code plane | Call resolution |
| --- | --- | --- | --- |
| TypeScript / JavaScript | `.ts` `.tsx` `.js` `.jsx` `.mjs` `.cjs` | Yes | **Compiler-grade (SCIP)** + heuristic fallback |
| Python | `.py` `.pyi` | Yes | Heuristic, band-disciplined |
| Rust | `.rs` | Yes | Heuristic, band-disciplined |
| C# | `.cs` | Yes (extraction) | Heuristic, band-disciplined |

Every language gets the **same** code-plane structure (`Repo`/`Module`/`Class`/
`Interface`/`Function`/`Method` nodes; `Defines`/`MemberOf`/`Imports`/`Calls`
edges) and the **same** confidence discipline. All four are first-class; they
differ in exactly one axis: the top call-resolution tier.

**Every language is fully supported; only the highest resolution tier is
TypeScript-specific today.** TS/JS is the one language with a SCIP adapter
(`scip-typescript`) wired in, so its call edges can reach the compiler-verified
`Resolved` tier (0.97). Python, Rust, and C# resolve calls with **heuristics
graded into bands**: a same-file binding is `Extracted` 0.95, a confident
heuristic (`self.`/`this.`, import-matched, unique repo-wide name, Rust
`Type::method()`) is `Inferred` 0.80, and an ambiguous one fans out below 0.40.
And those heuristics are **measured, not assumed**: validated against
compiler-grade SCIP ground truth for TypeScript, Python, and Rust, where the
`Extracted` and `Inferred` bands score ~1.0 precision (`docs/accuracy/
ts-resolution.md`, `py-resolution.md`, `rust-resolution.md`); **C# is
extraction-validated only** so far (`docs/accuracy/cs-extraction.md`). This is
the design's deliberate rollout order: Tree-sitter heuristic extraction with
banded confidence ships first for each language; compiler precision (Roslyn for
C#, pyright/SCIP for Python, rust-analyzer for Rust) follows.

Each language is honest about its own dynamism, and never guesses it: a Python
`getattr(...)()`, a Rust macro invocation (not even a call), a C# reflective
`mi.Invoke()` or `dynamic` receiver, and a JS computed member are dropped or kept
only at the Ambiguous band, never presented as a confident call.

## Language × plane

Which planes a repository gets depends on the *artifacts* present, not only the
code language. The code plane is per-language; the contract, infra, and data
planes are keyed off specs/IaC/DDL and then **bridge back** to whatever code
language the handler is written in.

| | Code | Contract | Infrastructure | Data |
| --- | --- | --- | --- | --- |
| **TypeScript / JavaScript** | Full, SCIP-resolved | Producer + consumer | `Runs` bridge ✓ | `Reads`/`Writes`, TypeORM `@Entity` `MapsTo` |
| **Python** | Full, heuristic | Producer + consumer² | `Runs` bridge ✓ | `Reads`/`Writes`, SQLAlchemy/Django `MapsTo` |
| **Rust** | Full, heuristic | None (no framework adapters) | `Runs` bridge deferred¹ | `Reads`/`Writes` (raw SQL) |
| **C#** | Extraction, heuristic | None (no framework adapters) | `Runs` bridge deferred¹ | `Reads`/`Writes` (raw SQL) |

¹ The code `Module` nodes exist; only the infra→code `Runs` *edge* is deferred,
because a C#/Rust Lambda `Handler` is a built-assembly / binary name, not a source
path. The Lambda is counted as handler-unresolved, honestly, rather than linked to
a guessed file.

² Python contract linking covers Flask/FastAPI/Django **producer routes**,
`requests`/`httpx` **consumer calls** (plus `aiohttp`'s direct module forms —
the session-variable pattern needs type information and is never guessed),
`gql("…")` **consumer documents**, and
Graphene/Strawberry/Ariadne **resolver producers**, all at the same banded confidence as
TS/JS. Django routes carry no HTTP method (the view dispatches internally), so they
match on path alone at a slightly lower `Inferred` tier (0.65) and attribute to the
`urls` module when the view is defined cross-file (`docs/accuracy/py-extraction.md`).

A few notes the matrix compresses:

- **Contract producer/consumer linking** is driven by framework-shaped signals
  (route declarations, GraphQL resolver maps, HTTP-client calls, GraphQL
  documents). These are recognised in the **TypeScript and Python** analyzers
  today (TS/JS reads `fetch`/`axios`/`got`/`ky`/`superagent` calls; Python adds
  Flask/FastAPI/Django routes, `requests`/`httpx`/`aiohttp` calls,
  `gql` documents, and Graphene/Strawberry/Ariadne resolvers); Rust and C# code is
  not yet scanned for producer/consumer signals, so a Rust/C# service's *code* does
  not link to contract operations even though its spec files are still parsed into
  operation nodes.
- **The `Runs` bridge** lands for **TS/JS and Python** handlers (file-path
  handlers). The contrast with Rust/C# above is exactly the build-artifact vs
  source-path distinction.
- **ORM `MapsTo`** is **explicit-name only** and per-framework: SQLAlchemy
  `__tablename__` and Django `db_table` (Python), TypeORM `@Entity("…")` (TS). See
  the data-plane deferrals below.

## Per plane: supported / inferred / deferred

### Code plane

- **Supported (`Extracted`/`Resolved`).** Symbol structure and containment
  (`Defines`/`MemberOf`, 1.0); module/package imports; SCIP-resolved calls for
  TS/JS (0.97); same-file/same-module call bindings (0.95).
- **Inferred (band-capped).** Heuristic calls in all languages: `this.`/`self.`
  methods, import-matched names, unique repo-wide names, Rust `Type::method()`
  (0.80); ambiguous fan-outs (< 0.40).
- **Deferred.** Compiler-grade resolution for Python/Rust/C# (heuristic today);
  `Extends`/`Implements` inheritance edges (reserved in the vocabulary, **emitted
  by no analyzer**); cross-language call linking (a TS function calling into a
  Python service links through the contract/infra plane, not directly).

### Contract plane

- **Formats.** OpenAPI/Swagger, GraphQL SDL (including AppSync), gRPC `.proto`.
- **Supported (`Extracted`).** Operation nodes from every format (1.0); GraphQL
  consumer documents that parse to a unique field (0.95, the document names the
  contract in its own language).
- **Inferred.** Producer links (route/resolver → operation by method+path or
  `type.field` convention; single 0.80, multi 0.35); REST consumer links by
  name/URL convention (≤ 0.80, ambiguous 0.35).
- **Deferred.** **gRPC producer/consumer code linking**: `.proto` rpcs are
  *extracted* into `ApiOperation` nodes, but no producer/consumer code is linked
  to them yet (extraction milestone only; see `docs/accuracy/grpc-linking.md`).
  **AsyncAPI / event schemas** (`EventSchema`, `Topic`, `PUBLISHES`/`SUBSCRIBES`)
  are designed but not built. Contract producer/consumer signals are scanned in
  TS and Python code (not Rust/C#).

### Infrastructure plane

- **Formats.** CloudFormation (JSON/YAML), SAM (`template.yaml`, transform
  expanded), Terraform (static HCL + `terraform show -json` plan), Terragrunt
  (structural dependencies).
- **Supported (`Extracted`).** Resource inventory (`LambdaFn`, `IamRole`, AppSync
  API/resolver/datasource, generic `CloudResource`); same-template `Ref`/`GetAtt`
  wiring (0.95); the `Runs` bridge for TS/Python handlers (0.95); the AppSync
  resolver→`GraphqlField` money link when the chain is fact-graded (0.95);
  Terragrunt unit→unit deps (0.95); and IAM `Grants` (`IamRole` → `CloudAction`,
  0.95) from CloudFormation/SAM inline policies and expanded SAM policy templates
  and Terraform inline/standalone policies (Track D2), with any un-enumerable
  grant recorded as an `<opaque:…>` indeterminacy marker.
- **Inferred.** References recovered from `Sub`/`Join`/`Fn::If` interpolations
  (0.70); a `Fn::If` over distinct targets emits one edge per branch.
- **Deferred.** **IAM permission-gap reconciliation** (Track D2, in progress):
  the `Grants` half ships (above), but the `RequiresPermission` half (boto3 / AWS
  SDK v3 call → action detection) and the `permission_gap` traversal/tool that
  reconciles the two are **not built yet**, so gap detection itself is
  unavailable; Terraform **plan-JSON** IAM grants are a close follow-on to the
  shipped HCL path. The
  `Runs` bridge for C#/Rust handlers (build-artifact handlers). Non-AWS providers
  (Kubernetes, GCP, Azure): the model is provider-agnostic but only the AWS
  adapter exists. OpenTofu (close to free later via the shared plan JSON). Cross-
  unit Terragrunt *attribute* wiring (`dependency.x.outputs.*` is not evaluated:
  the structural dependency is, the resource-level cross-wiring is not).

### Data plane

- **Formats.** SQL DDL (`CREATE TABLE` + cumulative `ALTER`), via `sqlparser`,
  parsed with the PostgreSQL dialect plus a **ClickHouse recovery ladder**: the
  ClickHouse dialect as a fallback, then a column-list recovery that strips
  ClickHouse-only decoration (`ENGINE`/`PARTITION BY`/`TTL`/`SETTINGS` tails,
  `CODEC`/`ALIAS`/`MATERIALIZED` column modifiers, inline `INDEX`/`PROJECTION`
  entries) — every recovery re-validated by the parser, so the declared column
  set is exact and nothing is guessed. RBAC/maintenance statements and
  clone/CTAS tables (whose shape lives elsewhere) are recognized and skipped
  with counts, never reported as file failures.
- **Supported (`Extracted`, all 0.95).** `Table`/`Column` nodes; `HasColumn`
  containment; `ForeignKey` from explicit `REFERENCES`/`FOREIGN KEY`; code→table
  `Reads`/`Writes` from raw-SQL string literals; ORM `MapsTo` from an **explicit
  table name**: SQLAlchemy `__tablename__`, Django `db_table`, TypeORM
  `@Entity("…")`.
- **Inferred.** None today. The data plane infers nothing this milestone: every
  edge is an explicit-DDL or explicit-literal fact. (Convention-derived ORM names
  would be the first `Inferred` tier, and it is not built.)
- **Deferred (honest bounds: real positioning).**
  - **Convention-derived ORM names**: guessing a table from a class name with no
    explicit literal (Rails/Django implicit names, a SQLAlchemy model with no
    `__tablename__`). This is the future `Inferred` ORM tier.
  - **Drizzle** (`pgTable("x", …)`, a `const`, which the TS analyzer does not yet
    emit as a symbol to anchor a `MapsTo`), **Prisma** `.prisma` schemas (a new
    parser), **C# Entity Framework** (`[Table("x")]`).
  - **Column-level** read/write granularity: `Reads`/`Writes` are table-level;
    which columns a query touches is not tracked.
  - **Dynamic SQL**: concatenated/interpolated query strings (not a single
    literal) are not captured, so they form no edge.
  - **Runtime `OBSERVED` links**: query-log / `pg_stat_statements` ingestion is a
    future runtime-observation enhancement, not built. The whole plane is static-first by design.

  A model or query that names a table the parsed DDL never declares yields **no
  edge**: counted as unresolved and surfaced, never invented. A repo whose models
  rely entirely on convention reports honest zeros, not false links.

### Knowledge plane

**Not built.** The fifth plane (LLM/vision over docs and diagrams, `MODEL`
provenance) is designed but unimplemented. See
[The five planes](planes.md#knowledge-plane-future-not-built). Its defining rule
is already fixed: `MODEL` provenance never gates impact.

## Surfaces

Coverage of *capabilities* is independent of *how you reach them*: all four
surfaces call the same `strata-core` traversals over the same graph:

| Surface | What it is |
| --- | --- |
| **CLI** | `strata query` / `context` / `impact` / `explain` / `detect-changes` / `rename` / `blast`. See the [CLI reference](../reference/cli.md). |
| **MCP** | `strata mcp` serves the graph to an AI agent (hot-reloads on re-index). See the [MCP reference](../reference/mcp.md). |
| **Desktop** | A graph view with contract-aware context buckets and in-app re-indexing. See [Desktop](../reference/desktop.md). |
| **Agent kit** | `strata init claude\|kiro` installs the MCP server, steering, skills, and scoped hooks. See [The agent kit](../reference/agent-kit.md). |

## The one-line summary

StrataGraph reads **five languages** across **four deterministic planes** (code,
contract, infra, data; knowledge is future), grading every relationship into a
confidence band and never claiming more than the evidence supports. All five are
first-class, with the same structure, tools, and discipline; TypeScript
**additionally** carries a compiler-verified `Resolved` tier (SCIP), while
Python, Rust, and C# use band-disciplined heuristics, measured at ~1.0 precision
against compiler ground truth for Python and Rust and extraction-validated for
C#. The deferrals (gRPC code linking, IAM permission-gap reconciliation (the
`Grants` half ships; the `RequiresPermission` half and the traversal do not yet),
convention/Drizzle/Prisma/EF ORM, column-level data, the knowledge plane) are
stated plainly because *stating the edge of what is known* is the product.
