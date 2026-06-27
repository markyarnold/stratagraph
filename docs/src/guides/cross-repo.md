# Cross-repository impact

**Goal:** answer "what breaks if I change this?" when the answer lives in a *different repository*. A team changes a GraphQL field in the schema repo; the thing that breaks is a query in the frontend repo. Within one repo, no tool (StrataGraph included) can see that. Index the two repos as an **estate** and the link appears: `impact` on the field reaches the consumer across the repo boundary.

This guide walks a real two-repo example end to end. It uses StrataGraph's own cross-repo GraphQL fixture, `crates/strata-index/tests/fixtures/crossrepo_graphql`, so you can reproduce every command.

## The shape of the example

Two repositories, joined by one GraphQL contract:

- **`repo-schema`**: declares the schema (`Query.getUser`) and implements it. The resolver `getUser` (in `src/resolvers.ts`) is the **producer**. This repo has no consumers; it only serves the API.
- **`repo-app`**: has no schema of its own. It **consumes** the API: `loadUserProfile` (in `src/queries.ts`) runs a `gql` query for `getUser`.

In plain code terms there is *no* edge between these repos: different packages, no shared import. The link is the contract: `getUser` resolver → `Query.getUser` operation → `loadUserProfile` query.

## Steps

### 1. Write a workspace manifest

An estate is declared by a `strata.workspace.toml` listing the repos. The minimal form:

```toml
[workspace]
name = "gql-estate"

[[repos]]
name = "repo-schema"
path = "repo-schema"

[[repos]]
name = "repo-app"
path = "repo-app"
```

`path` is relative to the manifest. (This is the actual manifest in the fixture.)

### 2. Index the estate

```console
$ strata index --workspace strata.workspace.toml
Indexed estate 'gql-estate' (2 repos)
  [ok] repo-schema: 10 nodes, 9 edges
  [ok] repo-app: 14 nodes, 21 edges
  total: 24 nodes, 30 edges
```

Each repo is indexed, then a cross-repo link pass connects producers in one repo to consumers in another over the contract plane.

### 3. Run impact on the contract operation

Every estate command takes `--workspace` instead of `--db`. Run `impact` on the resolver:

```console
$ strata impact getUser --workspace strata.workspace.toml
Impact of getUser (src/resolvers.ts) — 2 affected:
  depth  conf  amb  verdict     name (path)
      1  0.80   no  WILL BREAK  QUERY getUser (getUser)
      2  0.76   no  WILL BREAK  loadUserProfile (src/queries.ts)
```

There it is. Changing the `getUser` resolver in `repo-schema` reaches **`loadUserProfile` in `repo-app`** (a different repository) at depth 2, through the operation node `Query.getUser` at depth 1. That d=2 row is the cross-repo blast radius. (`src/queries.ts` is repo-app's file; the estate view shows repo-relative paths.)

## How to read it

### The boundary is the contract, not the code

Turn the contract plane off and the cross-repo dependent vanishes:

```console
$ strata impact getUser --no-contracts --workspace strata.workspace.toml
Impact of getUser (src/resolvers.ts) — 0 affected:
  (nothing depends on this within the given depth/confidence)
```

Code-only, the resolver looks dead. That contrast is the whole value: the dependent is reachable **only** through the contract plane, which is exactly the kind of link a per-repo grep (or a per-repo tool) physically cannot follow. With contracts on (the default), it's a confident d=2 break.

### See both sides at once with context

`context` on the operation node shows the producer and the consumer together, regardless of which repo each is in:

```console
$ strata context Query.getUser --workspace strata.workspace.toml
Context for QUERY getUser (GraphqlField) — getUser
  uid: contract|gql-estate|repo-schema/graphql|Query.getUser|
  producers (1):
    - getUser (src/resolvers.ts)
  consumers (1):
    - loadUserProfile (src/queries.ts)
  ...
```

One implementer, one consumer, two repos.

## Canonical API identity

Look at that uid: `contract|gql-estate|repo-schema/graphql|Query.getUser|`. The operation has **one canonical identity** in the estate. The consumer's `gql` query in `repo-app` doesn't define its own copy of `getUser`; the link pass resolves it to the *same* canonical operation node the producer implements. That's why a single `impact` spans both repos: there is one node for `Query.getUser`, with edges reaching into each repo.

For HTTP/OpenAPI and multi-repo APIs, you control how identity is assigned. By default each operation's `api_id` is its repo name. When two repos genuinely share one API (say a producer and a separate gateway both describe the same OpenAPI spec) declare a shared `id` so their operations **merge** into one canonical node:

```toml
[[repos]]
name = "repo-producer"
path = "repo-producer"
  [[repos.apis]]
  id   = "user-api"
  spec = "openapi.yaml"

[[repos]]
name = "repo-gateway"
path = "repo-gateway"
  [[repos.apis]]
  id   = "user-api"          # same id → operations collapse to one canonical node
  spec = "gateway.yaml"
```

The same `id` across repos is the merge feature: it collapses the two descriptions of one real API to a single operation, so impact crosses the boundary cleanly instead of treating them as two unrelated endpoints. (An `id` must be slug-safe: lowercase ascii, digits, dashes, because it composes into the canonical uid.) GraphQL needs none of this: a root field's `Type.field` name is already canonical, as the worked example shows.

## When to pause

Cross-repo impact is precisely the case the steering rules single out for caution:

> MUST warn and pause for direction when the blast radius crosses a repo boundary (estate), or touches contract surface consumed by another plane.

A change that looks local (one resolver, one schema field) can ship a break to a consumer owned by another team in another repo. When a d≥2 dependent sits in a different repository, report it and get direction before editing.

## The agent kit is estate-aware

Once a repo has been enrolled in an estate via `strata index --workspace`, the
agent kit (hooks and MCP server) resolves the estate automatically when you work
inside that repo: the pre-edit blast hook surfaces cross-repo dependents before
each edit, `detect-changes` aggregates impact across the estate before each
commit, and the MCP server serves the linked estate graph from the first session.
You do not need to pass `--workspace` on each command or re-run `strata init`
after enrollment. For the full setup details, see
[The agent kit in an estate](../getting-started/estates.md#the-agent-kit-in-an-estate).

## Notes

- All four read tools (`query`, `context`, `impact`, `explain`) accept `--workspace <manifest>` and operate over the estate graph. `--workspace` and `--db` are mutually exclusive.
- The estate graph reloads the same way a single repo's does: re-running `strata index --workspace …`, or editing a single repo, is picked up on the next tool call (see [Pre-edit blast checks](pre-edit-blast.md) for the reload behavior).
- Setup details for estates are in [Multi-repo estates](../getting-started/estates.md); the manifest format in full is in [Configuration](../reference/configuration.md).

## What to do next

- The mechanics of why contract edges cross repos: [Cross-boundary impact](../concepts/cross-boundary.md).
- One worked walkthrough per plane (including the cross-repo infra and data cases): [Plane walkthroughs](plane-walkthroughs.md).
- Reading the verdict and confidence columns: [What breaks if I change this?](impact.md).
