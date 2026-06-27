# TS/JS Call-Resolution Accuracy

Measured accuracy of Strata's slice-1 **heuristic** call resolution, scored
against **SCIP (`scip-typescript`) as ground truth**, per heuristic class. This
is spec A5 (quantified, published accuracy) and feeds spec A4 (confidence
calibration).

The numbers are produced by `resolve_differential` + `accuracy_report` over a
committed, hermetic fixture corpus (each project ships its sources + a committed
`index.scip`; no Node runs at test time). They are kept honest two ways:

- **`tests/accuracy_gate.rs::corpus_meets_documented_floors`** fails the build if
  any gated number regresses below its floor.
- **`tests/accuracy_gate.rs::report_matches_committed_doc`** asserts the live
  metrics equal the machine-readable companion `ts-resolution.json` (the same
  numbers tabulated below), so this report cannot silently drift.

Regenerate the raw figures with:

```
cargo test -p strata-index --test accuracy_gate -- --nocapture print_corpus_report
```

## Honesty / scope caveat

**The corpus is modest (56 call sites, 52 SCIP-adjudicable).** These numbers are
a *starting calibration*, not a statistically authoritative accuracy claim. The
durable deliverables are the harness, the per-class + per-band metric
definitions, the CI gate, and this report, all of which sharpen automatically as
the corpus grows. Treat a class (or band) with fewer than 5 adjudicable sites as
indicative only; those are **not** calibrated and keep their slice-1 prior
confidence (noted below). At the current size the **per-band** view is the
meaningful calibration unit: INFERRED (28 sites) and AMBIGUOUS (24 sites) are
both well-populated, while only `BareMulti` (1 site) remains indicative-only.

## The metric (exact definition)

Over **call sites that SCIP resolves** (SCIP-uncovered sites are excluded from
precision/recall and counted separately as `uncovered`):

- `scip_target(site)`: the node SCIP resolves the callee to.
- `heuristic_targets(site)`: the set of nodes the slice-1 heuristic emits edges
  to for that site (0, 1, or many).
- Per site, in its heuristic **class**:
  - **recall hit** iff `scip_target ∈ heuristic_targets`.
  - each heuristic target equal to `scip_target` is a **true-positive edge**;
    each heuristic target `≠ scip_target` is a **false-positive edge**.
- Aggregated per class:
  - `precision = true_positive_edges / (true_positive_edges + false_positive_edges)`
  - `recall = recall_hits / sites_in_class`
- Overall precision/recall aggregate the same edge/site tallies across all
  classes. An empty denominator (a class with no covered sites) is defined as
  `1.0` (vacuous), and the `sites` column makes that emptiness visible.

The four classes are the slice-1 call-resolution branches:

| class | trigger | heuristic candidates |
|---|---|---|
| `BareSingle` | bare `foo()`, one match | the single local/imported `foo` |
| `BareMulti` | bare `foo()`, several matches | every local + imported `foo` |
| `ThisMethod` | `this.m()` | methods named `m` in the enclosing class |
| `UnknownReceiver` | `obj.m()` | **all** methods named `m` repo-wide |

## Corpus

Six committed projects under `crates/strata-index/tests/fixtures/` (the two
slice-2 projects plus four added in Slice 13 to cover real patterns the small
corpus missed):

- **`resolve/`** (reused): aliased import, namespace call, re-export hop, a local
  bare call, non-ASCII callees, and an `any`-typed dynamic call.
- **`accuracy/methods/`**: two classes sharing method names (`save`, `render`)
  called through typed receivers (`UnknownReceiver` over-inclusion SCIP narrows),
  a base/derived `this.base()` (`ThisMethod` recall miss SCIP resolves via
  inheritance), a `this.own()` hit, a unique-name bare import (`BareSingle`), and
  an import/local name collision (`BareMulti`).
- **`accuracy/reexports/`**: two-level barrel chains (`index.ts` → `core.ts` →
  `impls.ts`), an alias-through-barrel (`beta as delta`), a default export lifted
  to a named re-export, and default + namespace imports. The barrel-local call
  names have no candidate in the re-exporting module (recall misses) that SCIP
  follows to the original symbol.
- **`accuracy/inheritance/`**: a three-level hierarchy (`Animal`→`Dog`→`Puppy`)
  with method override, inherited `this.describe()` calls (the enclosing-class
  heuristic misses, SCIP climbs the chain), `super.speak()`, and polymorphic
  typed-receiver `.speak()` over-inclusions SCIP narrows.
- **`accuracy/async_hof/`**: `async`/`await` and Promise `.then()`/`.map()`
  chains (the chained methods resolve to the external `typescript` lib, covered
  ground truth), unique-name free async functions (`BareSingle` hits), and
  higher-order callbacks (both named functions passed + called, and
  callback-*parameter* invocations (`fn(x)`) that have no first-party target).
- **`accuracy/dynamic/`**: namespace-imported free-function calls (`NS.load()`),
  an overloaded `parse` (two signatures, one implementation SCIP resolves to),
  two classes sharing `area`/`name` called through typed receivers (`AMBIGUOUS`
  over-inclusion SCIP narrows), and an `any`-typed `thing.area()` dynamic call
  (genuinely **unadjudicable**, surfaced, not scored).

`node_modules` is **not** committed (each project's `.gitignore` excludes it);
the committed `index.scip` is the hermetic ground truth, regenerated with
`STRATA_SCIP_LIVE=1 cargo test -p strata-index --test gen_scip -- --ignored`.

## Results

Measured 2026-06-13 over the committed corpus, after the overload-alignment fix:
**56 sites, 52 SCIP-covered, 4 unadjudicable**.

| class | sites | precision | recall | calibrated? |
|---|---:|---:|---:|---|
| `BareSingle` | 20 | **1.00** | 0.55 | yes (≥ 5 sites) |
| `BareMulti` | 1 | 0.50 | 1.00 | no, only 1 site |
| `ThisMethod` | 8 | **1.00** | 0.50 | yes (≥ 5 sites) |
| `UnknownReceiver` | 23 | **0.53** | 0.70 | yes (≥ 5 sites) |
| **overall** | -- | **0.68** | **0.62** | -- |

Reading the recalls (where the heuristic *misses* and SCIP earns its keep):

- `BareSingle` recall 0.55: precision stays **1.00** (every bare-single edge it
  *did* emit was correct), but the expanded corpus added many recall misses SCIP
  resolves: **re-export / barrel hops** (the call name has no candidate in the
  re-exporting module), **default imports**, and **callback-parameter calls**
  (`fn(x)`, a parameter has no first-party definition). The heuristic binds on
  the local name; SCIP follows the chain or the binding.
- `UnknownReceiver` recall 0.70, precision **0.53**: the over-inclusion case,
  with no receiver type the heuristic emits an edge to *every* same-named method;
  SCIP keeps the one the receiver's type selects. The misses are namespaced calls
  to free *functions* (`NS.load()`, and the overloaded `NS.parse()`, now aligned
  through the overload-tolerant merge), and calls into the external `typescript`
  lib, which the method-only rule cannot see; SCIP resolves them. (The two
  `NS.parse` sites moved coverage from 50 to 52: they are recall misses the
  method-only rule cannot make, so honestly counting them is what nudged overall
  recall from 0.64 down to 0.62.)
- `ThisMethod` recall 0.50, precision **1.00** (now 8 sites, calibrated): the
  misses are **inherited** `this.describe()` / `this.speak()` calls on subclasses:
  the enclosing-class-only rule does not climb the inheritance chain; SCIP
  resolves them to the ancestor definition. Every `this.`-edge it *did* emit was
  correct.

## Per-band calibration (spec A4 / design §4.1): "does 0.9 mean ≥ 90%?"

The per-class table above scores the heuristic *branch*. This section answers the
**calibration** question directly: for each design §4.1 **confidence band**, does
an edge that *claims* that band actually resolve correctly at the rate the band
promises? This is the measurement Track C2 was assigned and the input that
resolves §15.6 (below).

Each call site is binned by the band the **heuristic edge** for that site claims
(its `call_confidence` provenance, the single source of truth the builder uses):
`BareSingle`/`ThisMethod` → **INFERRED** (0.40–0.80); `BareMulti`/
`UnknownReceiver` → **AMBIGUOUS** (< 0.40). SCIP is the **oracle** that grades
each edge, not the producer of the edge being graded. The band precision is
**edge-level** (`confirmed / (confirmed + denied)`): an over-included edge SCIP
rejects is a *denial*, so a band that over-includes scores low, the honest
answer to "does an edge in this band resolve correctly?".

Two honesty rules are enforced in code (`differential.rs`, `accuracy_gate.rs`):

- **Undefined ≠ perfect.** A band with **no adjudicable edges** reports precision
  `--` (undefined), never a vacuous 1.00 or 0.00. The heuristic never emits
  `RESOLVED`/`EXTRACTED`-grade edges (those tiers are the compiler's and the
  deterministic AST's, "an inference can never masquerade as a fact"), so over a
  *heuristic* corpus those two top bands are always undefined.
- **Unadjudicable sites are surfaced, not assumed.** A site SCIP cannot
  ground-truth (e.g. a call into an `any`-typed dynamic construct) is tallied in
  its band's `unadjudicable` column and **excluded** from the precision
  denominator, never silently counted as confirmed or denied.

Measured 2026-06-13 over the committed corpus, after the overload-alignment fix
(**52 SCIP-covered sites, 4 unadjudicable**):

| band | adjudicable sites | confirmed | denied | measured precision | unadjudicable | claim (§4.1) |
|---|---:|---:|---:|---:|---:|---|
| `RESOLVED` | 0 | 0 | 0 | -- (undefined) | 0 | 0.90–1.0, *no heuristic edge* |
| `EXTRACTED` | 0 | 0 | 0 | -- (undefined) | 0 | 0.95–1.0, *no heuristic edge* |
| `INFERRED` | 28 | 15 | 0 | **1.00** | 0 | 0.40–0.80 |
| `AMBIGUOUS` | 24 | 17 | 15 | **0.53** | 4 | < 0.40 |

**Monotonicity invariant (asserted, not hoped):** measured precision must be
non-increasing down the bands: `RESOLVED` ≥ `EXTRACTED` ≥ `INFERRED` ≥
`AMBIGUOUS`. A higher-confidence band that resolves *less* reliably than a lower
one means the confidence ordering is lying, and
`AccuracyReport::check_band_monotonicity` fails the build on it. Here **INFERRED
1.00 ≥ AMBIGUOUS 0.53** holds: the single-candidate guess tier out-resolves the
over-included tier, exactly as the bands promise. (The two undefined top bands
carry no claim and are skipped.) Both populated bands now exceed 20 adjudicable
sites, so this is a meaningful calibration rather than a one-site artefact.

The four unadjudicable sites are all **genuine SCIP gaps**: the `any`-typed
dynamic call in `resolve/`, the `any`-typed `thing.area()` in `dynamic/`, and the
two namespace-through-reexport calls (`pkg.alpha`/`pkg.delta`): SCIP itself emits
no first-party ground-truth edge (an `any` receiver, or a re-export resolving out
of the first party). They are excluded from the precision denominator (the
heuristic emits no edge for them), so they never inflate a precision number.

(An earlier *fifth* and *sixth* unadjudicable site, the overloaded `NS.parse(...)`
pair, was a Strata limitation, **now fixed**. SCIP resolves both to `src/lib.ts`,
but our SCIP-merge keyed a definition by `(file, line, name)` while SCIP points at
the overload **signature** line and our extractor records the **implementation**
line, so the exact key missed and the sites were dropped as uncomparable. The
merge now falls back to a unique `(file, name)` match when the exact line key
misses (overload-tolerant, but declining when the name is ambiguous in the file
so distinct same-name symbols are never merged), and both sites are adjudicated,
moving `covered` from 50 to 52. They are genuine recall misses, now honestly
counted rather than hidden as uncomparable.)

### §15.6 resolution: the default "will break" threshold, set by measurement

Design §15.6 (recall-vs-noise default) asked where the `impact`/`detect_changes`
label should flip from **"will break"** to **"may be affected, review"**. Per the
roadmap (§14.1 Track C2) this is resolved *empirically*: the cutoff is the lowest
band whose **measured** precision crosses the will-break bar.

INFERRED measures **1.00** and AMBIGUOUS measures **0.53**, so the boundary sits
at the **INFERRED band floor = 0.40** (§4.1): edges with confidence **≥ 0.40**
(INFERRED and above, measured 1.00 in-band here) cross the will-break bar.
AMBIGUOUS edges are "may be affected, review", and the engine excludes them two
ways that agree: by **provenance** (the `!ambiguous` guard in `will_break_label`),
and because §4.1 **caps every AMBIGUOUS edge's stored confidence below 0.40**, so
they fall under the threshold by construction. The 0.53 is the *measured precision*
of the band (too noisy to call a break), not a stored confidence being compared to
0.40. This measured cutoff is recorded as
`strata_core::traverse::DEFAULT_WILL_BREAK_CONFIDENCE` with its justification in the
doc comment.

**Status of the label (shipped):** the *threshold* is established and tested, and
the *label* is now emitted. Every `AffectedNode` carries a derived `will_break`
field, stamped in `impact` from this constant, and re-derived in `detect_changes`
after cross-symbol aggregation, both via `strata_core::will_break_label`, and it
is surfaced through the MCP impact tool JSON (additive field), the CLI
`impact`/`detect-changes` printers (a "WILL BREAK" / "may affect" verdict column),
and the desktop impact table. It remains a classification, never a filter.

Crucially, this governs the **label only**. `impact`'s default stays
recall-biased (`min_confidence = 0.0`): it still surfaces *everything*, AMBIGUOUS
paths included and flagged; the threshold decides what is called a break, not
what is shown. (See design §15.6, now marked *resolved by measurement:
0.40, 2026-06-12*.)

## Calibrated confidences (spec A4)

Each heuristic class's **stored** edge confidence (`build.rs` `CONF_*`) is
computed as `min(measured_precision, provenance-band ceiling)` per design §4.1.
Calibration informs the number *within* its band; it cannot break the band.
A heuristic edge (Inferred or Ambiguous) must never reach or exceed a RESOLVED
(0.97) or EXTRACTED (1.0) confidence: "an inference can never masquerade as a
fact." Under-populated classes (< 5 sites) keep their slice-1 prior and are
tagged `uncalibrated`.

The **precision/recall metrics** in the table above reflect the heuristic's raw
measured accuracy against SCIP; they are not affected by the band cap. The
stored confidence column below shows the band-capped value that is actually
written to graph edges.

| constant | raw measured precision | stored confidence | provenance |
|---|---:|---:|---|
| `CONF_BARE_SINGLE` | **1.00** (20 sites) | **0.80** | Inferred band ≤ 0.80; `min(1.00, 0.80)` |
| `CONF_BARE_MULTI` | 0.50 (1 site) | 0.35 | slice-1 prior, `uncalibrated`; already in Ambiguous band |
| `CONF_THIS_METHOD` | 1.00 (8 sites) | 0.80 | calibrated; at the Inferred ceiling, in-band |
| `CONF_UNKNOWN_RECEIVER` | **0.53** (23 sites) | **0.39** | Ambiguous band < 0.40; `min(0.53, 0.39)` |

`CONF_BARE_SINGLE`: raw measured precision is 1.00 (every emitted bare-single
edge correct across 20 sites), but a stored confidence of 1.00 would breach the
Inferred ceiling and outrank RESOLVED/EXTRACTED edges. Capped to 0.80 (the
Inferred ceiling) per §4.1. It is a measured starting point, not a claim that
single-candidate bare calls are *never* wrong; it will move as the corpus grows.

`CONF_UNKNOWN_RECEIVER`: raw measured precision is 0.53 (over-inclusion across 23
UnknownReceiver sites), but 0.53 exceeds the Ambiguous ceiling (< 0.40) and would
outrank Inferred-provenance edges. Capped to 0.39 per §4.1.

(The stored `CONF_*` constants are unchanged in this slice; Slice 13 adds the
*band* calibration + the §15.6 threshold; re-deriving the per-class stored
confidences from the larger sample is deferred. The raw measured numbers above
are refreshed for honesty; the band caps still hold.)

SCIP-resolved (`RESOLVED`) edges remain at the fixed 0.97 compiler-grade
confidence and are entirely unaffected by this calibration.

The `tests/confidence_bands.rs` guardrail iterates all edges in both the
heuristic and resolved-mode graphs and asserts the §4.1 band invariant per
provenance, so this cap cannot regress silently.

## CI floors

`corpus_meets_documented_floors` gates (re-derived over the corpus after the
overload-alignment fix): overall recall ≥ 0.61 (measured 0.62 = 32/52), overall
precision ≥ 0.65, `BareSingle` precision ≥ 1.00, `ThisMethod` precision ≥ 1.00
(now 8 sites), `UnknownReceiver` precision ≥ 0.53 (now 23 sites), and SCIP
coverage ≥ 52 covered sites. Per-class floors sit at the measured values (the
corpus is deterministic).

It additionally gates the **per-band calibration** (≥ 5 adjudicable sites
required to gate a band, so a band is never gated on noise): `INFERRED`
precision ≥ **0.85** (measured 1.00, margin 0.15) and `AMBIGUOUS` precision ≥
**0.40** (measured 0.53, margin 0.13): floors set from the MEASURED numbers
minus a documented honesty margin, never aspirational. The §4.1 monotonicity
invariant (`RESOLVED` ≥ `EXTRACTED` ≥ `INFERRED` ≥ `AMBIGUOUS`) is asserted in
the same gate. All floors are re-derived from this report whenever the corpus
changes.
