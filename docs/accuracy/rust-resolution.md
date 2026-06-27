# Rust Call-Resolution Accuracy

Measured accuracy of Strata's Rust **heuristic** call resolution
(`strata-lang-rust`'s band-disciplined linker), scored against **SCIP
(`rust-analyzer scip`) as ground truth**, per design §4.1 confidence band. This
is the Rust third of Track C1 (cross-language measured precision); the
TypeScript and Python parts are `ts-resolution.md` / `py-resolution.md`, and the
harness is shared.

The numbers are produced by `resolve_differential_graph` + `accuracy_report`
over a committed, hermetic corpus (each project ships its sources + a committed
`index.scip`; **no rust-analyzer runs at test time**). They are kept honest two
ways, by the **same** harness the TS/Python gates use (`tests/support/accuracy.rs`):

- **`tests/rust_accuracy_gate.rs::corpus_meets_documented_floors`** fails the
  build if any gated band regresses below its floor, or if a gated band goes
  vacuous.
- **`tests/rust_accuracy_gate.rs::report_matches_committed_doc`** asserts the
  live metrics equal the machine-readable companion `rust-resolution.json` (the
  same numbers tabulated below), so this report cannot silently drift.

Ground truth: **`rust-analyzer` 1.96.0** (recorded in `rust-resolution.json`).
Regenerate the raw figures with:

```
cargo test -p strata-index --test rust_accuracy_gate -- --nocapture print_rust_corpus_report
```

Regenerate the committed indexes (needs rust-analyzer) with:

```
STRATA_SCIP_LIVE=1 cargo test -p strata-index --test gen_scip -- --ignored generate_rust
```

## The moniker shim (the one Rust-specific piece)

rust-analyzer encodes impl methods as `…/impl#[Type]method().` (inherent) and
`…/impl#[Type][Trait]method().` (trait impl). The descriptor before the `(` then
ends in a `[Type]`/`[Type][Trait]` bracket run, so the pre-existing
scip-typescript/python moniker extractor returned `None` for it (~the methods
would be invisible to the SCIP→node merge). Track C1 adds a ~6-line shim to
`scip_merge::symbol_name_from_moniker`: when a descriptor segment starts with
`[`, strip the leading bracket groups and take the trailing identifier
(`new`/`area`/`scale`/`describe`). TypeScript and Python monikers never start a
segment with `[`, so they are provably untouched (pinned by
`scip_merge::tests::ts_and_py_monikers_unchanged_by_rust_shim`). Everything else
in the harness is the language-agnostic differential reused unchanged.

## How the measurement generalizes (the harness)

Identical to the Python gate: the Rust linker (`assemble_rust`) owns Rust call
resolution, so the faithful, no-drift source of the heuristic decision is the
**assembled Rust graph itself**. `resolve_differential_graph` reads back, per
call site, the exact heuristic `CALLS` edges the linker emitted and the band they
claim; rust-analyzer's SCIP is mapped onto the `rust`-tagged nodes via the shared
`(file, line, name)` alignment (now incl. the impl-method shim). The per-site
outcomes feed the **same** `accuracy_report` / `by_band` / monotonicity code as
TS and Python.

## Honesty / scope caveat

**The corpus is modest (33 call sites, 30 SCIP-adjudicable).** These numbers are
a *starting calibration*, not a statistically authoritative accuracy claim. The
durable deliverables are the language-parametric harness, the per-band metric
definition, the CI gate, and this report, all of which sharpen automatically as
the corpus grows. The **per-band** view is the meaningful calibration unit, and
all three heuristic-reachable bands are non-vacuously populated (EXTRACTED 11,
INFERRED 10, AMBIGUOUS 9 adjudicable sites, each ≥ 5, the gating threshold).

## The metric (exact definition)

Identical to the TS/Python metric, over **call sites that rust-analyzer
resolves** (uncovered sites are excluded from precision and counted as
`unadjudicable` in their band):

- `scip_target(site)`: the node `rust-analyzer` resolves the callee to.
- `heuristic_targets(site)`: the set of nodes the Rust linker emits `CALLS`
  edges to for that site, read back from the assembled graph.
- Band precision = `confirmed / (confirmed + denied)` over the adjudicable edges;
  **undefined** (`--`) when a band has no adjudicable edges.

The Rust linker's resolution rules map to bands as follows (see
`strata-lang-rust/src/link.rs`):

| rule | trigger | band (provenance) |
|---|---|---|
| same-file def | bare `f()` / type-qualified call resolving to a same-file def | **EXTRACTED** (0.95) |
| `self.m()` / `Self::m()` | own-type method on the enclosing impl | **INFERRED** (0.80) |
| type-qualified `Type::m()` | unique type+method match in another file | **INFERRED** (0.80) |
| unique cross-module name | bare `f()` with exactly one repo-wide fn/method | **INFERRED** (0.80) |
| ambiguous fan-out | several same-named candidates, instance-receiver `obj.m()`, trait dispatch | **AMBIGUOUS** (0.35) |
| unresolved | unknown name, `self.ghost()`, a `.m()` with no repo candidate, a macro (never a call) | *no edge* |

RESOLVED carries **no** heuristic edge (it is rust-analyzer/compiler grade, a
later compiler-precision slice). Like the Python linker (and unlike the TS
heuristic), the Rust linker *does* emit EXTRACTED-band edges, because a same-file
simple-name call to a same-file definition is the strongest static signal
without the type system; so EXTRACTED is a gated, populated band here.

## Corpus

Two committed cargo crates under
`crates/strata-index/tests/fixtures/accuracy/rust-corpus/` (each an **isolated
workspace** via an empty `[workspace]` table, so `rust-analyzer scip .` gets a
clean `cargo metadata`; `target/`/`Cargo.lock` are gitignored and never
committed, and the committed `index.scip` is the hermetic artifact):

- **`shapes/`**: `Rectangle`/`Circle` structs with impl methods, a `Shape` trait
  implemented by both. Type-qualified `Rectangle::new`/`Circle::new`
  constructors (INFERRED, resolved exactly), `self.area()`/`self.normalized()`
  own-type calls (INFERRED), same-file helper calls (EXTRACTED), `.area()` and
  `.scale()` on instance receivers, two same-named methods each → AMBIGUOUS
  fan-out rust-analyzer narrows, and `.describe()` **trait dispatch** → AMBIGUOUS
  fan-out across both impls **and** the trait signature (3 candidates), narrowed
  by rust-analyzer to the concrete impl.
- **`registry/`**: a `Store` and a `Job`/`Tally`. Same-file helpers
  (`empty`/`fold`/`reduce`/`stage_one`/`stage_two`/`Store::new`, EXTRACTED),
  `self.snapshot()`/`self.payload()` own-type calls (INFERRED), a cross-module
  type-qualified `Store::new()` (INFERRED), a unique cross-module bare `reduce()`
  (INFERRED), and a `.sum()` fan-out across `Store::sum`/`Tally::sum` (AMBIGUOUS,
  narrowed by rust-analyzer).

## Results

Measured 2026-06-14 over the committed corpus: **33 call sites, 30
SCIP-covered, 3 unadjudicable**.

| band | adjudicable sites | confirmed | denied | measured precision | unadjudicable | claim (§4.1) |
|---|---:|---:|---:|---:|---:|---|
| `RESOLVED` | 0 | 0 | 0 | -- (undefined) | 0 | 0.90–1.0, *no heuristic edge* |
| `EXTRACTED` | 11 | 11 | 0 | **1.00** | 0 | 0.95–1.0 |
| `INFERRED` | 10 | 10 | 0 | **1.00** | 1 | 0.40–0.80 |
| `AMBIGUOUS` | 9 | 9 | 10 | **0.47** | 2 | < 0.40 |
| **overall** | 30 | -- | -- | **precision 0.75, recall 1.00** | 3 | -- |

Reading the bands:

- **EXTRACTED 1.00** (11 sites): every same-file call resolved exactly as
  rust-analyzer did, the strongest static signal, no denials.
- **INFERRED 1.00** (10 adjudicable, 1 unadjudicable): type-qualified
  `Type::new()` constructors, `self.`-methods, and the unique cross-module
  `reduce()` were all confirmed. The 1 unadjudicable is a `Store::new()` reached
  via `Default::default()` that rust-analyzer routes differently, a recall miss,
  surfaced, never inflating precision.
- **AMBIGUOUS 0.47** (9 adjudicable, 2 unadjudicable): the over-inclusion case.
  With no receiver type the linker fans out to every same-named method;
  rust-analyzer keeps the one the receiver's concrete type selects. The
  `.describe()` **trait dispatch** is the sharpest example: the heuristic emits 3
  candidates (both impls + the trait signature), of which rust-analyzer confirms
  exactly one: 1 confirmed + 2 denied per call. The 2 unadjudicable are a
  `.clone()`/`.len()` on a std type (no first-party candidate, an external SCIP
  target, excluded).

**Monotonicity invariant (asserted, not hoped):** measured precision is
non-increasing down the bands: `EXTRACTED 1.00 ≥ INFERRED 1.00 ≥ AMBIGUOUS
0.47`. `AccuracyReport::check_band_monotonicity` fails the build on any
inversion.

## CI floors

`corpus_meets_documented_floors` gates each band at its **measured** precision
minus a documented honesty margin (never aspirational), and only gates a band
with ≥ 5 adjudicable sites:

- `EXTRACTED` ≥ **0.95** (measured 1.00, margin 0.05: a same-file fact must stay
  a fact).
- `INFERRED` ≥ **0.85** (measured 1.00, margin 0.15).
- `AMBIGUOUS` ≥ **0.40** (measured 0.47, margin 0.07: the will-break bar; trait
  dispatch and same-name fan-outs must never claim more).

The §4.1 monotonicity invariant is asserted in the same gate. All floors are
re-derived from this report whenever the corpus changes. The stored per-rule
confidence constants (`CONF_*` in `strata-lang-rust/src/link.rs`) are unchanged
by this slice; this report calibrates the **band** view (the cross-language
"reliably accurate" claim) and corroborates the §15.6 will-break threshold
(0.40): INFERRED-and-above resolve well above the bar, AMBIGUOUS sits below it,
consistent with TS and Python.
