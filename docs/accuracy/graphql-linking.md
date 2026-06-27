# GraphQL Link Coverage

Measured coverage of Strata's **contract-plane linking** for GraphQL: how much
resolver (producer) and gql-document (consumer) code the slice-4 contract plane
connects to `GraphqlField` operations, and at what honest-provenance tier. This is
spec R4 (a published link-coverage report), the GraphQL companion to
`docs/accuracy/openapi-linking.md`.

The numbers are produced by `link_estate` over a committed, hermetic fixture
estate (sources only; no `node_modules`, no Node at test time). They are kept
honest two ways, the same discipline as the OpenAPI report:

- **`tests/graphql_coverage.rs::graphql_report_matches_committed_numbers`** asserts
  the live `EstateLinkCoverage` equals the numbers tabulated below (so this report
  cannot silently drift from the code).
- **`tests/graphql_coverage.rs::graphql_coverage_meets_documented_floors`** is the
  CI gate: it fails the build if any documented floor regresses.

Regenerate the raw figures with:

```
cargo test -p strata-index --test graphql_coverage print_graphql_coverage -- --ignored --nocapture
```

## Honesty / scope caveat

**The corpus is a single 2-repo fixture estate.** These numbers are a *starting*
coverage measurement on a deliberately small, hand-built estate that exercises
every GraphQL linking path once, not a statistically authoritative claim about
real-world recall. The durable deliverables are the `link_estate` pipeline, the
`match_graphql_consumer` matcher, the band invariant over GraphQL `Consumes`
edges, the `(api_id, format, key)` estate dedup, the CI gate, and this report,
all of which sharpen automatically as the corpus grows.

## Canonical identity (the api-scoped key, B6 fix)

A canonical `GraphqlField` is identified estate-wide by **`(api_id, format,
key)`**, encoded in its UID as `contract | <estate> | <api_id>/graphql | <key> |`.
The `api_id` is the manifest-declared `[[repos.apis]]` id when a declared `spec`
owns the field, **else the repo name** (the safe default).

This replaced a bare `(format, key)` identity that falsely merged two *unrelated*
GraphQL APIs sharing a field key (e.g. a user service's `Query.getUser` and a
billing service's unrelated `Query.getUser`) into one node, confidently dragging
an unrelated repo into a blast radius. Now:

- **Safe default:** two repos never merge a shared key; unrelated same-key
  schemas get **distinct** canonical nodes.
- **Opt-in merge:** two repos that host one real API declare the **same** `id` in
  both `[[repos.apis]]` to collapse the shared `Query.<field>` to one node (the
  `dedup_graphql` fixture demonstrates this).
- **Honest fan-out:** when a consumer document's field is owned by several apis,
  we emit one `Ambiguous` 0.35 edge per owning api rather than a silent confident
  `Extracted` pick.

The format part of the discriminator still keeps a GraphQL `Query.getUser` and an
OpenAPI op of the same key string on distinct nodes.

## What is counted

Over the estate graph `link_estate` produces (operations deduped by
`(api_id, format, key)`, producer/consumer edges re-pointed to the canonical
`GraphqlField` node, cross-repo consumer links added):

- `producers_total`: `PRODUCES` edges (a resolver → the field it implements).
- `consumers_total`: `CONSUMES` edges (a gql document → a field it queries),
  after de-dup, including the cross-repo links and any api fan-out edges.
- `consumers_ambiguous`: of `consumers_total`, the `CONSUMES` edges that are an
  **api fan-out**: a consumer whose field is owned by several apis emits one
  `Ambiguous` 0.35 edge per api. Surfaced separately so a fan-out never inflates
  the "confidently linked" reading. **0** for a unique-key estate.
- `by_tier`: `CONSUMES` edges bucketed by provenance: `extracted` (a unique
  GraphQL match, the document *names* the contract in its own language) vs
  `ambiguous` (the api fan-out, the field is owned by several apis); `inferred`
  is 0 for GraphQL consumers (a parsed document is spec-derived, not a convention
  guess).
- `unmatched_consumers`: outgoing HTTP calls (`fetch`/`axios`) that matched no
  operation. **0** here (this estate has no HTTP calls; the field exists for the
  report's fixed column shape).
- `unparsed_documents`: **tagged** GraphQL documents that could not be parsed
  into root fields and so produced no link: an **interpolated** `gql` template
  (text unreliable) or a document `parse_operations` rejects (comment-only /
  empty). The honest "saw a gql document, could not link it" count, never a
  guessed field. **Untagged** template-constant candidates are deliberately NOT
  counted here: a parse failure means it was never GraphQL, so it is silently
  skipped (the honesty rule), not an honest miss.
- `unresolved_root_spreads`: root-level fragment spreads / inline fragments seen
  in parsed documents but not expanded into a concrete field (a fragment's body is
  opaque here). Counted, never guessed; surfaced so the gap is visible.

## Consumer link tiers (honest provenance, R1/R5)

A REST consumer call only *matches* an operation by URL/name convention, so it is
at most `Inferred`. A **GraphQL** consumer document is different: `query { getUser
}` names the contract element `Query.getUser` in the contract's own language. The
only residual uncertainty is which schema owns the key, so:

| signal | trigger | unique match | several matches |
|---|---|---:|---:|
| GraphQL field (tagged) | parsed `` gql`query { getUser }` `` → `Query.getUser` | Extracted **0.95** | Ambiguous 0.35 |
| GraphQL field (untagged) | parsed `` const Q = `query { getUser }` `` → `Query.getUser` | Extracted **0.95** | Ambiguous 0.35 |
| unknown field | a field no schema declares | n/a (no link; surfaced) | n/a |
| interpolated template (tagged) | `` gql`query { ${frag} }` `` | n/a (no link; counted unparsed) | n/a |
| untagged candidate, parse fails | `` const X = `{ not graphql ` `` | n/a (no link; **silently skipped**, not counted) | n/a |

An **untagged** template constant is a *candidate*: it is parse-gated, and once
`parse_operations` succeeds the evidence is identical to a tagged document (the
parse IS the proof it is GraphQL), so it links at the **same** tiers. The only
difference is the accounting for a parse *failure*: a tagged miss is counted in
`unparsed_documents` (the author declared it GraphQL); an untagged failure is
silently skipped (it never claimed to be).

`0.95` sits at the EXTRACTED band floor; the §4.1 band invariant (Extracted ≥
0.95, Inferred ≤ 0.80, Ambiguous < 0.40) extends to these edges, guarded by
`tests/confidence_bands.rs::graphql_contract_edges_satisfy_band_invariant`.

Resolver producers are `Inferred` **0.80** (a resolver-map entry → the field it
implements is a convention match, like a REST route), `Ambiguous` 0.35 when the
key is declared in several schemas.

## Corpus

One committed fixture estate under
`crates/strata-index/tests/fixtures/crossrepo_graphql/`:

- **`repo-schema`**: a GraphQL SDL schema declaring `Query.getUser`,
  `Query.listUsers`, `Mutation.createUser`, and an Apollo-style resolver map
  implementing all three (`getUser`/`createUser` named functions, `listUsers`
  inline). **No consumer code.**
- **`repo-app`**: **no schema**; GraphQL consumers in both the tagged and the
  **untagged** template-constant style (the dominant AppSync/Amplify shape):
  `src/queries.ts` (`` gql`query { getUser }` `` → `Query.getUser`),
  `src/orders.graphql` (an operation document → `Query.listUsers`),
  `src/mutations.ts`, **untagged** constants: `CREATE_USER`
  (`` const CREATE_USER = `mutation { createUser … }` `` → `Mutation.createUser`,
  links exactly like a tagged doc) and `BROKEN_UNTAGGED` (passes the cheap
  prefilter but is not valid GraphQL → **silently skipped**, NOT counted unparsed,
  it never claimed to be GraphQL),
  `src/unknown.ts` (`` gql`query { nonExistentField }` `` → no field declares it),
  `src/broken.ts` (an **interpolated** `gql` template → counted unparsed),
  and `src/empty.graphql` (comment-only → counted unparsed, benign skip).

`node_modules` is **not** committed (`.gitignore` excludes it); the estate is
linked hermetically with `ResolveMode::Off` (no Node/SCIP).

## Results

Measured 2026-06-11 over the committed `crossrepo_graphql` estate.

| metric | value |
|---|---:|
| `producers_total` | **3** |
| `consumers_total` | **3** |
| &nbsp;&nbsp;of which `extracted` | 3 |
| &nbsp;&nbsp;of which `inferred` | 0 |
| &nbsp;&nbsp;of which `ambiguous` | 0 |
| `consumers_ambiguous` (api fan-out) | **0** |
| `unmatched_consumers` | **0** |
| `unparsed_documents` | **2** |
| `unresolved_root_spreads` | **0** |

This is a unique-key estate (every field is owned by exactly one api, the schema
in `repo-schema`, default api id `repo-schema`), so there is no api fan-out:
`consumers_ambiguous` is **0**. The collision/fan-out path is exercised separately
by `tests/estate_api_collision.rs` (two unrelated `Query.getUser` schemas → two
canonical nodes + an Ambiguous fan-out for the shared consumer).

> **Change note (dogfood fix 1).** `consumers_total`/`extracted` rose from 2 → 3
> when `repo-app/src/mutations.ts` was added to the fixture: its **untagged**
> `CREATE_USER` template constant links to `Mutation.createUser` exactly like a
> tagged document. This is the fixture extension that exercises the new untagged
> path, not a behavior change to existing links. `unparsed_documents` stayed at 2;
> the sibling `BROKEN_UNTAGGED` constant is parse-gated and silently skipped, by
> design.

Reading the numbers:

- **3 producer links:** the `getUser`/`createUser` resolver handlers and the
  inline `listUsers` arrow → their canonical `GraphqlField` operations (Inferred
  0.80).
- **3 consumer links, all cross-repo, all Extracted:** `repo-app`'s
  `gql` query (`Query.getUser`, 0.95), the `orders.graphql` operation document
  (`Query.listUsers`, 0.95), and the **untagged** `CREATE_USER` constant
  (`Mutation.createUser`, 0.95) → the canonical fields in the *other* repo. This
  is the cross-repo GraphQL blast-radius payoff: `impact(getUser resolver)` reaches
  the `repo-app` consumer at 0.80 × 0.95 = 0.76. The untagged link proves the
  AppSync/Amplify style is now covered identically to the tagged style.
- **0 unmatched HTTP consumers:** this estate is GraphQL-only.
- **2 unparsed documents:** the interpolated `gql` template in `broken.ts` and the
  comment-only `empty.graphql`, both saw a *tagged* gql document, neither could
  be parsed to a field, neither was guessed into an edge (R1/R5). The untagged
  `BROKEN_UNTAGGED` constant is **not** among them: it parses to nothing and is
  silently skipped (it never claimed to be GraphQL), so the count stays 2.
- The `nonExistentField` query (`unknown.ts`) matches no schema field → **no**
  link, surfaced by its absence (it is not "unparsed"; it parses fine, it simply
  hits a field the estate's schemas don't declare).

## CI floors

`graphql_coverage_meets_documented_floors` gates: `producers_total ≥ 3`,
`consumers_total ≥ 3`, `by_tier.extracted ≥ 3` (the untagged `CREATE_USER`
constant links identically to a tagged doc), and `unparsed_documents == 2`, a
two-sided honesty pin: the tagged interpolated template and the empty doc must
stay surfaced as unparsed (never invented into edges), AND the untagged
`BROKEN_UNTAGGED` constant must NOT inflate the count (it never claimed to be
GraphQL). Floors sit at the measured values (the fixture is deterministic); they
are re-derived from this report whenever the fixture changes.
