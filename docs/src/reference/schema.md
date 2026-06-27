# Graph schema

The reference for StrataGraph's graph vocabulary: every `NodeKind`, every `EdgeKind`,
the UID format per plane, and how provenance and confidence are represented. This
page states *what* the schema is; for *why* the planes and confidence bands exist,
see [Concepts](../concepts/graph.md).

The canonical source is `crates/strata-core/src/model.rs` (the enums and their
doc-comments). The same vocabulary is published to MCP clients through the
[`strata://schema` resource](mcp.md#the-strataschema-resource), built by
`graph_schema_json` in `crates/strata-mcp/src/tools.rs`; the two are kept in sync.

## Node and edge JSON names

Both enums derive serde, so every kind serialises to its bare variant name (e.g.
`NodeKind::Function` → `"Function"`, `EdgeKind::MapsTo` → `"MapsTo"`). These are
the exact strings that appear in tool payloads, the `strata://schema` resource,
and subgraph DTOs.

## NodeKind

A node is one entity in the graph. Every node carries the fields below
(`strata_core::Node`):

| Field | Type | Meaning |
|---|---|---|
| `uid` | string | The deterministic identity (see [UID format](#uid-format)). |
| `kind` | `NodeKind` | The variant, serialised as its name. |
| `name` | string | The short, human-readable name. |
| `fqn` | string | The fully-qualified name. |
| `path` | string | The source/spec/template path the node was extracted from. |
| `span` | `Span` | `start_line`, `start_col`, `end_line`, `end_col` (all `u32`). |
| `provenance` | `Provenance` | How the node was established (see [Provenance](#provenance)). |
| `confidence` | `Confidence` | A float clamped to `0.0..=1.0` (see [Confidence](#confidence)). |

The 19 node kinds, grouped by plane:

| NodeKind | Plane | Meaning |
|---|---|---|
| `Repo` | code | A repository root. |
| `Package` | code | A package within a repo. |
| `File` | code | A source file. |
| `Module` | code | A module (also the unit a contract operation is consumed from, and the target of an infra `Runs` edge). |
| `Class` | code | A class. |
| `Interface` | code | An interface. |
| `Function` | code | A free function. |
| `Method` | code | A method (a member of a class/interface). |
| `ApiOperation` | contract | An interface-contract operation: an OpenAPI path+method or a gRPC rpc. Produced from a `strata_contract::OperationDef`. |
| `GraphqlField` | contract | A GraphQL root-operation field (`Query.getUser` / `Mutation.createUser` / `Subscription.onPing`). |
| `LambdaFn` | infra | An `AWS::Lambda::Function` / `AWS::Serverless::Function` resource. |
| `IamRole` | infra | An `AWS::IAM::Role` resource. |
| `AppSyncApi` | infra | An `AWS::AppSync::GraphQLApi` resource (inventory; its `ApiId` containment is wired via `Contains`). |
| `AppSyncResolver` | infra | An `AWS::AppSync::Resolver` resource. |
| `AppSyncDataSource` | infra | An `AWS::AppSync::DataSource` resource: the `Routes` waypoint between a resolver and its backing Lambda. |
| `CloudResource` | infra | Any other infrastructure resource (queue, table, bucket, …): logical id + CFN type only. |
| `Table` | data | A database table, from a committed `CREATE TABLE` / cumulative `ALTER`. |
| `Column` | data | A table column. |
| `CloudAction` | infra (security) | One IAM action a role grants: a concrete action `dynamodb:PutItem`, a wildcard pattern `dynamodb:*` / `*`, or an `<opaque:reason>` indeterminacy marker for grants that could not be enumerated. The shared node where `Grants` (role → action) and `RequiresPermission` (code → action) meet. |

The plane of each kind is assigned by `plane_of` in
`apps/strata-desktop/src-tauri/src/subgraph.rs`: the single source of truth for
the plane↔kind mapping. The four planes are `code`, `contract`, `infra`, `data`.

> The graph handles every kind **generically**: there are no per-kind match arms
> in the graph, store, or traversal. New node kinds are therefore additive.

## EdgeKind

An edge is a directed relationship between two nodes (`strata_core::Edge`):

| Field | Type | Meaning |
|---|---|---|
| `src` | `Uid` | The source node. |
| `dst` | `Uid` | The destination node. |
| `kind` | `EdgeKind` | The variant, serialised as its name. |
| `provenance` | `Provenance` | How the edge was established. |
| `confidence` | `Confidence` | A float clamped to `0.0..=1.0`. |

The 19 edge kinds:

| EdgeKind | Direction (src → dst) | Dependency edge? | Meaning |
|---|---|---|---|
| `Defines` | container → defined symbol | structural | A scope defines a symbol. |
| `MemberOf` | member → container | structural | A member belongs to a container. |
| `Imports` | importer → imported | yes | An import relationship. |
| `Calls` | caller → callee | yes | A call relationship. |
| `Extends` | subclass → superclass |  | **Reserved; not yet emitted by any analyzer.** See [reserved edges](#reserved-edges). |
| `Implements` | implementer → interface |  | **Reserved; not yet emitted by any analyzer.** See [reserved edges](#reserved-edges). |
| `Produces` | handler/function → `ApiOperation`/`GraphqlField` | yes (contract hop) | A producer implements an operation/field. |
| `Consumes` | calling function/module → `ApiOperation`/`GraphqlField` | yes (contract hop) | A consumer calls an operation/field. |
| `Assumes` | `LambdaFn` → `IamRole` | yes (infra) | A Lambda assumes the role it references. |
| `Runs` | `LambdaFn` → code `Module` | yes (infra) | A Lambda's resolved handler module (`CodeUri` + `Handler`). |
| `Routes` | `AppSyncResolver` → `AppSyncDataSource`, and `AppSyncDataSource` → `LambdaFn` | yes (infra) | Resolver→datasource→Lambda wiring. |
| `Contains` | `AppSyncApi` → its resolvers/datasources | no (membership) | API containment from `ApiId`. Lights up `context.members`; `impact` does **not** traverse it. |
| `HasColumn` | `Table` → `Column` | no (membership) | Table containment. Lights up `context.members`; a changed `Column` reaches its owning `Table` (incoming reverse walk), but `impact(table)` does not re-list columns. |
| `ForeignKey` | `Column` → referenced `Column` | yes | An explicit `REFERENCES` / table-level `FOREIGN KEY`. |
| `Reads` | code symbol → `Table` | yes | A raw-SQL `SELECT … FROM t` / `JOIN t` read, emitted only when the table name matches a declared `Table` node. |
| `Writes` | code symbol → `Table` | yes | A raw-SQL `INSERT INTO t` / `UPDATE t` / `DELETE FROM t` write, same declared-table condition as `Reads`. |
| `MapsTo` | ORM model class → `Table` | yes | An ORM model maps to its table (SQLAlchemy `__tablename__`, Django `Meta.db_table`, TypeORM `@Entity("…")`). Emitted only when the model's table name matches a declared `Table`. |
| `Grants` | `IamRole` (or a SAM `LambdaFunction` via its implicit execution role) → `CloudAction` | no (reconciliation) | An IAM policy statement `Allow`s a role an action (concrete, a `dynamodb:*`/`*` wildcard, or an `<opaque:reason>` marker). The supply side of IAM permission-gap detection; `impact` does **not** traverse it (it is reconciliation input, not a blast-radius edge). |
| `RequiresPermission` | code `Function`/`Method` → `CloudAction` |  | **Reserved; not yet emitted by any analyzer.** The demand side of IAM permission-gap detection. See [reserved edges](#reserved-edges). |

"Dependency edge?" indicates whether [`impact`](cli.md#impact) reverse-walks the
edge (it walks edges **incoming** to the target, so a dependency edge from
`A → B` means "changing `B` affects `A`"). Structural and membership edges are
not traversed as dependencies. The contract hops (`Produces`/`Consumes`) are
gated by `include_contracts`; the infra hops (`Assumes`/`Routes`/`Runs`) by
`include_infra`.

### Code→table edges and the declared-table condition

`Reads`, `Writes`, and `MapsTo` are emitted **only** when the table name parsed
from code matches a declared `Table` node: they are graded `Extracted` at
confidence `0.95`. An undeclared table yields **no edge** (it is counted as
unresolved, never invented). The source of a `Reads`/`Writes` edge is the
enclosing `Function`/`Method`, falling back to the file `Module` when there is no
enclosing symbol.

### Reserved edges

`Extends` and `Implements` are defined in the enum so its serde round-trip is
stable, but **no extractor emits them today** and `impact` never traverses one
(none exist to traverse). Class-inheritance and interface-implementation edges
are a future enhancement.

`RequiresPermission` is likewise defined but **not yet emitted by any analyzer**.
It is the demand side of IAM permission-gap detection (a code `Function`/`Method`
→ the `CloudAction` it needs, from a detected AWS SDK call): the `Grants` supply
side ships, but the AWS SDK call→action detection that would populate
`RequiresPermission`, and the `permission_gap` traversal that reconciles the two,
are not built yet. When emitted it will be reconciliation input, not a dependency
edge `impact` traverses.

## UID format

A UID is a deterministic, human-readable identity string built by
`Uid::new(language, package, path, fqn, signature)` in
`crates/strata-core/src/ids.rs`, which formats the five fields pipe-separated:

```
{language}|{package}|{path}|{fqn}|{signature}
```

The five slots are filled differently per plane:

| Plane | language | package | path | fqn | signature |
|---|---|---|---|---|---|
| code | the source language (`ts`, `py`, `rust`, `cs`, …) | package/repo name | source file path | the symbol's fully-qualified name | the signature (e.g. `()`); empty for some kinds |
| contract (per-repo) | `contract` | repo name | the spec file path (`spec_path`) | the operation `key` | empty |
| contract (estate-canonical) | `contract` | estate name | `{api_id}/{format}` | the operation `key` | empty |
| infra | `infra` | repo name | template/unit path | the resource logical id | empty |
| data | `data` | repo name | schema file path | the table name | empty |

Notes:

- **Contract, per-repo:** `operation_uid` keys an operation by its cross-repo
  `key` within its spec file, so the same `operationId` in the same spec collapses
  to one node (`crates/strata-index/src/contract.rs`).
- **Contract, estate-canonical:** `canonical_operation_uid` drops the spec path
  and uses the `{api_id}/{format}` discriminator instead, so the same
  `(api_id, format, key)` from any repo in an estate collapses to one canonical
  node, while two unrelated APIs that share a key string land on distinct nodes.
  The `format` token is `openapi`, `graphql`, or `grpc` (`format_discriminator`).
  `api_id` is the manifest-declared `[[repos.apis]]` id when a declared spec owns
  the operation, else the repo name.
- **data:** a `Table` UID is `Uid::new("data", repo, schema_path, table, "")`
  (`table_uid` in `crates/strata-index/src/data.rs`).

The per-plane `language`/`package` tokens are the constants `CONTRACT_LANG =
"contract"`, `INFRA_LANG = "infra"`, and `DATA_LANG = "data"`.

## Provenance

`Provenance` records how a node or edge was established. The six variants:

| Provenance | Meaning |
|---|---|
| `Extracted` | Read directly from source/spec/template syntax. |
| `Resolved` | Established via precise resolution (SCIP). |
| `Observed` | Established from an observed signal. |
| `Inferred` | Inferred heuristically. |
| `Ambiguous` | Established but ambiguous (multiple candidates). |
| `Model` | Established from a model. |

## Confidence

`Confidence` wraps an `f32` clamped to the inclusive range `0.0..=1.0`
(`Confidence::new` clamps; `Confidence::value` reads it back). It appears as a
plain JSON number in tool payloads.

Confidence is the basis of StrataGraph's trust policy and the `will_break` verdict.
The bands an agent applies are documented in
[Concepts → Confidence](../concepts/confidence.md); in short: `≥ 0.90` act on it,
`0.40–0.89` verify in source, `< 0.40` or ambiguous treat as unknown. The
`impact`/`explain` `will_break` field is `true` when `confidence ≥ 0.40` AND the
node is not ambiguous, independent of depth.
