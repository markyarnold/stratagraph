# Infrastructure Link Coverage

Measured coverage of Strata's **infrastructure-plane linking**: how much of a
detected AWS CloudFormation / SAM estate the slice-5 infra plane connects to the
code plane (`Runs`, Lambda → its handler module) and the contract plane
(`PRODUCES`, an AppSync resolver chain → the `GraphqlField` it implements), and at
what honest-provenance tier. This is spec R4 (a published link-coverage report),
the infrastructure companion to `docs/accuracy/openapi-linking.md` and
`docs/accuracy/graphql-linking.md`.

The numbers are produced by indexing a committed, hermetic fixture estate (sources
only; no `node_modules`, no Node at test time) and aggregating each repo's
per-repo `InfraLinkCoverage` (surfaced in `IndexStats`, and printed by
`strata index`). They are kept honest two ways, the same discipline as the OpenAPI
and GraphQL reports:

- **`tests/infra_coverage.rs::infra_report_matches_committed_numbers`** asserts the
  live aggregated `InfraLinkCoverage` equals the numbers tabulated below (so this
  report cannot silently drift from the code).
- **`tests/infra_coverage.rs::infra_coverage_meets_documented_floors`** is the CI
  gate: it fails the build if any documented floor regresses.

Regenerate the raw figures with:

```
cargo test -p strata-index --test infra_coverage print_infra_coverage -- --ignored --nocapture
```

## Honesty / scope caveat

**The corpus is a single 2-repo fixture estate.** These numbers are a *starting*
coverage measurement on a deliberately small, hand-built estate that exercises
every infra linking path once, not a statistically authoritative claim about
real-world recall. The durable deliverables are the `infra.rs` builder, the
`RefValue`-graded wiring/`Runs`/money-link rules, the band invariant over the four
infra edge kinds, the estate re-point of the infra-sourced `PRODUCES`, the CI
gate, and this report, all of which sharpen automatically as the corpus grows.

## What is counted

Per repo, over the templates `CfnSamAdapter` detects, the infra plane records:

- `templates_detected`: CFN/SAM templates detected and extracted.
- `resources_total`: every `InfraResource` across those templates (each becomes a
  typed node: `LambdaFn` / `IamRole` / `AppSyncApi` / `AppSyncResolver` /
  `AppSyncDataSource`, or a Generic `CloudResource`).
- `resolvers_total`: AppSync resolvers with a **root** `TypeName` ∈ {Query,
  Mutation, Subscription} and a `FieldName` (the money-link candidates).
- `resolvers_linked`: resolvers whose `{type}.{field}` named a `GraphqlField` in
  the graph → a `PRODUCES` edge was added.
- `resolvers_unlinked`: resolvers whose `{type}.{field}` named **no**
  `GraphqlField` (a field no schema declares) → **no** edge. Surfaced by its
  absence, never invented (R1).
- `lambdas_runs_linked`: Lambdas whose handler path matched **exactly one**
  indexed module → a `Runs` edge.
- `lambdas_handler_unresolved`: Lambdas whose handler path matched **zero or
  several** modules → **no** `Runs` edge. With the TS/JS **and Python** (Slice 9)
  code planes, those handlers now resolve; what lands here is a handler in a
  language with no plane yet (Go, Java, …), a `CodeUri` outside the indexed tree,
  or a genuine miss, surfaced, never invented.

An unresolved reference (a dropped `Assumes`/`Routes` edge from an `Unresolved`
`Role`/`DataSourceName`/`LambdaFunctionArn`) is surfaced by the **absence** of the
edge in the graph rather than a separate counter; the resolver/Lambda buckets are
the report's headline honesty signal.

## Edge tiers (honest provenance, R1/R5)

Every infra edge is graded from the source template's `RefValue`:

| edge | trigger | tier |
|---|---|---:|
| `Assumes` / `Routes` / `Contains` | a same-template `Ref`/`GetAtt` (`RefValue::Resource`) | Extracted **0.95** |
| `Assumes` / `Routes` / `Contains` | a `Sub`/`Join`-recovered id, or the single resource a `Fn::If` reaches (`RefValue::Inferred`) | Inferred **0.70** |
| `Assumes` / `Routes` / `Contains` | a `Fn::If` over ≥2 DISTINCT same-template targets (`RefValue::InferredMulti`) | Inferred **0.70** per branch target |
| `Assumes` / `Routes` / `Contains` | an `Unresolved` ref (parameter / cross-stack / dynamic ARN) | n/a (no edge) |
| `Runs` | `CodeUri`+`Handler` resolves to exactly one indexed module | Extracted **0.95** |
| `Runs` | zero or several module matches (a language with no plane yet, or a miss) | n/a (no edge; counted) |
| `PRODUCES` | a resolver chain wholly `Resource`-graded → the **Lambda** sources it | Extracted **0.95** |
| `PRODUCES` | the chain contains an `Inferred` hop → the **Lambda** sources it | Inferred **0.70** |
| `PRODUCES` | the chain breaks (Unresolved/`Fn::If` hop / missing node) → the **resolver** sources it | Extracted **0.95** |
| `PRODUCES` | `{type}.{field}` names no `GraphqlField`, or a non-root type | n/a (no edge; counted) |

`0.95` sits at the EXTRACTED band floor; `0.70` is comfortably inside the Inferred
band. The §4.1 band invariant (Extracted ≥ 0.95, Inferred 0.40–0.80) extends to
every infra edge kind (`Assumes`/`Routes`/`Runs`/`PRODUCES`/`Contains`), guarded by
`tests/confidence_bands.rs::infra_edges_satisfy_band_invariant`.

**`Fn::If` grading (Slice 10, B2).** The event parser preserves `Fn::If`, so the
grader collects the same-template `Ref`/`GetAtt` ids from BOTH branches: one id →
`Inferred` (band 0.70), two or more distinct ids → an edge per branch target (both
possible deployments surfaced, recall-biased). An `Fn::If` reference is never
`Resource` (we cannot pin which branch deploys without evaluating the condition),
and a branch resolving to nothing (a parameter / `AWS::NoValue`) contributes
nothing, never an invented edge (R1).

**`ApiId` containment (Slice 10, B3).** An AppSync resolver/datasource's `ApiId`
(typically `!GetAtt Api.ApiId`) emits an `AppSyncApi —Contains→ resolver|datasource`
edge at the ref's graded tier. `Contains` is a membership edge: it lights up
`context(api).members` (the API lists its resolvers), but `impact` deliberately does
NOT traverse it: changing the API container is not changing a resolver.

The money-link `PRODUCES` deliberately sources from the **Lambda** when the
resolver→datasource→lambda chain is a fact end-to-end (so `impact(Query.<field>)`
surfaces the implementing function), and falls back to the **resolver node** when
the chain breaks (the `TypeName`/`FieldName` are still template literals: the
field IS implemented here, we just cannot name the Lambda).

## Corpus

One committed fixture estate under
`crates/strata-index/tests/fixtures/crossrepo_infra/`:

- **`repo-a`**: a GraphQL SDL schema declaring `Query.getUser` and
  `Mutation.createUser`, an AppSync SAM template (`UserFunction` Lambda with a
  same-template `Role`, a `UserDS` data source backing it, and `GetUserResolver` /
  `CreateUserResolver` whose chains resolve crisply to the Lambda), and the
  Lambda's handler module `src/handlers/user.ts`.
- **`repo-b`**: **no schema, no infrastructure**, a pure `gql` consumer of
  `Query.getUser` (`src/queries.ts`). It is reached cross-repo when
  `impact(Query.getUser)` runs over the linked estate.

`node_modules` is **not** committed (`.gitignore` excludes it); the estate is
indexed hermetically with `ResolveMode::Off` (no Node/SCIP).

## Results

Measured 2026-06-11 over the committed `crossrepo_infra` estate (aggregated across
its repos; `repo-b` contributes no infrastructure).

| metric | value |
|---|---:|
| `templates_detected` | **1** |
| `resources_total` | **6** |
| `resolvers_total` | **2** |
| &nbsp;&nbsp;of which `resolvers_linked` | 2 |
| &nbsp;&nbsp;of which `resolvers_unlinked` | 0 |
| `lambdas_runs_linked` | **1** |
| `lambdas_handler_unresolved` | **0** |

Reading the numbers:

- **1 template, 6 resources:** repo-a's SAM template, with its Lambda, role,
  AppSync API, data source, and two resolvers as typed nodes. The API now
  `Contains` its data source and resolvers (from their `ApiId`; Slice 10 B3): a
  membership edge that lights up `context(api).members` but is never traversed by
  `impact`. (`Contains` is not a linking counter, so the coverage numbers above
  are unchanged by B3.)
- **2 resolvers, both linked:** `GetUserResolver` → `Query.getUser` and
  `CreateUserResolver` → `Mutation.createUser`, each at Extracted 0.95 sourced from
  the `UserFunction` Lambda (their resolver→datasource→lambda chains are wholly
  `Resource`-graded). This is the cross-repo infrastructure blast-radius payoff:
  over the linked estate, `impact(Query.getUser)` reaches the implementing Lambda
  in repo-a (0.95) **and** the gql consumer in repo-b (0.95): the infra
  `PRODUCES` edge re-points onto the canonical `GraphqlField` exactly like a
  code-sourced producer.
- **1 Runs link:** `UserFunction`'s `user.handler` + `CodeUri: src/handlers/`
  resolves to the single indexed module `src/handlers/user.ts`.
- **0 unlinked resolvers, 0 unresolved handlers:** this estate is deliberately
  clean (every resolver names a declared field and the one Lambda's handler
  resolves). The dedicated honesty cases (a ghost-field resolver and a Python
  handler that stay unlinked) are exercised by the single-repo `infra_appsync`
  fixture in `tests/infra_linking.rs`, not this estate.

## CI floors

`infra_coverage_meets_documented_floors` gates: `templates_detected ≥ 1`,
`resolvers_linked ≥ 2`, `lambdas_runs_linked ≥ 1`, and a two-sided honesty pin
`resolvers_unlinked == 0` / `lambdas_handler_unresolved == 0`: a regression that
silently dropped a real link (inflating either honesty counter) fails the build.
Floors sit at the measured values (the fixture is deterministic); they are
re-derived from this report whenever the fixture changes.
