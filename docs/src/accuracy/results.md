# Measured results

This page summarizes the numbers StrataGraph actually measures, and points to the
canonical, test-pinned reports that are the source of truth. Read
[How accuracy is measured](./methodology.md) first for what the bands and the
metric mean.

> **The committed reports are authoritative; this page is a summary.** Each
> headline number below is quoted from a report whose figures are pinned by a
> consistency test (`report_matches_committed_doc`) against the live differential.
> Those reports live in the repository under `docs/accuracy/` (cited by path
> below: they are the in-repo source of truth, not separate book pages). Where
> this page gives a number, it is *measured on the committed corpus* and
> attributed to its report; treat the cited report as the precise, current value,
> since the corpora sharpen over time.

## Resolution precision, by language and band

These are the per-confidence-band precisions from the differential harness:
heuristic call resolution scored against a compiler-grade SCIP oracle
(`scip-typescript` / `scip-python` / `rust-analyzer`). Precision is **edge-level**
(`confirmed / (confirmed + denied)`); `--` means *undefined* (no adjudicable
edges, never a vacuous 1.00, see [methodology](./methodology.md#the-non-vacuity-guardrails)).

The pattern to read across all three languages: **the deterministic and
single-guess bands resolve at or near 1.00, and `AMBIGUOUS` is lower by design**.
It is the honest over-inclusion fan-out, where a multi-candidate edge set
scores one confirmed against one-or-more denied. In every language the
[monotonicity invariant](./methodology.md#3-the-monotonicity-invariant) holds:
`EXTRACTED ≥ INFERRED ≥ AMBIGUOUS`.

### TypeScript / JavaScript

Ground truth `scip-typescript`. Measured on the committed **56-site** corpus
(52 SCIP-adjudicable), six fixtures covering re-export chains, inheritance/
override, async + higher-order callbacks, overloads, namespace and dynamic
access. See the canonical report `docs/accuracy/ts-resolution.md`.

| Band | Precision | Adjudicable sites | Notes |
|---|---:|---:|---|
| `RESOLVED` | `--` | 0 | the heuristic emits no compiler-grade edge |
| `EXTRACTED` | `--` | 0 | the TS heuristic emits no deterministic-AST call edge |
| `INFERRED` | **1.00** | 28 | single-candidate guesses (`BareSingle` / `ThisMethod`) |
| `AMBIGUOUS` | **0.53** | 24 | over-included fan-out (`UnknownReceiver` / `BareMulti`) |

The TS report additionally breaks resolution down per **heuristic class**
(`BareSingle` precision 1.00, `ThisMethod` 1.00, `UnknownReceiver` 0.53) and
records the calibrated, band-capped confidences actually written to edges; see
the report for that detail.

### Python

Ground truth `scip-python` 0.6.6. Measured on the committed **45-site** corpus
(34 SCIP-adjudicable), three packages (`shop`, `geometry`, `pipeline`) covering
import-matched cross-module calls, same-module defs, `self`-methods,
typed-receiver fan-outs, constructors, and dynamic `getattr`. See the canonical
report `docs/accuracy/py-resolution.md`.

| Band | Precision | Adjudicable sites | Notes |
|---|---:|---:|---|
| `RESOLVED` | `--` | 0 | the heuristic emits no compiler-grade edge |
| `EXTRACTED` | **1.00** | 9 | same-module bare call to a local `def` (a deterministic binding) |
| `INFERRED` | **1.00** | 15 | import-matched, `self`-method, unique-name calls |
| `AMBIGUOUS` | **0.56** | 10 | unknown-receiver fan-out |

Unlike the TS heuristic, the Python linker *does* emit `EXTRACTED`-band edges
(a same-module bare call to a local `def` is a deterministic name binding), so
`EXTRACTED` is a populated, gated band here.

### Rust

Ground truth `rust-analyzer` 1.96.0. Measured on the committed **33-site** corpus
(30 SCIP-adjudicable), two cargo crates (`shapes`, `registry`) covering
type-qualified constructors, `self`/`Self` methods, same-file helpers, instance-
receiver fan-outs, and trait dispatch. See the canonical report
`docs/accuracy/rust-resolution.md`.

| Band | Precision | Adjudicable sites | Notes |
|---|---:|---:|---|
| `RESOLVED` | `--` | 0 | the heuristic emits no compiler-grade edge |
| `EXTRACTED` | **1.00** | 11 | same-file simple-name call to a same-file def |
| `INFERRED` | **1.00** | 10 | `Type::new()`, `self`-methods, unique cross-module names |
| `AMBIGUOUS` | **0.47** | 9 | instance-receiver and trait-dispatch fan-out |

Trait dispatch is the sharpest `AMBIGUOUS` example: a `.describe()` call fans out
to three candidates (both impls plus the trait signature), of which
`rust-analyzer` confirms exactly one: one confirmed, two denied per call. That
is the over-inclusion the band is designed to surface honestly, not an error.

### C#: extraction coverage only (no resolution-precision number yet)

C# has **extraction-coverage only** today. There is **no** measured per-band
resolution-precision report for C#, because producing one requires a
compiler-grade oracle (`scip-dotnet`) which needs the **.NET SDK**, not
available in the current build environment. So the cross-language measured-
precision corpus covers **TypeScript, Python, and Rust**; C# resolution precision
awaits the .NET SDK + `scip-dotnet` (the same shape as the deferred Roslyn
compiler-precision track).

The C# linker is still **band-disciplined and honest by construction**: every
call edge is capped strictly below a compiler-grade confidence, reflection and
dynamic dispatch are never guessed, and what it links / leaves unlinked is pinned
by a coverage gate. It is simply not yet *graded against an oracle*. See the
canonical report `docs/accuracy/cs-extraction.md` for the extraction coverage and
the per-rule confidence constants.

## Per-plane extraction and linking coverage

Beyond call resolution, each non-code plane ships its own **coverage report**: a
test-pinned account of what is extracted and linked, at what confidence, and
(load-bearing for the honesty story) **what is deliberately left unlinked**.
These are coverage reports, not precision-against-an-oracle scores; a missed link
is surfaced and counted, never faked. Each is pinned two ways, like the
resolution reports: a floors gate that fails on regression, and a consistency
test that asserts the live coverage equals the committed numbers.

| Plane / surface | Canonical report (in-repo) |
|---|---|
| GraphQL operations (producers / consumers) | `docs/accuracy/graphql-linking.md` |
| OpenAPI operations | `docs/accuracy/openapi-linking.md` |
| gRPC / protobuf (extraction only, see [limitations](./limitations.md)) | `docs/accuracy/grpc-linking.md` |
| Infrastructure (CloudFormation, Lambda `Runs`) | `docs/accuracy/infra-linking.md` |
| Terraform / Terragrunt | `docs/accuracy/terraform-linking.md` |
| Data plane (SQL schema, ORM `MapsTo`) | `docs/accuracy/data-linking.md` |
| Python code-plane extraction | `docs/accuracy/py-extraction.md` |
| Rust code-plane extraction | `docs/accuracy/rust-extraction.md` |
| C# code-plane extraction | `docs/accuracy/cs-extraction.md` |

A recurring honesty signal across these reports: a contract surface with **zero
producers and zero consumers** is flagged as *likely dead* rather than reported
as live, and an infra handler that cannot be resolved to a code module (e.g. a
C# .NET Lambda whose handler is a CLR reference, not a file path) is counted as
*unresolved* rather than linked to a guess.

## How to read these numbers

- **They are starting calibrations on small, committed corpora.** The corpus
  sizes above (tens of call sites each) are deliberately small and hand-built to
  exercise the hard cases. They are not statistically authoritative population
  samples, and the reports say so explicitly. They sharpen (and the CI floors
  are re-derived) as the corpora grow.
- **The linked reports are the current truth.** Because the prose tables here
  could drift as corpora expand, treat every number as "measured on the committed
  corpus; see the linked report." The reports cannot drift from the code: a
  consistency test fails the build if they do.
- **Lower `AMBIGUOUS` precision is the system being honest**, not getting answers
  wrong. Those edges are surfaced and flagged below the will-break bar (confidence
  < 0.40); they are never presented as a confident break. See
  [Confidence and provenance](../concepts/confidence.md) and
  [limitations](./limitations.md).
