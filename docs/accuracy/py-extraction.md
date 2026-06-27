# Python Code-Plane Extraction & Linking

Measured behaviour of Strata's Python code plane (`strata-lang-py`, Slice 9):
Tree-sitter-python extraction of functions/classes/methods/imports/calls, and
the **band-disciplined** intra-repo call/import linking layered on top. Python
links *within its own resolution world* this slice: there is no cross-language
edge to the TS/JS plane, and no compiler-grade ground truth (no SCIP), so the
honesty story here is about **what is linked, at what confidence, and what is
deliberately left unlinked**, not a precision/recall score against an oracle.

The numbers below are produced by `assemble_python` over a committed, hermetic
corpus (`crates/strata-index/tests/fixtures/accuracy/py/`). They are kept honest
two ways, the same discipline as `ts-resolution.md`:

- **`tests/py_extraction.rs::py_coverage_meets_documented_floors`** fails the
  build if any gated count regresses below its floor.
- **`tests/py_extraction.rs::py_coverage_matches_committed_numbers`** asserts the
  live `PyLinkCoverage` equals the numbers tabulated below, so this report cannot
  silently drift.

Regenerate the raw figures with:

```
cargo test -p strata-index --test py_extraction -- --ignored --nocapture print_py_coverage
```

## Honesty / scope caveat

**The corpus is small (6 call sites across a 4-module package).** These numbers
are a *starting calibration* of the linking behaviour, not a statistically
authoritative accuracy claim, exactly as the TS report is for its 18-site
corpus. The durable deliverables are the extraction crate, the band-capped
confidence constants, the per-outcome coverage counters, the CI gate, and this
report, all of which sharpen automatically as the corpus grows.

### What is deliberately NOT extracted/linked this slice

This is the load-bearing honesty of the plane (a missed link is surfaced, never
faked):

- **Contract plane (now linked; bounds stay honest).** Python emits all four
  contract signals (Flask/FastAPI/Django producer routes, `requests`/`httpx`
  consumer calls, `gql("…")` consumer documents, and Graphene/Strawberry/Ariadne
  resolver producers), linked by the shared language-agnostic linker at the same
  banded confidence as TS/JS. What stays **unlinked by design** (never guessed): a
  Django route is method-less, so it matches on path alone at a lower `Inferred`
  tier (0.65), and a cross-file view (`urls.py` → `views.py`) attributes the
  producer to the `urls` module, not the view; a non-`requests`/`httpx` HTTP client
  (a bare `client.get(...)`), a dynamic or f-string route path, and a resolver host
  not named `Query`/`Mutation`/`Subscription` are not matched.
- **Inheritance edges.** Class bases (`class Dog(Animal)`) are parsed for the
  class header but **no `Extends` edge is emitted**, identical to how the TS
  analyzer treats inheritance (the frozen `AnalyzedFile` model carries no bases
  field). A `self.m()` that resolves only via a base class is therefore an honest
  miss, not an invented edge.
- **Dynamic dispatch.** `getattr(obj, name)()`, star-import-bound calls, and
  monkey-patching are **never guessed**. A `getattr(...)()` callee is dropped at
  extraction; `from m import *` binds no name; an unknown receiver fans out only
  at the AMBIGUOUS band.
- **Absolute imports to external packages.** A `from <pkg> import x` only seeds a
  call binding when `<pkg>` resolves to a **repo** module file; an external
  package never invents a cross-package link.

## The model (resolution precedence)

A call site is resolved by the first rule that matches; the per-rule confidence
is a constant capped to its provenance band (design §4.1):

| rule | trigger | target | provenance · confidence |
|---|---|---|---|
| same-module | bare `f()`, `f` defined in this file | the local def | **Extracted 0.95** |
| self-method | `self.m()`, `m` on the enclosing class | that method | **Inferred 0.80** |
| import-matched | bare `f()`, `f` import-bound to a resolved repo module that defines it | that module's `f` | **Inferred 0.80** |
| bare-unique | bare `f()`, exactly one repo-wide function `f` | that function | **Inferred 0.80** |
| ambiguous | bare/unknown-receiver call with several same-named candidates | fan out to all | **Ambiguous 0.35** |
| (none) | unknown name / `self.ghost()` / dynamic target | n/a | **no edge** (counted) |

`self`-resolution and unknown-receiver resolution mirror the TS plane's
`this.method()` / `UnknownReceiver` rules exactly, with `self` in place of
`this`.

## Calibrated confidences (design §4.1)

Each constant is `min(measured precision, provenance-band ceiling)`; calibration
informs the number *within* its band and can never break it. A heuristic edge
must never reach or exceed a RESOLVED (0.97) or EXTRACTED-1.0 confidence: "an
inference can never masquerade as a fact."

| constant | value | provenance | band justification |
|---|---:|---|---|
| `CONF_SAME_MODULE` | **0.95** | Extracted | A same-file name binds to its local `def` deterministically, the strongest static signal. EXTRACTED **floor**, NOT 1.0: never outranks a RESOLVED 0.97 fact, and a re-binding after the def is an unchased miss. |
| `CONF_SELF_METHOD` | **0.80** | Inferred | `self.m()` names this class, but `m` may be overridden or inherited: a confident guess, capped at the Inferred ceiling. |
| `CONF_IMPORT_MATCHED` | **0.80** | Inferred | An import binding recovered without a type system; strong but heuristic → Inferred ceiling. |
| `CONF_BARE_UNIQUE` | **0.80** | Inferred | A single repo-wide name match with no import; a single guess, Inferred ceiling. |
| `CONF_AMBIGUOUS` | **0.35** | Ambiguous | Several same-named candidates the heuristic cannot disambiguate; fans out below the Ambiguous ceiling (< 0.40), an honest "could be any of these". |

The band invariant is guarded non-vacuously over Python edges by
`crates/strata-lang-py/tests/linking.rs::python_edges_satisfy_band_invariant_non_vacuously`
(which asserts the graph contains Extracted, Inferred, **and** Ambiguous Python
edges and that every one is in band) and, at the indexer level, by the
`tests/confidence_bands.rs` suite that iterates every edge in the assembled
graph.

## Coverage (committed corpus)

Measured over `tests/fixtures/accuracy/py/`, a 4-module package
(`pkg/__init__.py`, `pkg/models.py`, `pkg/service.py`) exercising every rule:
**6 call sites.**

| outcome | count | what it is |
|---|---:|---|
| `calls_total` | 6 | every call site considered |
| `calls_same_module` | 1 | `run()` → `helper()` (Extracted 0.95) |
| `calls_inferred` | 2 | `build_user()` → `make_user` via import; `User.save()` → `self.validate()` |
| `calls_ambiguous` | 1 | `acct.save()`: two `save` methods (`User`, `Account`) → fan-out |
| `calls_unresolved` | 2 | `getattr(...)()` dynamic target + `object()` bare name with no repo def (**no edge, surfaced**) |

The buckets do not sum to a tidy "resolved/unresolved" split by design: the two
`calls_unresolved` sites are the honesty signal, the dynamic `getattr` call and
an unknown bare name produce no edge rather than an invented one.

## CI floors

`py_coverage_meets_documented_floors` gates: `calls_total ≥ 6`,
`calls_same_module ≥ 1`, `calls_inferred ≥ 2`, `calls_ambiguous ≥ 1`, and
`calls_unresolved ≥ 2` (the two-sided honesty pin: a regression that invented a
confident edge for the dynamic call would drop the unresolved count and fail
here). Floors sit at the measured values (the corpus is deterministic); they are
re-derived from this report whenever the corpus changes.
