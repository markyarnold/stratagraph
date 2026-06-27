# How accuracy is measured

StrataGraph's central claim is that it is **reliably accurate and never confidently
wrong**. That is a strong promise, so this page documents exactly how it is
backed: what is measured, against what ground truth, and which CI checks keep the
published numbers honest over time. Nothing here is aspirational. Every number
this section reports is computed by a test over a committed corpus, and the test
fails the build if the code and the number disagree.

If you have not read [Confidence and provenance](../concepts/confidence.md), read
it first: this page assumes the four-band model and does not re-derive it.

## The provenance and confidence model, in one paragraph

Every edge StrataGraph writes carries a **provenance** tag and a numeric
**confidence** in `[0, 1]`. The provenance determines a band, and the band bounds
the confidence (design §4.1):

| Band | Source of the edge | Confidence range |
|---|---|---|
| `RESOLVED` | A compiler / language server with full symbol resolution (SCIP) | 0.90–1.0 |
| `EXTRACTED` | A deterministic read of a source artifact (AST node, SDL, DDL, resolved plan) | 0.95–1.0 |
| `INFERRED` | A single confident heuristic guess (unique-name binding, `self`-method) | 0.40–0.80 |
| `AMBIGUOUS` | An over-included candidate set, none confidently selected | below 0.40 |

The `INFERRED` range here is the **emit ceiling**: `0.80` is the highest
confidence the linkers ever store on an inferred edge. The trust-policy tables
elsewhere (e.g. [Confidence and provenance](../concepts/confidence.md#the-trust-policy))
bucket all sub-`0.90` evidence together as `0.40–0.89`; that is the band a reader
applies, not a different measurement. Same band, two angles: what the engine
emits (`≤ 0.80`) versus how to treat it (`0.40–0.89` → verify before relying).

The design invariant that makes the promise enforceable is: **an inference can
never masquerade as a fact.** A heuristic edge is *structurally incapable* of
claiming a `RESOLVED` or `EXTRACTED` confidence: the band ceiling caps it. So
when StrataGraph is unsure, the uncertainty is visible as a low band, never laundered
into a confident-looking answer. The rest of this page is about *measuring*
whether each band's confidence is earned.

## What "precision per band" means

The calibration question is blunt: **does an edge that claims a given band
actually resolve correctly at the rate the band promises?** If a 0.9-confidence
edge is wrong half the time, the confidence is a lie regardless of how the graph
is built.

To answer it, StrataGraph bins every heuristic call edge by the band it claims, then
asks an independent oracle whether that edge points where it should. Per band:

- **confirmed**: the oracle resolves this call to the same node the heuristic
  edge points at.
- **denied**: the heuristic edge points at a node the oracle rejects (e.g. an
  over-included candidate the receiver's real type rules out).
- **precision** = `confirmed / (confirmed + denied)`, **edge-level**, not
  site-level. An over-included edge that the oracle rejects is a denial, so a
  band that fans out too eagerly scores low. That is the honest answer to "does
  an edge in this band resolve correctly?".

This precision is computed by `accuracy_report` in
`crates/strata-index/src/differential.rs`, which folds a list of per-site
outcomes into per-band tallies. It is a *pure* function of the recorded outcomes
(it never re-runs resolution), so it is unit tested with hand-built outcomes as
well as run over the real corpus.

### Why `AMBIGUOUS` precision is expected to be lower: by design

`AMBIGUOUS` is the band StrataGraph uses when it genuinely cannot pick one target:
several methods share a name and there is no receiver type to disambiguate, so
the linker emits an edge to **every** candidate. By construction, a two-candidate
fan-out where the oracle confirms one scores one *confirmed* and one *denied*,
0.50 precision before anything has gone wrong. A three-candidate trait-dispatch
fan-out scores one confirmed and two denied.

This is not a defect; it is the over-inclusion being measured honestly. The
`AMBIGUOUS` band exists precisely to be the recall-preserving, low-confidence net
that catches "it could be any of these," and its precision is, correctly, the
fraction of that net that hits. The product's contract is that these edges are
**surfaced and flagged as uncertain**, never presented as a confident break (see
[the will-break threshold](#the-will-break-threshold-set-by-measurement) below). A
lower `AMBIGUOUS` precision is the system telling the truth about its own fan-out,
not getting answers wrong.

The opposite failure (a high-confidence band resolving *worse* than a
low-confidence one) would mean the confidence ordering is lying. That is
forbidden by an asserted invariant ([below](#3-the-monotonicity-invariant)).

## The differential harness: heuristic vs compiler-grade ground truth

StrataGraph resolves calls with **language-specific heuristics** (a unique-name
binding, a `self.`/`this.` method lookup, an import-matched call, an
over-included fan-out). Heuristics are fast and language-portable but, by
definition, not the compiler. To know how good they are, you need an oracle that
*is* compiler-grade.

That oracle is **[SCIP](https://github.com/sourcegraph/scip)**, the indexing
format emitted by compiler-backed indexers:

| Language | Ground-truth indexer |
|---|---|
| TypeScript / JavaScript | `scip-typescript` |
| Python | `scip-python` |
| Rust | `rust-analyzer scip` |

Each runs the real type system and emits, per call site, the symbol the call
actually resolves to. The harness compares **StrataGraph's heuristic resolution**
against **that compiler-grade resolution**, per call site, and scores the
difference.

### The metric, exactly

The harness lives in `crates/strata-index/src/differential.rs`. For each call
site that the SCIP oracle resolves (sites the oracle cannot resolve are excluded
and counted separately as *unadjudicable*, never silently scored):

- `scip_target(site)`: the node the compiler-grade indexer resolves the callee
  to.
- `heuristic_targets(site)`: the set of nodes StrataGraph's linker emits edges to for
  that site (0, 1, or many).
- A **recall hit** is a site where `scip_target ∈ heuristic_targets`.
- Each heuristic target equal to `scip_target` is a **confirmed** (true-positive)
  edge; each one that differs is a **denied** (false-positive) edge.
- Band precision and recall aggregate those edge/site tallies as defined above.

There are two ways the harness drives this, and they exist so the measurement can
never drift from the shipping code:

1. **TypeScript** uses `resolve_differential`, which replays the *TS builder's*
   own per-site decision through the shared `resolve_site_targets` function: the
   exact code path the real graph builder uses. The two cannot diverge, a
   property a drift test pins.
2. **Python / Rust** each have their own linker (`assemble_python` /
   `assemble_rust`), so the faithful, no-drift source of the heuristic decision
   is the **assembled graph itself**. `resolve_differential_graph` reads back,
   per call site, the exact `CALLS` edges the linker emitted and the band they
   claim. Both paths then feed the **identical** `accuracy_report` /
   `by_band` / monotonicity code, so every number in every language is computed
   by the same substrate.

### Aligning two coordinate systems (the part that is easy to get subtly wrong)

A heuristic edge and a SCIP edge only agree if you can prove they point at the
*same* definition. Two alignment details, handled in
`crates/strata-index/src/scip_merge.rs`, make that sound:

- **Column units.** Tree-sitter reports **byte** columns; SCIP reports **UTF-16
  code-unit** columns. They coincide for ASCII and diverge on any non-Latin
  character (`é` is 2 bytes but 1 UTF-16 unit; an emoji is 4 bytes but 2). The
  harness converts byte columns to UTF-16 before looking up a SCIP position, so a
  call after a non-ASCII identifier still aligns.
- **Definition-line skew.** SCIP keys a definition by `(file, line, name)`, but
  for an **overload** SCIP points at the signature line while StrataGraph's extractor
  records the implementation line. An exact key would silently drop those sites
  as uncomparable. The merge falls back to a unique `(file, name)` match when the
  exact line key misses: overload-tolerant, but **declining when the name is
  ambiguous in the file**, so distinct same-name symbols are never wrongly
  merged. (Rust adds a ~6-line shim for `rust-analyzer`'s impl-method monikers,
  which start a descriptor segment with `[`; TypeScript and Python monikers never
  do, a property pinned by a test, so they are provably untouched.)

## The hermetic, committed corpora

Every measurement runs over a **committed, hermetic** corpus. Each corpus project
ships its own sources *and* a committed `index.scip`, the compiler-grade ground
truth, generated once. **No compiler runs at test time**: there is no Node,
no `scip-python`, no `rust-analyzer` invocation in CI. The test reads the
committed sources and the committed SCIP and computes the differential. This is
deliberate: the numbers are reproducible by anyone, on any machine, with a plain
`cargo test`, and a green build cannot depend on a toolchain that happens to be
installed.

The committed indexes are regenerated explicitly (and only when the corpus
changes) with a marked, `--ignored` test that *does* shell out to the real
indexer, for example:

```
STRATA_SCIP_LIVE=1 cargo test -p strata-index --test gen_scip -- --ignored generate_py
```

The corpora are intentionally **small** and hand-built to exercise the cases that
matter: re-export/barrel chains, inheritance and `super`, async and higher-order
callbacks, overloads, namespace calls, dynamic `getattr`/reflection, trait
dispatch, typed vs untyped receivers. They are *starting calibrations*, not
statistically authoritative population samples; see
[the results page](./results.md) for the exact sizes and the caveat. The durable
deliverables are the harness, the metric definitions, the gates, and the reports,
all of which sharpen automatically as the corpora grow.

## The CI floors that keep the numbers honest

Measuring accuracy once is easy; keeping a published number true as the code
changes is the hard part. StrataGraph gates it three ways, in
`crates/strata-index/tests/{accuracy_gate,py_accuracy_gate,rust_accuracy_gate}.rs`
on top of the shared `tests/support/accuracy.rs`.

### 1. Floors = measured − margin (never aspirational)

`corpus_meets_documented_floors` fails the build if any gated band's precision
regresses below a floor. The floors are not targets someone hopes to reach: each
is the **measured value minus a small documented honesty margin**. For example,
on the Python corpus `EXTRACTED ≥ 0.95` (measured 1.00, margin 0.05),
`INFERRED ≥ 0.85` (measured 1.00, margin 0.15), `AMBIGUOUS ≥ 0.40` (measured
0.56, margin 0.16). If a refactor makes resolution *worse*, the number drops
below its floor and the build goes red. If it makes it *better*, the floor is
re-derived from the new measurement. Floors only ever track reality downward.

A band is gated **only when it has at least 5 adjudicable sites**
(`MIN_GATED_SITES`); below that the sample is too small to mean anything, so the
band is reported but not gated. This is the small-corpus honesty rule, applied
mechanically.

### 2. Consistency: the published report cannot drift from the code

Each language ships a machine-readable `*-resolution.json` next to its prose
report. `report_matches_committed_doc` asserts the **live** differential,
recomputed from the committed corpus on every test run, equals that JSON,
field by field (overall precision/recall, coverage counts, and every band's
sites / confirmed / denied / unadjudicable / precision, including
*defined-ness*: a band the code reports as undefined must be `null` in the doc,
and vice versa). The prose tables quote those same JSON numbers. So a number in
this documentation cannot silently diverge from what the code actually measures:
change the code without regenerating the doc and the consistency test fails.

### 3. The monotonicity invariant

`check_band_monotonicity` asserts measured precision is **non-increasing down the
bands**: `RESOLVED ≥ EXTRACTED ≥ INFERRED ≥ AMBIGUOUS`. A higher-confidence band
that resolves *less* reliably than a lower one means the confidence ordering is
lying, and the build fails on it. Bands with no measured precision (zero
adjudicable edges) carry no claim and are skipped: the invariant is asserted
only between bands that actually have a number. This is the structural guarantee
behind "a 0.9 edge is at least as trustworthy as a 0.5 edge."

## The will-break threshold, set by measurement

The measurement does more than report numbers: it **sets a product default by
data instead of judgment**. `impact` and `detect_changes` classify each affected
dependent as either **WILL BREAK** or **may be affected, review**. Where should
that line fall? Design §15.6 left it as a recall-vs-noise question to resolve
empirically: the cutoff is the **lowest band whose measured precision crosses the
will-break bar**.

The TypeScript calibration answered it. `INFERRED` measured 1.00 and `AMBIGUOUS`
measured 0.53, so the boundary sits at the **`INFERRED` band floor, 0.40** (the
lowest confidence an `INFERRED` edge can carry per §4.1): an edge with confidence
**≥ 0.40** crosses the will-break bar; an `AMBIGUOUS` edge (capped below 0.40)
does not. The Python (`AMBIGUOUS` 0.56) and Rust (`AMBIGUOUS` 0.47) corpora
corroborate the same boundary independently: `INFERRED`-and-above resolve well
above the bar, `AMBIGUOUS` sits below it.

This measured cutoff is recorded in code as
`strata_core::traverse::DEFAULT_WILL_BREAK_CONFIDENCE` (= 0.40), with its
justification in the doc comment, and is pinned by tests. Two facts keep it
honest:

- It governs the **label only**, never what is shown. `impact` stays
  recall-biased by default and surfaces *everything*, `AMBIGUOUS` paths included
  and flagged; the threshold decides what is *called* a break, not what appears.
- `AMBIGUOUS` edges are excluded from "will break" two ways that agree: by their
  provenance (an `ambiguous` guard) and because §4.1 caps their stored confidence
  below 0.40 by construction. So an over-included guess can never be labelled a
  certain break, which is the same "never confidently wrong" discipline applied to
  the impact verdict.

See [Confidence and provenance](../concepts/confidence.md#the-will-break-label)
for how the label is surfaced across the CLI, MCP, and desktop.

## The non-vacuity guardrails

A subtle way to publish a dishonest accuracy number is to compute it over **no
data** and report a vacuous `1.00`. StrataGraph forbids this explicitly:

- **Undefined ≠ perfect.** A band with no adjudicable edges reports precision as
  `--` / `null` (undefined), **never** a vacuous `1.00` or `0.00`. In code,
  `BandTally::precision` returns `Option<f64>` and is `None` when the denominator
  is zero. Because the heuristic never emits `RESOLVED`-grade edges, the
  `RESOLVED` band is *always* undefined over a heuristic corpus, and it honestly
  says so rather than claiming a perfect score it never earned. (The TS heuristic
  also never emits `EXTRACTED`-grade edges, so that band is undefined for TS too;
  the Python and Rust linkers *do* emit `EXTRACTED` edges for a deterministic
  same-file binding, so it is a populated, gated band there.)

- **Unadjudicable sites are surfaced, not assumed.** A site the oracle cannot
  ground-truth (a call into an `any`-typed value, a `.clone()` on a std type with
  no first-party target) is tallied in its band's `unadjudicable` column and
  **excluded** from the precision denominator, never silently counted as
  confirmed or denied.

- **Claimed bands must carry real data.** `assert_band_nonvacuous` pins that
  every band a language *claims* to calibrate carries at least `MIN_GATED_SITES`
  adjudicable sites. So a "measured precision" the report publishes can never be
  secretly computed over too few sites: either the corpus genuinely populates
  the band, or the band stops being gated.

Together these turn the "never confidently wrong" thesis into something a
continuous-integration run enforces: the system either has real evidence for a
band's precision, or it says it does not, and it can never quietly substitute a
vacuous number for the truth.

## What this does and does not establish

This methodology measures **call-resolution precision per confidence band**
against a compiler-grade oracle, on small hermetic corpora, with floors and
consistency checks that prevent the published numbers from drifting or inflating.

It does **not** claim statistical authority over real-world repositories at the
current corpus sizes, and it does **not** yet cover every language with a
compiler oracle (C# is extraction-coverage only; see
[results](./results.md) and [limitations](./limitations.md)). The honest framing,
applied everywhere in this section, is: *these are starting calibrations, pinned
by tests and sharpening as the corpora grow.*
