# Is this schema field / endpoint dead?

**Goal:** decide whether a GraphQL field, an API operation, a database table, or an IAM role is actually wired up, or dead weight you can delete. Grep can't answer this: a field is *produced* by a Lambda and *consumed* by a frontend in another file (or another repo), through edges that live in the contract and infra planes, not in the text. `context` reads those edges directly.

The rule is simple and the same across planes: **a contract operation with 0 producers AND 0 consumers is probably dead.** Below it's spelled out for fields, tables, and roles.

## The tool: `context`

`context` gives the 360° view of one symbol: its code neighbors *and* its cross-plane buckets.

- **`producers`**: who implements this field/operation (the resolver or Lambda).
- **`consumers`**: who queries it (the frontend or calling module).
- **`produces` / `consumes`**: the outgoing views, from a Lambda or module's side.
- **`mapped_by` / `maps_to`**: ORM-model ↔ table links (data plane).
- **`assumes` / `assumed_by`, `routes_to` / `routed_from`, `runs` / `run_by`**: infra wiring.

All buckets are always present, so `producers (0) / consumers (0)` is a real, readable answer, not a missing one.

## A live GraphQL field

Run `context` on the operation and read both sides. (These examples use StrataGraph's own cross-repo GraphQL fixture, `crates/strata-index/tests/fixtures/crossrepo_graphql`, indexed as an estate.)

```console
$ strata context Query.getUser --workspace strata.workspace.toml
Context for QUERY getUser (GraphqlField) — getUser
  uid: contract|gql-estate|repo-schema/graphql|Query.getUser|
  producers (1):
    - getUser (src/resolvers.ts)
  consumers (1):
    - loadUserProfile (src/queries.ts)
  produces (0):
  consumes (0):
  ...
```

`getUser` has a producer (the `getUser` resolver in `repo-schema`) and a consumer (`loadUserProfile` in `repo-app`). It's **live, and cross-repo**: changing it touches both sides, in two different repositories. This is not a dead-surface candidate; it's a "report and pause before editing the schema" candidate. (To see the full blast radius, run `impact` on it; the cross-repo case is walked in [Cross-repository impact](cross-repo.md).)

## A dead GraphQL field

Now the same call on a field nothing touches (the symbol below is **illustrative**:
it shows the shape of a dead-surface result, not output from a committed
fixture):

```console
$ strata context getActiveGeneralPolicies
Context for QUERY getActiveGeneralPolicies (GraphqlField) — getActiveGeneralPolicies
  producers (0):
  consumers (0):
  ...
```

**Zero producers AND zero consumers.** No code implements it and no code queries it: it is almost certainly dead schema surface. Flag it to the user as a removal candidate. Do *not* silently assume it's wired up somewhere the graph can't see: the whole point is that the contract plane already covers the links grep would miss, so when both buckets are empty, that's evidence, not absence of evidence.

> One bucket empty is **not** dead. A field with producers (1) and consumers (0) is implemented-but-unused (a candidate to retire the consumer-less endpoint); producers (0) and consumers (1) is consumed-but-unimplemented (likely a real bug: someone queries a field nobody serves). Only **both** zero means dead.

## Confirm with impact

`context` shows the immediate buckets; `impact` confirms there is no *transitive* reach either:

```console
$ strata impact getUser --no-contracts
Impact of getUser (src/resolvers.ts) — 0 affected:
  (nothing depends on this within the given depth/confidence)
```

That's the resolver with the contract plane turned **off**: code-only, it looks dead. Turn the plane back on and the consumer reappears:

```console
$ strata impact getUser
Impact of getUser (src/resolvers.ts) — 2 affected:
  depth  conf  amb  verdict     name (path)
      1  0.80   no  WILL BREAK  QUERY getUser (getUser)
      2  0.76   no  WILL BREAK  loadUserProfile (src/queries.ts)
```

The contrast is the lesson: a real dead-surface verdict survives with the planes **on**. If 0-affected only happens once you've disabled contracts/infra, the surface isn't dead; you just hid the edges that prove it's alive.

## Is this table / column dead?

Same idea, data-plane vocabulary. StrataGraph links code to tables through three edge kinds: reads and writes from SQL string literals, and `MapsTo` from an ORM model's `__tablename__`. (Examples use `crates/strata-index/tests/fixtures/crossrepo_data`.)

```console
$ strata context users --workspace strata.workspace.toml
Context for users (Table) — schema.sql
  uid: data|repo-a|schema.sql|users|
  mapped_by (1):
    - User (models.py)
  members (4):
    - email (schema.sql)
    - id (schema.sql)
    - last_login (schema.sql)
    - org_id (schema.sql)
  ...
```

`users` is mapped by an ORM model (`User`) and has four columns. To see the reading and writing code, run `impact`: data links are surfaced as the table's blast radius:

```console
$ strata impact users --workspace strata.workspace.toml
Impact of users (schema.sql) — 4 affected:
  depth  conf  amb  verdict     name (path)
      1  0.95   no  WILL BREAK  User (models.py)
      1  0.95   no  WILL BREAK  touch_last_login (writer.py)
      1  0.95   no  WILL BREAK  getUserEmail (src/users.ts)
      1  0.95   no  WILL BREAK  listUsersWithOrg (src/users.ts)
```

An ORM model, a Python writer, and two TypeScript readers, across languages. A table with **no** reads, writes, or `mapped_by` is a dead-table candidate. (Run `impact` on a single column to scope this to one field; see [Plane walkthroughs](plane-walkthroughs.md).)

**An honest bound:** data links are *table-level*. StrataGraph links code to a table from raw-SQL literals and the ORM `__tablename__`, but a dynamically built query (an f-string like `f"DELETE FROM {table}"`) is deliberately **not** linked, because the table name isn't a literal. So "no writes found" can mean "only dynamic writes exist." Verify before deleting. This is the never-invent rule: StrataGraph would rather miss a link than fabricate one.

## Is this IAM role dead?

On the infra plane, an unused role shows up as an empty `assumed_by` bucket. A live one names what assumes it (example from `crates/strata-index/tests/fixtures/crossrepo_infra`):

```console
$ strata context UserRole --workspace strata.workspace.toml
Context for UserRole (IamRole) — template.yaml
  uid: infra|repo-a|template.yaml|UserRole|
  assumed_by (1):
    - UserFunction (template.yaml)
  ...
```

`UserRole` is assumed by `UserFunction`: live. A role with `assumed_by (0)` (nothing assumes it) and no other infra edges is an orphaned-role candidate. As with tables, confirm with `impact UserRole` to see the full reach before recommending removal: for a live role that reach can be long (the Lambda that assumes it, the operations it serves, the frontend that consumes them).

## What to do next

- **Both buckets zero → flag as likely dead**, name it, and propose removal. Don't claim it's live.
- **One bucket zero** → report the asymmetry (unused vs unimplemented); it's a different problem.
- **Live and cross-plane / cross-repo** → run `impact`, report producers and consumers, and pause before editing the schema. Never edit a contract file without this check; see the steering rules in [Pre-edit blast checks](pre-edit-blast.md).
- For the mechanics behind producer/consumer and the planes, read [The five planes](../concepts/planes.md) and [Cross-boundary impact](../concepts/cross-boundary.md).
