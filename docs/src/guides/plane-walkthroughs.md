# Plane walkthroughs

**Goal:** see, concretely, what each plane buys you, and where the payoff is the cross-boundary jump grep can't make. One short walkthrough per plane: **code** (a call chain), **contract** (a GraphQL field → its implementing resolver + querying frontend), **infra** (an IAM role → the Lambdas and resources that depend on it), and **data** (a table/column → the code that reads, writes, and maps it).

Each uses real commands. The code walkthrough runs against StrataGraph's own repo; the contract/infra/data ones use the engine's cross-repo fixtures under `crates/strata-index/tests/fixtures/` (indexed with `--workspace`), so each is reproducible. For the plane model itself, see [The five planes](../concepts/planes.md).

---

## Code plane: a call chain

The code plane is functions, classes, imports, and calls. `impact` walks reverse-call edges; `context` shows a symbol's immediate callers and callees.

Take a function and ask who depends on it:

```console
$ strata impact cmd_impact
Impact of cmd_impact (crates/strata-cli/src/lib.rs) — 7 affected:
  depth  conf  amb  verdict     name (path)
      1  0.95   no  WILL BREAK  cmd_impact_dead_table_keeps_bare_message (crates/strata-cli/src/lib.rs)
      ...
      1  0.80   no  WILL BREAK  main (crates/strata-cli/src/main.rs)
```

Six tests call it directly, and so does `main`, the CLI entry point. To prove *why* a given caller is in the radius, ask `explain` for the chain:

```console
$ strata explain cmd_impact cmd_impact_dead_table_keeps_bare_message
Why cmd_impact affects cmd_impact_dead_table_keeps_bare_message (conf 0.95, WILL BREAK):
  cmd_impact  —CALLS (Extracted 0.95)→  cmd_impact_dead_table_keeps_bare_message    running 0.95
```

One hop, an `Extracted` call edge at 0.95: a fact, parsed from the source.

**Honest bounds on the code plane.** Direct, statically-resolvable calls land in the Extracted band. An **instance-method** call (`x.reload()`) often can't be resolved to one definition without full type inference, so it's surfaced as **Ambiguous** and capped below 0.40: `may affect`, never a certain break (you saw this in [What breaks if I change this?](impact.md)). And **class instantiation** (`new Foo()`) is *not* a call edge: constructing a type doesn't create a reverse-call dependency on it. So a clean code-plane `impact` is your call graph, with the unresolved tail honestly flagged rather than guessed.

---

## Contract plane: a GraphQL field to its resolver and frontend

The contract plane is GraphQL fields and API operations, linked to code by **producer** (who implements it) and **consumer** (who calls it) edges. This is where the cross-boundary payoff starts. (Fixture: `crossrepo_graphql`, indexed as an estate.)

Start from the field and look both ways with `context`:

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

One call gives you both sides of the contract: the resolver that **implements** `Query.getUser` and the frontend function that **queries** it, in two different repos. Now the blast radius:

```console
$ strata impact getUser --workspace strata.workspace.toml
Impact of getUser (src/resolvers.ts) — 2 affected:
  depth  conf  amb  verdict     name (path)
      1  0.80   no  WILL BREAK  QUERY getUser (getUser)
      2  0.76   no  WILL BREAK  loadUserProfile (src/queries.ts)
```

**The payoff:** changing the resolver reaches the frontend consumer through the operation node, a link that exists in *no* import and *no* shared code. With `--no-contracts`, this same impact returns 0 affected. The contract plane is the difference between "looks safe" and "breaks another team's query." (Full cross-repo treatment: [Cross-repository impact](cross-repo.md).)

---

## Infra plane: an IAM role to what assumes it

The infra plane is cloud resources parsed from IaC (Lambdas, IAM roles, AppSync APIs, data sources, resolvers) linked by `Assumes`, `Routes`, and `Runs` edges. `impact` traverses them as dependency edges, so changing a resource reaches everything that depends on it, and (via the contract hop off each Lambda) everything downstream. (Fixture: `crossrepo_infra`.)

Look at what assumes a role:

```console
$ strata context UserRole --workspace strata.workspace.toml
Context for UserRole (IamRole) — template.yaml
  uid: infra|repo-a|template.yaml|UserRole|
  assumed_by (1):
    - UserFunction (template.yaml)
  ...
```

`UserRole` is assumed by `UserFunction`. Now ask for the full blast radius of changing that role:

```console
$ strata impact UserRole --workspace strata.workspace.toml
Impact of UserRole (template.yaml) — 7 affected:
  depth  conf  amb  verdict     name (path)
      1  0.95   no  WILL BREAK  UserFunction (template.yaml)
      2  0.90   no  WILL BREAK  MUTATION createUser (createUser)
      2  0.90   no  WILL BREAK  QUERY getUser (getUser)
      2  0.90   no  WILL BREAK  UserDS (template.yaml)
      3  0.86   no  WILL BREAK  CreateUserResolver (template.yaml)
      3  0.86   no  WILL BREAK  GetUserResolver (template.yaml)
      3  0.86   no  WILL BREAK  loadUser (src/queries.ts)
```

**The payoff, in one chain:** the IAM role → the Lambda that assumes it (d=1) → the operations that Lambda produces and the data source fronting it (d=2) → the AppSync resolvers and, at d=3, **`loadUser`, a frontend query in another repo**. A change to an IAM role surfacing a break in a frontend query, three hops and one repo boundary away. That reach is infra edges and contract edges composed together; no single-file or single-plane view produces it.

**Honest bounds on the infra plane.** Edges come from what the IaC actually declares (a SAM `Role: !GetAtt`, an AppSync `DataSourceName`, a handler path). A relationship expressed only at runtime, or in a tool StrataGraph doesn't parse, won't appear: infra impact is as complete as the templates are explicit.

---

## Data plane: a table and column to the code around it

The data plane is database tables and columns parsed from SQL DDL, linked to code three ways: **reads** and **writes** from SQL string literals, and **`MapsTo`** from an ORM model's `__tablename__`. `impact` on a table surfaces all of them; `context` shows its columns and ORM mapping. (Fixture: `crossrepo_data`.)

Start with the table:

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
```

The table is mapped by an ORM model and has four columns. Its blast radius pulls in the code:

```console
$ strata impact users --workspace strata.workspace.toml
Impact of users (schema.sql) — 4 affected:
  depth  conf  amb  verdict     name (path)
      1  0.95   no  WILL BREAK  User (models.py)
      1  0.95   no  WILL BREAK  touch_last_login (writer.py)
      1  0.95   no  WILL BREAK  getUserEmail (src/users.ts)
      1  0.95   no  WILL BREAK  listUsersWithOrg (src/users.ts)
```

**The payoff:** one table reaches a Python ORM model (`User`), a Python writer (`touch_last_login`), and two TypeScript readers (`getUserEmail`, `listUsersWithOrg`), **across languages**. Drop a column and you want all four in front of you. You can scope to a single column, too:

```console
$ strata impact last_login --workspace strata.workspace.toml
Impact of last_login (schema.sql) — 5 affected:
  depth  conf  amb  verdict     name (path)
      1  0.95   no  WILL BREAK  users (schema.sql)
      2  0.90   no  WILL BREAK  User (models.py)
      2  0.90   no  WILL BREAK  touch_last_login (writer.py)
      2  0.90   no  WILL BREAK  getUserEmail (src/users.ts)
      2  0.90   no  WILL BREAK  listUsersWithOrg (src/users.ts)
```

The column reaches its table (d=1), then everything around the table (d=2).

**Honest bounds on the data plane.** Links are **table-level**, and they come from *literal* SQL and the ORM `__tablename__`; a dynamically built query (`f"DELETE FROM {table}"`) is deliberately **not** linked, because the table name isn't a literal. So "no writes found" can mean "only dynamic writes exist." This is the never-invent rule: StrataGraph would rather miss a link than fabricate one. Verify before you treat a table as unused (see [Is this schema field dead?](dead-surface.md)).

---

## Putting it together

The planes aren't separate tools; they're one graph. A single `impact` composes call edges, producer/consumer edges, infra edges, and data edges, so the answer to "what breaks?" follows the dependency wherever it goes: into another file, across a plane, into another repository. The honest bounds above are why the verdict column exists: StrataGraph surfaces the uncertain tail (ambiguous calls, dynamic queries) flagged, never as certain breakage.

## What to do next

- Reading the verdict / confidence / depth columns: [What breaks if I change this?](impact.md).
- Why these cross-plane edges exist and how they're graded: [Cross-boundary impact](../concepts/cross-boundary.md) and [Confidence and provenance](../concepts/confidence.md).
- The cross-repo cases, in depth: [Cross-repository impact](cross-repo.md).
- What's covered per language and per plane: [Languages and coverage](../concepts/coverage.md).
