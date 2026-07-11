# Cross-boundary impact

This is the capability that sets StrataGraph apart. Every code-intelligence tool can
tell you which functions call a function. StrataGraph tells you what breaks when the
thing you change is an *interface boundary* (a GraphQL field, a database column,
an IAM role) by carrying a blast radius **across planes and across repositories**
in a single traversal. A grep, or a code-only call graph, cannot see these links:
they live in contract, infra, and data edges that no amount of text search will
surface. This page explains the mechanism and walks one concrete chain end to
end.

The foundation is [the cross-plane graph](graph.md): code, contract, infra, and
data nodes in one graph, joined by edges that cross between them. `impact`
reverse-walks that graph from your target over **dependency** edges of *every*
plane at once. Because the planes share the graph, a path can hop from infra to
code to contract to another repo without ever leaving the one traversal. The
rest of this page is the four boundaries impact crosses, then a worked chain that
crosses three of them at once.

## How impact crosses a boundary

`impact` does one reverse walk over a fixed set of dependency edge kinds, then
layers a contract hop on top. The edge kinds it follows by default:

- **always:** `Calls`, and the data edges `ForeignKey`, `HasColumn`,
  `Reads`, `Writes`, `MapsTo`;
- **`include_infra` (default on):** `Assumes`, `Routes`, `Runs`;
- **`Imports` (internal, default off):** an internal `ImpactOptions` field, not a
  CLI flag or MCP argument: import edges are not traversed by the shipped tools;
- **`include_contracts` (default on):** the producer → operation → consumer hop,
  described below.

Confidence multiplies along the path and the result is the *maximum* over all
reaching paths; a path is flagged `ambiguous` if **any** edge on it was ambiguous.
Crucially, **impact never re-grades an edge**: each hop propagates at the edge's
own stored confidence. So the trust you can place in a cross-boundary result is
exactly the product of the trust in each boundary it crossed, and a single
Ambiguous hop taints the whole path's verdict (see
[Confidence and provenance](confidence.md)). The four boundaries:

### 1. The contract boundary: producer → operation → consumer

This is the cross-repo unlock. Two code regions that never call each other
(often in different repositories) are joined through a shared contract operation:

```
producer code  —Produces→  operation/field  ←Consumes—  consumer code
```

When you change a producer, impact hops *outgoing* `Produces` to the operation(s)
it implements, then *incoming* `Consumes` to the consumer code that calls those
operations (and that consumer's own reverse-`Calls` callers, within the remaining
depth). When your target **is** the operation itself (an `ApiOperation` /
`GraphqlField`), impact seeds the walk with the operation directly: its incoming
`Consumes` are the consumers that break, and its incoming `Produces` are the
handlers that must change too. Either way, the consumer can be in another repo,
which is why this is the boundary that makes a microservice estate analysable.

### 2. The data boundary: code ↔ table, model → table

Two edge families cross between the data and code planes, both reverse-walked:

- **`Reads` / `Writes`** join a code symbol to a `Table` it queries (from a raw-SQL
  literal). `impact(table)` reaches every function that reads or writes it.
- **`MapsTo`** joins an ORM model class to the `Table` it maps to. `impact(table)`
  reaches the mapping model, and, because the model node also has incoming
  `Calls`, transitively the code that instantiates and uses it.

`ForeignKey` (`Column → Column`) and `HasColumn` (`Table → Column`) compose with
these: a changed `Column` reaches its owning `Table` via `HasColumn`, and a
referenced `Column` reaches the referencing `Column` via `ForeignKey`. So
`impact(orgs.id)` reaches the columns that FK to it, the table that owns it, and
onward to the code that reads that table, all in one walk.

### 3. The infrastructure boundary: role → compute, compute → handler

The infra plane bridges into both code and contract:

- **`Assumes`** (`compute → IamRole`) is reverse-walked, so `impact(role)` reaches
  every Lambda that assumes it. This is the boundary that made a bare `impact` on
  an IAM role finally useful: the role's dependents were previously invisible.
- **`Routes`** carries the AppSync `resolver → datasource → Lambda` chain (and
  structural Terragrunt unit→unit dependencies).
- **`Runs`** (`LambdaFn → Module`) bridges a Lambda to its handler **code module**,
  so an infra change reaches the code plane and everything reachable from there.

Because the infra hop seeds the contract hop, reaching a Lambda is not the end of
the walk: impact continues from each Lambda through its `Produces` edges into the
contract plane. (One infra edge, `Contains`, is *not* traversed: it is API→member
membership, not a dependency.)

### 4. The repository boundary: estates and canonical contract identity

The first three boundaries cross *planes*. The fourth crosses *repos*, and it
works because of identity. Within one repo, a contract operation is keyed by its
spec path. Across an **estate** (a set of repos declared in one workspace
manifest) the operation's **canonical identity** is `(api_id, format, key)`:

- `key` is the spec-native key (OpenAPI `operationId`, GraphQL `type.field`, gRPC
  `package.Service/Method`);
- `format` keeps a GraphQL `Query.getUser`, an OpenAPI op, and a gRPC method with
  the same key string on distinct nodes;
- `api_id` is the manifest-declared `[[repos.apis]]` id when a declared spec owns
  the operation, else the repo name.

So the same operation produced in repo A and consumed in repo B **collapses to one
node**, and a `Produces` edge from A and a `Consumes` edge from B meet there: the
blast radius crosses the repo boundary. The `api_id` scoping is a safety property,
not just a join: two *unrelated* APIs that happen to share a key (two services
both exposing `GET /health`, two contexts both declaring `Query.getUser`) get
**distinct** canonical nodes, so they never falsely merge. When a consumer's key
genuinely is owned by several APIs, impact emits one **`Ambiguous` 0.35** edge per
owning API, never a single silent confident pick. (This is the design's §4.4
identity story; see [Cross-repository impact](../guides/cross-repo.md).)

## A worked chain: change an IAM role, reach a frontend in another repo

Here is the differentiator made vivid. You are about to modify an IAM role's
permissions. A grep for the role's logical id finds the template that declares it
and nothing else: it cannot tell you a frontend three planes and one repo away
depends on it. `impact` can, because the chain is a path in the graph:

```
IamRole                      (infra plane)        ← you change this
  ↑ Assumes
LambdaFn (PolicyOperations)  (infra plane)        the Lambda that assumes the role
  ↑ Runs
Module (policies handler)    (code plane)          its handler code
  │ Produces
GraphqlField (getPolicyStats)(contract plane)      the field that handler implements
  ↑ Consumes
Module (frontend, repo B)    (contract → code, cross-repo)  the frontend that queries it
  ↑ Calls
renderDashboard              (code plane, repo B)  and its callers
```

`impact(IamRole)` walks this in one pass:

1. **Infra → infra.** Reverse `Assumes` reaches `PolicyOperationsFunction` (the
   Lambda assumes the role). With an `Extracted` `Assumes` edge (`0.95`), this hop
   is a fact.
2. **Infra → code.** Reverse `Runs` reaches the handler `Module` (`CONF_RUNS`
   `0.95`).
3. **Code → contract.** From the Lambda, the contract hop follows *outgoing*
   `Produces` to `getPolicyStats` (the AppSync money link). If the resolver chain
   resolved end to end via `Resource` refs this is `Extracted` `0.95`; if a hop
   was interpolation-recovered it is `Inferred` `0.70`.
4. **Contract → code, across the repo boundary.** Incoming `Consumes` reaches the
   frontend module in **repo B** that queries `getPolicyStats` (the operation is
   one canonical node shared by both repos).
5. **Code → code.** From that consumer, ordinary reverse `Calls` reach
   `renderDashboard` and anything else that calls into it.

The result lists the Lambda, the handler, the field, *and* the frontend in the
other repo, each with a confidence that is the product of the boundaries crossed,
and each labelled WILL BREAK or "review" by that product. `explain IamRole
renderDashboard` then prints the exact five-hop chain above, edge by edge, with
each provenance and the running confidence, so the cross-repo, cross-plane verdict
is auditable, not asserted.

That is the claim no grep and no code-only tool can make: **"changing this IAM
role affects a frontend in another repository, here is the evidence chain, and
here is how much to trust it."** The data-boundary analogue (`impact(column)`
reaching a service's exposed contracts and their downstream consumers) is the
same idea one plane over. Both fall out of one property: every boundary is just an
edge in the one graph, and `impact` walks them all together.

## What to do at a boundary

The trust policy from [Confidence and provenance](confidence.md) applies with
extra force across boundaries, because a cross-plane result is exactly as strong as
its weakest hop:

- **A clean, ≥ 0.90 cross-boundary chain** (all `Extracted`/`Resolved`) is
  WILL BREAK: act on it before you change the target.
- **A chain with an `Inferred` hop** (an interpolated infra ref, a convention-
  matched producer) is still WILL BREAK but earns a look at the source.
- **A chain with any `Ambiguous` hop** (including the `Ambiguous` fan-out when
  an operation key is owned by several APIs — across repos, or by several spec
  files within one repo) is surfaced so you do not miss it,
  but its verdict is "review", and you must **treat it as UNKNOWN, not certain**.

And the cross-boundary cases are precisely the ones to **pause on**: when a blast
radius crosses a repo boundary, or touches contract surface another plane
consumes, report it and get direction before proceeding. That is the whole reason
the boundary-crossing reach exists: to make the invisible dependency visible
*before* the change, not after the incident.
