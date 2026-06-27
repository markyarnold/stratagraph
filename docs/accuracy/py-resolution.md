# Python Call-Resolution Accuracy

Measured accuracy of Strata's Python **heuristic** call resolution
(`strata-lang-py`'s band-disciplined linker), scored against **SCIP
(`scip-python`) as ground truth**, per design §4.1 confidence band. This is the
Python half of Track C1 (cross-language measured precision); the TypeScript half
is `ts-resolution.md`, and the harness is shared.

The numbers are produced by `resolve_differential_graph` + `accuracy_report`
over a committed, hermetic corpus (each project ships its sources + a committed
`index.scip`; **no Node/scip-python runs at test time**). They are kept honest
two ways, by the **same** harness the TS gate uses (`tests/support/accuracy.rs`):

- **`tests/py_accuracy_gate.rs::corpus_meets_documented_floors`** fails the build
  if any gated band regresses below its floor, or if a gated band goes vacuous.
- **`tests/py_accuracy_gate.rs::report_matches_committed_doc`** asserts the live
  metrics equal the machine-readable companion `py-resolution.json` (the same
  numbers tabulated below), so this report cannot silently drift.

Ground truth: **`scip-python` 0.6.6** (recorded in `py-resolution.json`).
Regenerate the raw figures with:

```
cargo test -p strata-index --test py_accuracy_gate -- --nocapture print_py_corpus_report
```

Regenerate the committed indexes (needs Node + network) with:

```
STRATA_SCIP_LIVE=1 cargo test -p strata-index --test gen_scip -- --ignored generate_py
```

## How the measurement generalizes from TypeScript (the harness)

The TS gate reproduces the *TS builder's* per-site decision graph-free, because
the TS builder owns call resolution. Python has its **own** linker
(`assemble_python`), so the faithful, no-drift source of the heuristic decision
is the **assembled Python graph itself**: the gate assembles the `py` plane and
`resolve_differential_graph` reads back, per call site, the exact heuristic
`CALLS` edges the linker emitted and the confidence band they claim. SCIP ground
truth is computed against the `py`-tagged symbol nodes, reusing the identical
`scip_position` / `symbol_name_from_moniker` / `(file, line, name)` alignment the
TS `scip_merge` path uses (those helpers are language-agnostic: **scip-python's
monikers parse with the existing extractor unchanged, no Python-specific shim**).
The resulting per-site outcomes feed the **same** `accuracy_report` / `by_band` /
monotonicity code as TS.

## Honesty / scope caveat

**The corpus is modest (45 call sites, 34 SCIP-adjudicable).** These numbers are
a *starting calibration*, not a statistically authoritative accuracy claim. The
durable deliverables are the (now language-parametric) harness, the per-band
metric definition, the CI gate, and this report, all of which sharpen
automatically as the corpus grows. The **per-band** view is the meaningful
calibration unit, and all three heuristic-reachable bands are non-vacuously
populated (EXTRACTED 9, INFERRED 15, AMBIGUOUS 10 adjudicable sites, each ≥ 5,
the gating threshold). A band with fewer than 5 adjudicable sites would be
reported but **not** gated (none are, here).

## The metric (exact definition)

Identical to the TS metric, over **call sites that SCIP resolves** (SCIP-uncovered
sites are excluded from precision and counted separately as `unadjudicable` in
their band):

- `scip_target(site)`: the node `scip-python` resolves the callee to.
- `heuristic_targets(site)`: the set of nodes the Python linker emits `CALLS`
  edges to for that site (0, 1, or many), read back from the assembled graph.
- Per site, in the **confidence band** the heuristic edge claims:
  - each heuristic target equal to `scip_target` is a **confirmed** edge;
  - each heuristic target `≠ scip_target` is a **denied** edge.
- Band precision = `confirmed / (confirmed + denied)`; **undefined** (`--`, never
  a vacuous 1.0) when a band has no adjudicable edges.

The Python linker's resolution rules map to bands as follows (see
`strata-lang-py/src/link.rs`):

| rule | trigger | band (provenance) |
|---|---|---|
| same-module def | bare `f()` resolving to a local `def` in the same file | **EXTRACTED** (0.95) |
| `self.m()` | own-class method on the enclosing class | **INFERRED** (0.80) |
| import-matched | bare `f()` bound by an import to a module that defines it | **INFERRED** (0.80) |
| unique bare name | bare `f()` with exactly one repo-wide function of that name | **INFERRED** (0.80) |
| ambiguous fan-out | bare name with several candidates, or any unknown-receiver `obj.m()` | **AMBIGUOUS** (0.35) |
| unresolved | unknown bare name, `self.ghost()`, `getattr(...)()`, `obj.m()` with no candidate | *no edge* |

Note RESOLVED carries **no** heuristic edge (it is SCIP/compiler grade, "an
inference can never masquerade as a fact"), so over a heuristic corpus it is
always undefined. Unlike the TS heuristic, which never claims EXTRACTED grade,
the Python linker *does* emit EXTRACTED-band edges, because a same-module bare
call to a local `def` is a deterministic name binding; so EXTRACTED is a gated,
populated band here.

## Corpus

Three committed packages under
`crates/strata-index/tests/fixtures/accuracy/py-corpus/` (self-contained,
hermetic, `scip-python`-indexable on bare sources (no venv/dependencies; no
`__pycache__` committed):

- **`shop/`**: a small layered app (`models` → `service` → `handlers`).
  Import-matched cross-module calls (`price_cart`, `checkout`, INFERRED),
  same-module helpers (`build_cart`, `envelope`, `sum_prices` → `fold_prices`,
  EXTRACTED), two classes sharing a `total` method called through
  **type-annotated** receivers (`cart: Cart`, `inv: Invoice`) that `scip-python`
  narrows while the heuristic fans out over both (AMBIGUOUS), single-candidate
  unknown-receiver `.tax()`/`.subtotal()` calls (AMBIGUOUS, confirmed), and a
  `getattr(report, name)()` dynamic dispatch the extractor drops (never guessed).
- **`geometry/`**: `Rectangle`/`Circle`, each with `area`/`scale`/`resize`/
  `describe`. `self.area()` / `self.resize()` own-class calls (INFERRED),
  constructor calls (`Rectangle(...)`) `scip-python` resolves to the class but the
  heuristic emits no edge for (recall misses), `.area()`/`.scale()` on
  **type-annotated** receivers (AMBIGUOUS, adjudicable) and on **untyped**
  parameters (AMBIGUOUS for the heuristic, an honest `scip-python` gap →
  unadjudicable), and a dynamic getattr call.
- **`pipeline/`**: module-level stages. Same-module `parse`→`tokenize`,
  `normalize`→`dedupe` (EXTRACTED), a unique-repo-wide bare `summarize()` reached
  without an import (INFERRED), an unknown bare `missing_stage()` resolving to
  nothing (unresolved, never invented), and `raw.split(",")` on a builtin
  (AMBIGUOUS attempt, unadjudicable).

## Results

Measured 2026-06-14 over the committed corpus: **45 call sites, 34
SCIP-covered, 11 unadjudicable**.

| band | adjudicable sites | confirmed | denied | measured precision | unadjudicable | claim (§4.1) |
|---|---:|---:|---:|---:|---:|---|
| `RESOLVED` | 0 | 0 | 0 | -- (undefined) | 0 | 0.90–1.0, *no heuristic edge* |
| `EXTRACTED` | 9 | 9 | 0 | **1.00** | 0 | 0.95–1.0 |
| `INFERRED` | 15 | 11 | 0 | **1.00** | 5 | 0.40–0.80 |
| `AMBIGUOUS` | 10 | 10 | 8 | **0.56** | 6 | < 0.40 |
| **overall** | 34 | -- | -- | **precision 0.79, recall 0.88** | 11 | -- |

Reading the bands:

- **EXTRACTED 1.00** (9 sites): every same-module bare call to a local `def`
  resolved exactly as `scip-python` did, the deterministic same-file binding the
  band promises. No denials.
- **INFERRED 1.00** (15 adjudicable, 5 unadjudicable): import-matched calls,
  `self.`-methods, and unique-name calls were all confirmed. The 5 unadjudicable
  are mostly **constructor calls** (`Cart(...)`, `Rectangle(...)`) and a
  unique-name call `scip-python` does not ground-truth: the heuristic emits no
  edge for a class constructor (its bare-name rule only indexes *functions*), so
  these are recall misses, surfaced; they never inflate precision.
- **AMBIGUOUS 0.56** (10 adjudicable, 6 unadjudicable): the over-inclusion case.
  With no receiver type the linker fans out to *every* same-named method;
  `scip-python` (via type annotations / local construction) keeps the one the
  receiver's type selects, so each two-candidate fan-out scores one confirmed +
  one denied. The single-candidate unknown-receiver calls (`.tax()`,
  `.subtotal()`) are confirmed. The 6 unadjudicable are untyped-receiver
  fan-outs and the `str.split()` builtin call: genuine `scip-python` gaps,
  excluded from the denominator.

**Monotonicity invariant (asserted, not hoped):** measured precision is
non-increasing down the bands: `EXTRACTED 1.00 ≥ INFERRED 1.00 ≥ AMBIGUOUS
0.56`. `AccuracyReport::check_band_monotonicity` fails the build on any
inversion.

## CI floors

`corpus_meets_documented_floors` gates each band at its **measured** precision
minus a documented honesty margin (never aspirational), and only gates a band
with ≥ 5 adjudicable sites:

- `EXTRACTED` ≥ **0.95** (measured 1.00, margin 0.05: a same-module fact must
  stay a fact).
- `INFERRED` ≥ **0.85** (measured 1.00, margin 0.15).
- `AMBIGUOUS` ≥ **0.40** (measured 0.56, margin 0.16: the will-break bar; an
  over-included fan-out must never claim more).

The §4.1 monotonicity invariant is asserted in the same gate. All floors are
re-derived from this report whenever the corpus changes. The stored per-rule
confidence constants (`CONF_*` in `strata-lang-py/src/link.rs`) are unchanged by
this slice; this report calibrates the **band** view (the cross-language
"reliably accurate" claim) and is the Python companion to the TS §15.6
will-break threshold (0.40), which it corroborates: INFERRED-and-above resolve
well above the bar, AMBIGUOUS sits below it.
