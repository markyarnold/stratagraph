# OpenAPI Link Coverage

Measured coverage of Strata's **contract-plane linking**: how much producer and
consumer code the slice-3 contract plane connects to OpenAPI operations, and at
what honest-provenance tier. This is spec R4 (a published link-coverage report),
the consumer-side companion to `docs/accuracy/ts-resolution.md`.

The numbers are produced by `link_estate` over a committed, hermetic fixture
estate (sources only; no `node_modules`, no Node at test time). They are kept
honest two ways, the same discipline as the call-resolution accuracy report:

- **`tests/openapi_linking.rs::report_matches_committed_numbers`** asserts the
  live `EstateLinkCoverage` equals the numbers tabulated below (so this report
  cannot silently drift from the code).
- **`tests/openapi_linking.rs::coverage_meets_documented_floors`** is the CI gate;
  it fails the build if any documented floor regresses.

Regenerate the raw figures with:

```
cargo test -p strata-index --test openapi_linking print_coverage -- --ignored --nocapture
```

## Honesty / scope caveat

**The corpus is a single 2-repo fixture estate (4 link signals).** These numbers
are a *starting* coverage measurement on a deliberately small, hand-built estate
that exercises every linking path once, not a statistically authoritative claim
about real-world recall. The durable deliverables are the `link_estate` pipeline,
the tiered `match_consumer`, the band invariant over `Consumes`, the CI gate, and
this report, all of which sharpen automatically as the corpus grows.

## Canonical identity (the api-scoped key, B6 fix)

A canonical operation is identified estate-wide by **`(api_id, format, key)`**,
encoded in its UID as `contract | <estate> | <api_id>/<format> | <key> |`. The
`api_id` is the manifest-declared `[[repos.apis]]` id when a declared `spec` owns
the operation, **else the repo name** (the safe default).

This replaced a bare `(format, key)` identity that falsely merged two *unrelated*
APIs sharing an operation key (two services both declaring `GET /health`) into one
node, confidently dragging an unrelated repo into a blast radius. Now:

- **Safe default:** two repos never merge a shared key. An undeclared spec
  namespaces to its repo, so unrelated same-key APIs get **distinct** canonical
  nodes.
- **Opt-in merge:** two repos that host one real API declare the **same** `id` in
  both `[[repos.apis]]` (each pointing at its own spec copy) to collapse the
  shared key to one node, the explicit unification path.
- **Honest fan-out:** when a consumer's matched `(format, key)` is owned by
  several apis, we emit one `Ambiguous` 0.35 edge per owning api (recall-biased,
  flagged) rather than a silent confident pick.

## What is counted

Over the estate graph `link_estate` produces (operations deduped by
`(api_id, format, key)`, producer/consumer edges re-pointed to the canonical
operation node, cross-repo consumer links added):

- `producers_total`: `PRODUCES` edges (a route → the operation it implements).
- `consumers_total`: `CONSUMES` edges (consumer code → an operation it calls),
  after de-dup, including the cross-repo links and any api fan-out edges.
- `consumers_ambiguous`: of `consumers_total`, the `CONSUMES` edges that are an
  **api fan-out**: a consumer whose matched `(format, key)` is owned by several
  apis emits one `Ambiguous` 0.35 edge per api. Surfaced separately so a fan-out
  never inflates the "confidently linked" reading (the unique-owner count is
  `consumers_total − consumers_ambiguous`). **0** for a unique-key estate.
- `by_tier`: `CONSUMES` edges bucketed by provenance: `inferred` (a unique,
  confident convention match) vs `ambiguous` (the api fan-out); `extracted` is
  always 0 for consumers (a consumer link is never a fact).
- `unmatched_consumers`: outgoing HTTP calls (`fetch`/`axios`, the unambiguous
  consumer signal) that matched **no** operation: a dynamic URL or an endpoint
  no spec declares. The honest "saw a call, could not link it" count, never
  turned into an invented edge.

## Consumer link tiers (honest provenance, R1/R5)

A consumer call does not *name* the operation it hits; the link is a name- or
URL-convention match, so every consumer link is at most `Inferred`, and a
multi-candidate match is `Ambiguous`. All tiers respect the §4.1 band invariant
(Inferred ≤ 0.80, Ambiguous < 0.40), guarded by
`tests/confidence_bands.rs::consumes_edges_satisfy_band_invariant`.

| signal | trigger | unique match | several matches |
|---|---|---:|---:|
| operationId name | callee name == an `operationId` | Inferred **0.75** | Ambiguous 0.35 |
| literal URL | `fetch("/users/1")` → method + norm path | Inferred **0.70** | Ambiguous 0.35 |
| template URL | `` fetch(`/users/${id}`) `` → method + norm path | Inferred **0.60** | Ambiguous 0.35 |
| dynamic / no match | computed URL, or no operation matches | n/a (no link; counted unmatched) | n/a |

A server route registration (`app.get("/x", h)`) shares the `recv.<verb>(string,
…)` shape with `axios.get("/x")` but is a **producer**, not an outgoing request;
it is never recorded as a consumer call (so a route file gets no spurious
`CONSUMES` edge to the operation it produces).

## Corpus

One committed fixture estate under `crates/strata-index/tests/fixtures/crossrepo/`:

- **`repo-producer`**: an OpenAPI spec declaring `getUser` (`GET /users/{id}`)
  and a `app.get("/users/:id", getUser)` route. No consumer code.
- **`repo-consumer`**: **no spec**; three outgoing calls: `fetch("/users/123")`
  (literal URL → getUser), `getUser({ id })` (operationId name → getUser), and
  `fetch("/widgets/9")` (an endpoint no operation declares → no link).

`node_modules` is **not** committed (`.gitignore` excludes it); the estate is
linked hermetically with `ResolveMode::Off` (no Node/SCIP).

## Results

Measured 2026-06-09 over the committed `crossrepo` estate.

| metric | value |
|---|---:|
| `producers_total` | **1** |
| `consumers_total` | **2** |
| &nbsp;&nbsp;of which `inferred` | 2 |
| &nbsp;&nbsp;of which `ambiguous` | 0 |
| &nbsp;&nbsp;of which `extracted` | 0 |
| `consumers_ambiguous` (api fan-out) | **0** |
| `unmatched_consumers` | **1** |

This is a unique-key estate (`getUser` is owned by exactly one api, the spec in
`repo-producer`, default api id `repo-producer`), so there is no api fan-out:
`consumers_ambiguous` is **0**. The collision/fan-out path is exercised separately
by `tests/estate_api_collision.rs` (two unrelated `GET /health` specs → two
canonical nodes + an Ambiguous fan-out for the shared uptime probe).

Reading the numbers:

- **1 producer link:** the `getUser` handler in `repo-producer` → the one
  canonical `getUser` operation (Inferred 0.80). The route registration is not
  itself counted as a consumer.
- **2 consumer links, both cross-repo, both Inferred:** `repo-consumer`'s
  literal-URL `fetch("/users/123")` (0.70) and operationId-name `getUser(…)`
  (0.75) → the canonical `getUser` operation in the *other* repo. This is the
  cross-repo blast-radius payoff: `impact(getUser handler)` reaches both.
- **1 unmatched consumer:** `fetch("/widgets/9")` matches no operation, reported
  unmatched, never invented into an edge (R1/R5).

## CI floors

`coverage_meets_documented_floors` gates: `producers_total ≥ 1`,
`consumers_total ≥ 2`, `by_tier.inferred ≥ 2`, and `unmatched_consumers ≥ 1`
(the undeclared-endpoint call must stay surfaced as unmatched). Floors sit at the
measured values (the fixture is deterministic); they are re-derived from this
report whenever the fixture changes.
