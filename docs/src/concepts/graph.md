# The cross-plane graph

Everything StrataGraph does (`query`, `context`, `impact`, `explain`,
`detect_changes`, `rename`, `blast`) reads one structure: a single directed
graph of your whole estate. A node is a thing that exists in your system (a
function, a database table, an API operation, a Lambda). An edge is a verified
relationship between two of those things (this function *calls* that one; this
Lambda *runs* that handler; this code *reads* that table). This page is the
canonical description of that model: what a node is, what an edge is, how each is
named, and how a repository on disk becomes a graph you can traverse.

The graph is *cross-plane*: code, contract, infrastructure, and data nodes live
in the **same** graph, joined by edges that cross between them. That is the whole
point: it is what lets `impact` walk from an IAM role to the Lambda that assumes
it to the GraphQL field it produces to the frontend that queries it, in one pass.
The planes themselves are covered in [The five planes](planes.md); this page is
the shared model underneath them.

## Nodes

A node is one identifiable entity. Every node carries the same fields (defined in
`strata-core`'s `Node`):

| Field | Meaning |
| --- | --- |
| `uid` | The deterministic, unique identity (see [UIDs](#the-uid-scheme) below). |
| `kind` | A `NodeKind`: what sort of thing this is. |
| `name` | The short display name (`getUser`, `orgs`, `GET /users`). |
| `fqn` | The fully-qualified name within its file/spec (`UserService.getUser`, `orgs.id`). |
| `path` | The repo-relative file the node came from. |
| `span` | The line/column range in that file (`0` for synthesised nodes). |
| `provenance` | How the node was derived (see [Confidence and provenance](confidence.md)). |
| `confidence` | How much to trust it, `0.0`–`1.0`. |

### Node kinds

The `NodeKind` vocabulary is small and deliberately generic: the graph and the
traversal handle every kind the same way (there are no per-kind special cases in
`impact`), so adding a kind is additive. The kinds, grouped by the plane they
belong to:

| Plane | `NodeKind`s | Represents |
| --- | --- | --- |
| Code | `Repo`, `Package`, `File`, `Module`, `Class`, `Interface`, `Function`, `Method` | The structural skeleton of source code. A `Module` is a source file's top-level scope; `Package` is an external dependency. |
| Contract | `ApiOperation`, `GraphqlField` | One operation in an interface contract. `ApiOperation` covers OpenAPI paths **and** gRPC rpcs; `GraphqlField` is a GraphQL root field (`Query.getUser`). |
| Infrastructure | `LambdaFn`, `IamRole`, `AppSyncApi`, `AppSyncResolver`, `AppSyncDataSource`, `CloudResource` | AWS resources from CloudFormation/SAM/Terraform. `CloudResource` is the catch-all inventory kind (a queue, bucket, table, Terragrunt unit). |
| Data | `Table`, `Column` | A database table and one of its columns, from SQL DDL. |

> The authoritative, machine-readable list lives in the MCP `graph_schema_json`
> output (the `node_kinds` array) and in `strata-core/src/model.rs`. The
> [Graph schema reference](../reference/schema.md) reproduces it. The design
> document (`docs/strata-design.md` §4.2) describes a broader target vocabulary
> (`EventSchema`, `RdsInstance`, `Database`, `Decision`, …); this table is what
> is **built today**. [Languages and coverage](coverage.md) is scrupulous about
> the difference.

## Edges

An edge is a directed relationship `src → dst`, with its own `kind`,
`provenance`, and `confidence` (an edge is graded independently of its
endpoints). Edge confidence is the substance of an impact result: when `impact`
walks a path it multiplies the confidence of every edge it crosses, so the
edges' grades (not the nodes') decide whether a dependent is reported as
**WILL BREAK** or "may be affected, review".

### Edge kinds

| Plane | `EdgeKind` | `src → dst` | Meaning |
| --- | --- | --- | --- |
| Code | `Defines` | container → member | A module/class defines this symbol. |
| Code | `MemberOf` | member → container | The reciprocal of `Defines`. |
| Code | `Imports` | module → module/package | An import/`use`/`using`. |
| Code | `Calls` | caller → callee | A function/method call. The workhorse code edge. |
| Code | `Extends`, `Implements` | n/a | **Reserved, not emitted by any analyzer yet.** Defined for stability; no extractor produces them and `impact` never traverses one. |
| Contract | `Produces` | handler → operation | This code implements this operation/field. |
| Contract | `Consumes` | caller → operation | This code calls this operation/field. |
| Infra | `Assumes` | compute → `IamRole` | A Lambda assumes an execution role. |
| Infra | `Runs` | `LambdaFn` → `Module` | A Lambda's handler resolves to this code module (the infra→code bridge). |
| Infra | `Routes` | resolver → datasource → Lambda; Terragrunt unit → unit | Wiring along the AppSync resolver chain, or a structural Terragrunt dependency. |
| Infra | `Contains` | `AppSyncApi` → resolver/datasource | Membership, **not** a dependency: `impact` deliberately does not traverse it. |
| Data | `HasColumn` | `Table` → `Column` | Containment. `impact` reaches a table *from* a changed column, but does not re-list a table's columns. |
| Data | `ForeignKey` | `Column` → `Column` | An explicit `REFERENCES` / `FOREIGN KEY`. A dependency edge. |
| Data | `Reads` / `Writes` | code → `Table` | A raw-SQL `SELECT`/`INSERT`/`UPDATE`/`DELETE` literal in code touching a declared table. |
| Data | `MapsTo` | ORM model class → `Table` | An explicit ORM table-name declaration (`__tablename__`, `@Entity("…")`, Django `db_table`). |

Two distinctions matter for reading an impact result correctly:

- **Dependency edges vs. membership edges.** `impact` reverse-walks *dependency*
  edges only: `Calls`, `Imports` (when enabled), `Produces`/`Consumes`,
  `Assumes`/`Routes`/`Runs`, `ForeignKey`, `Reads`/`Writes`, `MapsTo`, and
  `HasColumn`. It deliberately does **not** traverse `Contains` (changing an API
  container is not changing a resolver). `Defines`/`MemberOf` light up
  `context`'s `members`/`container` buckets but are not a blast-radius path.
- **Direction is the dependency direction.** Edges point the way the dependency
  runs, and `impact` walks them *backwards* from the target. `accounts.org_id
  —ForeignKey→ orgs.id` means `accounts.org_id` depends on `orgs.id`, so
  `impact(orgs.id)` reaches `accounts.org_id`. A model `—MapsTo→` a table means
  the model breaks when the table changes, so `impact(table)` reaches the model.

## The UID scheme

Cross-plane and cross-repo linking only works if the same thing has the same
identity everywhere. Every node's `uid` is a deterministic string of **five
pipe-delimited fields** (`strata-core/src/ids.rs`):

```
{language}|{package}|{path}|{fqn}|{signature}
```

The same five inputs always produce the same UID, so an index is reproducible and
two planes referring to the same entity land on the same node. The first field is
overloaded as a **plane/language tag**, which keeps a Python `getUser` and a
contract operation of the same name from ever colliding:

| First field | Plane | Example UID |
| --- | --- | --- |
| `ts` / `py` / `rust` / `cs` | Code | `rust\|strata\|crates/strata-core/src/model.rs\|EdgeKind\|` |
| `contract` | Contract (per-repo) | `contract\|myrepo\|openapi/users.yaml\|getUser\|` |
| `contract` | Contract (estate-canonical) | `contract\|myestate\|users-api/openapi\|getUser\|` |
| `infra` | Infrastructure | `infra\|myrepo\|template.yaml\|PolicyOperationsFunction\|` |
| `data` | Data (table) | `data\|myrepo\|schema.sql\|orgs\|` |
| `data` | Data (column) | `data\|myrepo\|schema.sql\|orgs.id\|` |

Notes that make the scheme work:

- **Code** UIDs use the language tag, the repo name as the package, the file
  path, the symbol's fqn, and (reserved) a signature slot. A file's `Module`
  node uses the literal `<module>` in the fqn slot.
- **Data columns** carry `table.column` in the fqn slot, so the same column name
  in two tables stays distinct (`orgs.id` vs `accounts.id`).
- **Contracts** are special, because the cross-repo unlock depends on it. A
  per-repo operation is keyed by its spec path; the **estate-canonical** identity
  is keyed by `{api_id}/{format}` (no spec path), so the same operation collapses
  to one node across repos, while two *unrelated* APIs that share a key string
  get distinct nodes. This `(api_id, format, key)` identity is the subject of
  [Cross-boundary impact](cross-boundary.md) and the design doc's §4.4.

The current implementation renders the UID as a readable key string. Per
`ids.rs`, a compact hash or SCIP monikers could replace the *internals* later
without changing the type's interface, so do not parse UIDs in your own
tooling; treat them as opaque handles obtained from `query`/`context`.

## How a repository becomes a graph

Indexing (`strata index`) is a pipeline. Each stage is pure and deterministic
(no stage does anything random or order-dependent), so the same repository always
produces the same graph, which is what makes the accuracy numbers in
[Measured results](../accuracy/results.md) reproducible.

1. **Collect.** Walk the file tree, skipping vendored/build directories
   (`node_modules`, `target`, `.git`, vendored Python, …). Classify each file:
   source (by extension → language), interface spec (OpenAPI/GraphQL/`.proto`),
   IaC (CloudFormation/SAM/Terraform/Terragrunt), or SQL DDL. Content is hashed
   for incremental re-indexing.
2. **Analyze.** Parse each file into facts, *without* resolving anything
   cross-file. The code analyzers are Tree-sitter based (one per language) and
   emit symbols, imports, call sites, route declarations, GraphQL resolver
   entries, raw-SQL literals, and ORM hints. The contract/infra/data adapters
   parse their artifacts into operation, resource, and schema definitions. This
   stage invents nothing: a callee it cannot name is dropped, not guessed.
3. **Link.** Turn the per-file facts into the cross-file, cross-plane graph. This
   is where resolution and confidence grading happen:
   - the **code plane** resolves imports to modules and call sites to target
     symbols: precisely via SCIP where available (TypeScript today), else with
     band-disciplined heuristics;
   - the **contract plane** matches route/resolver handlers to operations
     (`Produces`) and call signals to operations (`Consumes`);
   - the **infrastructure plane** wires resources (`Assumes`/`Routes`), bridges
     Lambdas to code modules (`Runs`), and adds the AppSync resolver→field
     "money link" (`Produces`);
   - the **data plane** builds tables/columns, foreign keys, code→table
     `Reads`/`Writes`, and ORM `MapsTo` edges.

   The planes are layered in a fixed order (code, then contract, then infra,
   then data), so each later plane can attach to the nodes the earlier ones
   created (an infra `Runs` edge needs the code `Module` to already exist; a data
   `Reads` edge needs the enclosing function).
4. **Store.** Persist the assembled graph to `.strata/graph.duckdb`. Storage is
   deliberately a *swappable* substrate: the reliability-critical traversals
   (`impact`, `context`, `explain`) are owned and tested in `strata-core` over an
   in-memory graph, so correctness never depends on the database engine.
5. **Serve.** Expose the graph to clients: the CLI reads it per-command; `strata
   mcp` serves it to an agent over MCP and hot-reloads when the on-disk index
   changes; the desktop app renders it. All of them call the same `strata-core`
   traversals over the same graph.

You can see the assembled shape directly. On this repository:

```text
$ strata context EdgeKind
Context for EdgeKind (Class) — crates/strata-core/src/model.rs
  uid: rust|strata|crates/strata-core/src/model.rs|EdgeKind|
  container: model.rs (crates/strata-core/src/model.rs)
  members (0):
  callers (0):
  ...
```

That single `context` call reads the node, its `uid` (the five-field scheme), its
container (a `Defines`/`MemberOf` edge), and every relationship bucket (code,
contract, infra, and data) from the one graph. The next pages describe what
fills those buckets ([planes](planes.md)), how much to trust what you find
([confidence](confidence.md)), and how a blast radius crosses between them
([cross-boundary impact](cross-boundary.md)).
