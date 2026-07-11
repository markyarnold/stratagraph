# Multi-repo estates

A single repository is rarely the whole story. The Lambda that implements a
GraphQL field, and the frontend in *another repository* that queries it, live in
different checkouts, and a change to the field's contract can break the consumer
across that boundary. An **estate** is StrataGraph's model for this: a set of local
repositories indexed together, with their shared API contracts linked across repo
boundaries so impact follows the dependency wherever it lives.

## The workspace manifest

You describe an estate with a `strata.workspace.toml` manifest. The minimal shape
is a name and a list of repos (each `path` is relative to the directory holding
the manifest):

```toml
[workspace]
name = "acme-platform"

[[repos]]
name = "api"
path = "./api"

[[repos]]
name = "web"
path = "./web"
```

That alone gets you a unified estate graph: every repo is indexed into its own
`.strata/graph.duckdb`, the manifest `name` qualifies each repo's node uids so
they stay unique across the estate, and the per-repo graphs are linked into one
graph you can query.

## Cross-repo contract identity

To make two repos share *one real API* (so an operation defined in `api` and
called from `web` collapses to a single canonical node, and impact crosses the
boundary), declare the API identity in each repo with `[[repos.apis]]`:

```toml
[workspace]
name = "acme-platform"

[[repos]]
name = "api"
path = "./api"

  [[repos.apis]]
  id = "orders"
  spec = "schema/orders.graphql"

[[repos]]
name = "web"
path = "./web"

  [[repos.apis]]
  id = "orders"
  spec = "src/generated/orders.graphql"
```

The shared `id` (`"orders"` here) is the link: declaring the **same** `id` in two
repos tells StrataGraph their specs are the same API, so its operations merge into one
canonical contract node. `spec` is the repo-relative path to the spec file that
defines that API in each repo. (An `id` must be a slug, lowercase letters,
digits, and dashes, because it composes into the canonical node identity.)

A manifest with no `apis` (the first example) still works unchanged: each repo's
operations simply default to being identified by that repo. The `[[repos.apis]]`
block is the explicit opt-in for the cross-repo merge: StrataGraph will not silently
assume two repos share an API.

Manifest validation also rejects **duplicate repo paths** (lexically normalized,
so `svc` and `./svc` are the same directory): two entries indexing one directory
would overwrite each other's graph and estate marker, silently losing a declared
identity — the manifest fails to parse instead, before any damage.

## Index and serve the estate

Both the indexer and the MCP server take `--workspace` pointing at the manifest:

```bash
# Index every repo in the estate.
strata index --workspace strata.workspace.toml

# Serve the linked estate graph over MCP.
strata mcp --workspace strata.workspace.toml
```

`strata index --workspace` reports per-repo results and an estate total:

```text
Indexed estate 'acme-platform' (2 repos)
  [ok] api: 1840 nodes, 12044 edges
  [ok] web: 2310 nodes, 18903 edges
  total: 4150 nodes, 30947 edges
```

If one repo fails to index, it is reported `[FAIL]` and the others still index;
an estate degrades gracefully rather than failing wholesale. (Only an estate
where *every* repo failed is treated as an error, because serving an empty graph
silently would be a lie.)

The read commands take `--workspace` too, so you can query across the estate from
the command line:

```bash
strata impact "Query.placeOrder" --workspace strata.workspace.toml
strata context "Query.placeOrder" --workspace strata.workspace.toml
```

## How cross-repo impact works

Once the contract identity is declared, an operation defined in one repo and
consumed in another is a single node in the estate graph with edges reaching into
both. So `impact` on a producer in `api` surfaces the consumer in `web`: the walk
follows producer → operation → consumer across the contract plane, and that path
crosses the repo boundary because the operation node is shared. This is exactly
the cross-boundary dependency a grep in either repo alone cannot see, and StrataGraph
flags when a result crosses a repo boundary so you treat it with the extra care it
deserves.

The MCP server hot-reloads an estate just like a single repo: a change to the
manifest, or to any repo's index, swaps in a freshly-linked estate graph before
the next request, degrade-safe, with no restart after the first.

## The agent kit in an estate

When you run `strata index --workspace <manifest>`, StrataGraph writes a small
membership marker, `.strata/estate.toml`, inside **each member repo** as it
indexes it. The marker records the manifest path, the estate name, and that
repo's declared name. It is written atomically alongside the repo's graph DB, so
it is always consistent with the index.

That marker is what makes the agent kit estate-aware at runtime. Once a repo has
been enrolled, all of the following resolve the estate graph automatically when
you work inside that repo, with no extra flags:

- **Pre-edit blast hook** (`Edit | Write | MultiEdit`): reads the marker and
  computes the blast radius against the full estate graph, so a change to a
  producer surfaces the consumer in another repo before the edit lands.
- **`strata detect-changes`**: aggregates the changed symbols across the estate
  rather than the single repo, so a pre-commit scope check covers cross-repo
  dependents.
- **`strata index <member>`**: reindexes the repo using its estate-qualified
  identity and keeps the marker current, so the post-commit reindex hook keeps
  the estate fresh without re-running `--workspace`.
- **`strata mcp`** (bare, no flags): serves the linked estate graph, not just
  the local repo's DB. The MCP server an agent uses is therefore estate-aware
  from the first session.

### Workflow

Index the estate once, then open any member repo and run `strata init claude`
(or `strata init kiro`) as usual:

```bash
# From the workspace root, once:
strata index --workspace strata.workspace.toml

# Then, from any member repo:
cd api
strata init claude   # or: strata init kiro
```

The hooks and MCP server resolve the estate from the marker at runtime, so
re-running `strata init` after enrolling a repo is not required: any repo
enrolled after `init` was first run is picked up automatically on the next
session. If you do run `strata init` while the marker already exists, it writes
the bare `["mcp"]` estate form directly and refreshes the hooks.

### Explicit overrides

- **`--db <path>`**: pins the command to a single-repo DB, ignoring the marker.
  Use this when you want to query one repo in isolation.
- **`--workspace <manifest>`**: forces estate mode regardless of the marker or
  working directory.

### Honest bound

A repo is estate-aware only **after** `strata index --workspace` has recorded
its marker. Before that, the same commands fall back to single-repo mode
(degrade-safe, never a silent guess). There is no cross-repo impact in
single-repo mode; once the marker exists, the full estate blast radius applies.

`rename` is repo-local for now: renaming a symbol updates references within the
current repo only. Estate-wide rename is a deferred follow-up.

## More

For a full cross-repository walkthrough (declaring identities, reading
cross-boundary impact, and the gotchas), see
[Cross-repository impact](../guides/cross-repo.md).
