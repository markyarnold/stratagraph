# The five planes

A *plane* is one layer of your system that StrataGraph can read: source code, the
contracts between services, the cloud infrastructure that runs them, the data
they persist, and (in the future) the human knowledge that describes them. Each
plane contributes its own nodes and edges to the one
[cross-plane graph](graph.md) and, crucially, forms edges that **cross into**
the other planes. That cross-plane wiring is what makes a blast radius reach from
a database column to the frontend that displays it; see
[Cross-boundary impact](cross-boundary.md) for the chains.

StrataGraph builds whatever planes are present. A plain library repo has only the code
plane. Add an OpenAPI spec and the contract plane appears; add a SAM template and
the infrastructure plane appears; add SQL migrations and the data plane appears.
Impact degrades gracefully: a traversal returns whatever planes exist, and a
graph with no edges of a given kind is byte-identical to one where that plane was
never considered.

This repository, for reference, has the code, contract, and infra planes present.
Each section below states exactly **what it ingests**, **what nodes and edges it
creates**, and **the cross-plane links it forms**.

## Code plane

The structural skeleton of your source, per language. This is the one plane
present in essentially every repository, and the foundation the others attach to.

**Ingests.** Source files in the five supported languages: TypeScript/JavaScript
(one grammar), Python, C#, and Rust, parsed with Tree-sitter. Vendored and build
directories (`node_modules`, `target`, vendored Python, …) are skipped.

**Creates.**

- Nodes: `Repo`, `Package` (external dependency), `Module` (a source file's
  top-level scope), `Class`, `Interface`, `Function`, `Method`.
- Edges: `Defines`/`MemberOf` (structural containment, always `Extracted` 1.0),
  `Imports` (module → module or external package), and `Calls` (the resolved
  call graph).

**How calls resolve: the band-disciplined heart of the plane.** Each language
has its own pure, deterministic linker that binds a call site to its target
*within that language's resolution world*, grading every edge by how it was
derived:

- A call resolved by a **compiler-grade indexer** (SCIP) is `Resolved`,
  confidence `0.97`. Today this applies to **TypeScript/JavaScript** (via
  `scip-typescript`); a SCIP hit supersedes the heuristic edge for that site.
- A **same-file / same-module** binding (the strongest static signal without a
  compiler) is `Extracted` at the band floor, `0.95`.
- A confident **heuristic** binding (`this.`/`self.` to an enclosing-type
  method, an import-matched name, a unique repo-wide name, a `Type::method()`
  qualifier in Rust) is `Inferred`, capped at the band ceiling `0.80`.
- An **ambiguous** binding (several same-named candidates, an unknown-receiver
  method call) fans out to *all* candidates at `Ambiguous`, strictly below
  `0.40` (e.g. `0.35`).

The discipline is the same in every language: dynamic dispatch is never guessed
(a Python `getattr(...)()`, a Rust macro, a C# `mi.Invoke()`, a JS computed
member) is dropped or kept only at the Ambiguous band, never presented as a
confident call. The exact `CONF_*` constants and the reasoning behind each cap
are in [Confidence and provenance](confidence.md).

**Cross-plane links.** The code plane is the anchor every other plane bridges
*to*: contract `Produces`/`Consumes` edges attach to its `Function`/`Module`
nodes; an infra `Runs` edge bridges a `LambdaFn` to a code `Module`; data
`Reads`/`Writes`/`MapsTo` edges originate at a code symbol. Within the code plane
itself, linking is bounded to one language per slice: a TS function and a Python
function are not call-linked across the language boundary (they meet *through* the
contract or infra plane instead).

## Contract plane

The interfaces between components: the deterministic, cross-repo glue. Where the
code plane stops at a repo boundary, the contract plane carries impact *across*
it, because a contract names the same operation on both sides.

**Ingests.** Interface artifacts: **OpenAPI/Swagger** (`.yaml`/`.json`),
**GraphQL SDL** (including AppSync schemas), and **gRPC** `.proto` files. On the
consumer side it also reads signals from code: outgoing HTTP calls
(`fetch`/`axios`/`got`/`ky`/`superagent` in TS/JS; `requests`/`httpx` and
`aiohttp`'s module forms in Python), ordinary calls whose name matches an
`operationId`, and GraphQL documents (tagged or untagged) parsed from string
literals.

**Creates.**

- Nodes: `ApiOperation` (one OpenAPI path+method **or** one gRPC rpc; a gRPC rpc
  *is* an API operation, so no separate kind) and `GraphqlField` (a GraphQL root
  field such as `Query.getUser`). Operation nodes are `Extracted` 1.0: they come
  straight from the spec.
- Edges:
  - `Produces` (handler code → the operation it implements). A route does not
    *name* the operation it implements; the link is a method+normalized-path (or
    `type.field`) convention match, so a single match is `Inferred` `0.80`,
    several candidates are `Ambiguous` `0.35`, and **no match yields no edge**
    (the route implements something the spec does not declare, surfaced by its
    absence).
  - `Consumes` (calling code → the operation it calls). The tier reflects the
    signal's strength: a unique GraphQL document match is `Extracted` `0.95` (the
    document *names* the contract in its own language); a REST name/URL match is
    `Inferred` (≤ `0.80`); an over-broad match is `Ambiguous` `0.35`.

**Cross-plane links.** The contract plane is the bridge between two code regions
that never call each other directly: producer code `—Produces→` an operation
`←Consumes—` consumer code. `impact` walks producer → operation → consumer in one
hop (`include_contracts`, on by default), which is how changing a backend handler
reaches a frontend in another repo. The infra plane feeds in here too: the
AppSync resolver "money link" emits a `Produces` edge into a `GraphqlField`, so an
IAM-role change can reach a GraphQL consumer through the contract plane. Detailed
chains are in [Cross-boundary impact](cross-boundary.md); the measured linking
accuracy is in `docs/accuracy/openapi-linking.md`, `graphql-linking.md`, and
`grpc-linking.md`.

## Infrastructure plane

The cloud resources that run your code, and the wiring between them. Built only
when IaC is detected, and provider-agnostic by design: the AWS adapter is the
first and richest.

**Ingests.** **CloudFormation** (JSON/YAML) and **SAM** `template.yaml` (the SAM
shorthand is expanded to its real resources), and **Terraform**/**Terragrunt**
(static HCL parsing, with `terraform show -json` plan ingestion as the preferred
resolved source where available). Every reference is graded from its source form
(a same-template `Ref`/`GetAtt` is a fact; a `Sub`/`Join`-recovered or `Fn::If`
id is an inference).

**Creates.**

- Nodes: `LambdaFn`, `IamRole`, `AppSyncApi`, `AppSyncResolver`,
  `AppSyncDataSource`, `CloudResource` (the inventory catch-all: a queue,
  bucket, table, or a Terragrunt unit), and `CloudAction` (one IAM action a role
  grants, e.g. `dynamodb:PutItem`, a wildcard `dynamodb:*`, or an `<opaque:…>`
  indeterminacy marker; Track D2). All `Extracted` 1.0.
- Edges:
  - `Assumes` (a Lambda/datasource → the `IamRole` it assumes, from a `Role`
    reference).
  - `Routes` (the AppSync `resolver → datasource → Lambda` chain; **and**
    structural Terragrunt unit→unit dependencies).
  - `Runs` (a `LambdaFn` → the code `Module` its `CodeUri`+`Handler` resolves to:
    the infra→code bridge).
  - `Contains` (an `AppSyncApi` → the resolvers/datasources it owns, from their
    `ApiId`). This is *membership*, not a dependency: it surfaces in `context`
    but `impact` does not traverse it.
  - `Produces` (the AppSync "money link": a resolver chain → the `GraphqlField`
    it implements).
  - `Grants` (an `IamRole` → each `CloudAction` it grants, from inline and
    managed IAM policies in CloudFormation/SAM and Terraform; Track D2). A grant
    that cannot be enumerated (a managed-policy ARN, a `Deny`, a `NotAction`, a
    `data`-source or non-literal policy document, a policy attachment) is recorded
    as an `<opaque:…>` `CloudAction` that marks the role INDETERMINATE, never
    silently treated as "grants nothing".

  Reference edges are graded honestly: a `Resource`-graded reference is
  `Extracted` `0.95`; a `Sub`/`Join`/`Fn::If`-recovered one is `Inferred` `0.70`
  (and a `Fn::If` over distinct targets emits one edge *per* branch, both
  possible deployments surfaced); an unresolved reference (a parameter,
  cross-stack import, dynamic ARN) produces **no edge**, counted but never
  invented.

**Cross-plane links.** This plane is where StrataGraph's reach is widest. `Runs`
bridges infra **into the code plane**; the money-link `Produces` bridges infra
**into the contract plane**; and because `impact` follows `Assumes`/`Routes`/`Runs`
as dependency edges (`include_infra`, on by default), a single `impact(role)`
reaches the Lambdas that assume the role, then (via the contract hop off each
Lambda) the operations they produce and the frontends that consume them. That
"change an IAM role, find dependent compute and its reach" chain is the headline
example in [Cross-boundary impact](cross-boundary.md); measured numbers are in
`docs/accuracy/infra-linking.md` and `terraform-linking.md`.

> **Honest bounds.** A C# or Rust Lambda's `Handler` is a built-assembly /
> binary-name reference, not a source path, so its `Runs` bridge is deferred (the
> code `Module` nodes still exist: only the infra→code edge is missing, counted
> as unresolved). IAM permission-gap detection is **half-built**: the `Grants`
> side ships (above), but the `RequiresPermission` side (AWS SDK call detection)
> and the `permission_gap` reconciliation are still on the roadmap, so gap
> detection itself is not available yet. AsyncAPI/event sources are not built.
> [Coverage](coverage.md) lists every deferral; the
> [roadmap](../project/roadmap.md) shows the order of what is coming.

## Data plane

The database schema, and the code that touches it. Built when SQL DDL is present.

**Ingests.** SQL schema files (`CREATE TABLE` statements and their cumulative
`ALTER`s) parsed with `sqlparser`. From code, it ingests raw-SQL string literals
(captured by the language analyzers) and **ORM model hints** with an explicit
table name (a Python SQLAlchemy `__tablename__ = "…"`, a Django `class Meta:
db_table = "…"`, a TypeScript TypeORM `@Entity("…")`).

**Creates.**

- Nodes: `Table` and `Column`. Both `Extracted` 1.0. A column's fqn is
  `table.column`, so the same column name in two tables stays distinct.
- Edges (every one an `Extracted` fact at `0.95`):
  - `HasColumn` (a `Table` → each of its `Column`s, containment).
  - `ForeignKey` (a `Column` → the `Column` it references, from an explicit
    `REFERENCES`/`FOREIGN KEY`).
  - `Reads` / `Writes` (a code symbol → a `Table` it reads or writes, parsed from
    a `SELECT … FROM t` / `INSERT INTO t` / `UPDATE t` / `DELETE FROM t` literal).
  - `MapsTo` (an ORM model class → the `Table` it maps to).

  The plane is rigorously honest about misses: an edge is emitted **only** when
  the referenced table/column matches a *declared* node. A foreign key, read,
  write, or ORM mapping that names a table the parsed DDL never declares produces
  **no edge**: it is counted (`*_unresolved`) and surfaced, never invented.
  Dynamic SQL (concatenated/interpolated, not a single literal) is not captured,
  so it contributes no edge either.

**Cross-plane links.** `Reads`/`Writes`/`MapsTo` all originate at a **code-plane**
node (the enclosing function/method, or the file module). Because `impact`
reverse-walks them, `impact(table)` reaches every function that reads or writes
it and every ORM model mapped to it, and, transitively through ordinary `Calls`
edges, the code that uses those functions and models. This is the "change an RDS
column, find dependent services" scenario. Measured linking accuracy is in
`docs/accuracy/data-linking.md`.

> **Honest bounds (real product positioning, not gaps to hide).** The data plane
> links the **explicit**: declared DDL, raw-SQL literals, and explicit-name ORM
> mappings. **Convention-derived** ORM table names (guessing the table from the
> class name, no literal), **Drizzle**/**Prisma**/**C# Entity Framework** ORM
> dialects, and **column-level** read/write granularity are deliberately deferred
> (the design's static-first-with-honest-bounds stance; runtime query-log
> ingestion is a future runtime-observation enhancement). See [Coverage](coverage.md).

## Knowledge plane (future: not built)

The fifth plane in the design is the **knowledge plane**: an opt-in LLM and vision
pass over ADRs, design documents, PDFs, and architecture diagrams, attaching
`Document`, `Decision`, and `Concept` nodes with `MODEL` provenance.

**This plane is not built.** It is described here for completeness because the
manual and the design document (`docs/strata-design.md`) refer to all five
planes. When it lands, its two defining properties are already specified:

- Its nodes/edges carry `MODEL` provenance and are **visually and structurally
  segregated** from the deterministic graph.
- `MODEL` provenance **never gates impact**: a model-derived edge is surfaced as
  context, never counted as a "will break" dependency. The deterministic
  guarantee of the other four planes is not diluted by the knowledge plane.

Until then, treat any reference to the knowledge plane as forward-looking. The
four deterministic planes above are what StrataGraph reads today.
