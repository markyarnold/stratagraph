# C# Code-Plane Extraction & Linking

Measured behaviour of Strata's C# code plane (`strata-lang-cs`, Slice 11):
Tree-sitter-c-sharp extraction of classes/interfaces/structs/records/methods/
usings/calls, and the **band-disciplined** intra-repo call/using linking layered
on top. C# links *within its own resolution world* this slice: there is no
cross-language edge to the TS/JS or Python plane, and **no compiler-grade ground
truth** (this is Tree-sitter, not Roslyn; Roslyn precision is Track A3). So the
honesty story here is about **what is linked, at what confidence, and what is
deliberately left unlinked**, not a precision/recall score against an oracle.

The numbers below are produced by `assemble_csharp` over a committed, hermetic
corpus (`crates/strata-index/tests/fixtures/accuracy/cs/`). They are kept honest
two ways, the same discipline as `py-extraction.md` / `ts-resolution.md`:

- **`tests/cs_extraction.rs::cs_coverage_meets_documented_floors`** fails the
  build if any gated count regresses below its floor.
- **`tests/cs_extraction.rs::cs_coverage_matches_committed_numbers`** asserts the
  live `CsLinkCoverage` equals the numbers tabulated below, so this report cannot
  silently drift.

Regenerate the raw figures with:

```
cargo test -p strata-index --test cs_extraction -- --ignored --nocapture print_cs_coverage
```

## Measured resolution precision: DEFERRED (no .NET SDK / scip-dotnet)

C# has **extraction-coverage only** today (this document); it has **no** measured
per-band *resolution* precision report (the C# analogue of `ts-resolution.md` /
`py-resolution.md` / `rust-resolution.md`). Producing one needs a compiler-grade
SCIP oracle, `scip-dotnet`, which requires the **.NET SDK**, not available in
this build environment. So the cross-language measured-precision corpus (Track
C1) covers **TypeScript, Python, and Rust**; C# resolution precision awaits the
.NET SDK + `scip-dotnet` (the same shape as the deferred Roslyn compiler-precision
track A3). The C# linker remains band-disciplined and honest by construction (the
coverage gate below pins what it links, at what confidence, and what it leaves
unlinked); it simply is not yet *graded against an oracle*.

## De-risk verdict (the grammar/core compatibility matrix)

The C# plane was gated on a hard compatibility question, proved before any code
was written:

| component | version | result |
|---|---|---|
| workspace `tree-sitter` core | **0.25.10** | the shared parser core (same as TS/Python) |
| `tree-sitter-c-sharp` grammar | **0.23.5** (latest published) | generates an **ABI-15** parser |
| `Parser::set_language` against core 0.25 | n/a | **Ok**: clean parse of a namespace+interface+class+method+invocation file, `has_error == false` |

The grammar crate's `0.23` semver is the *grammar's own* version, **not** its
Tree-sitter ABI: it targets ABI 15, which core 0.25 supports (ABI 13–15). So a
single shared `tree-sitter 0.25.10` is used by every language plane; the C#
grammar is the only new dependency, pinned exactly (`=0.23.5`). **Not blocked.**
The de-risk is itself pinned as a test
(`strata-lang-cs/src/lib.rs::wiring_smoke::parses_csharp`) so a future
grammar/core bump that breaks the ABI fails loudly rather than silently emptying
every `.cs` file.

## Honesty / scope caveat

**The corpus is small (7 call sites across a 3-file namespace).** These numbers
are a *starting calibration* of the linking behaviour, not a statistically
authoritative accuracy claim, exactly as the Python report is for its 6-site
corpus. The durable deliverables are the extraction crate, the band-capped
confidence constants, the per-outcome coverage counters, the CI gate, and this
report, all of which sharpen automatically as the corpus grows.

### What is deliberately NOT extracted/linked this slice

This is the load-bearing honesty of the plane: a missed link is surfaced, never
faked:

- **Roslyn precision is Track A3, not this slice.** No full overload resolution,
  no generic instantiation, no `partial`-type merging across files, no
  cross-assembly symbol resolution. This plane is Tree-sitter heuristics with
  honest provenance; every call edge is capped strictly below a RESOLVED
  (compiler) confidence.
- **Overloads collapse to one node per name.** `Run()` and `Run(int)` on the same
  type are two `RawSymbol`s with the **same** fqn (`T.Run`), and a call to `Run`
  links to that one collapsed node. Arity-aware splitting is an A3/Roslyn
  refinement; the flat-fqn collapse is the same precedent the Python and TS planes
  use.
- **Reflection and dynamic dispatch are never guessed.** `t.GetMethod("Run")` is
  recorded as an honest member call to `GetMethod` (receiver `t`); the `"Run"`
  string is an *argument*, never promoted to a call of a method named `Run`.
  `mi.Invoke(...)`, a `dynamic` receiver, and delegate indirection resolve to a
  same-named method only at the AMBIGUOUS band, or, with no repo candidate, to
  **no edge at all**.
- **Contract plane.** C# contributes **no** `routes`/`http_calls`/
  `gql_documents`/`resolver_entries`. ASP.NET routing attributes (`[HttpGet]`,
  `[Route]`) and `HttpClient`/`HttpRequestMessage` consumer calls are a later
  enhancement. A C# file is a full code-plane citizen but adds no contract
  producers/consumers yet.
- **Inheritance edges.** Type bases (`class Worker : Base, IService`) are parsed
  for the header but **no `Extends`/`Implements` edge is emitted**, identical to
  how the TS/Python analyzers treat inheritance (the frozen `AnalyzedFile` model
  carries no bases field). A `this.M()` that resolves only via a base type is
  therefore an honest miss, not an invented edge.
- **Infra `Runs` for C# (deferred).** A .NET Lambda `Handler` is
  `Assembly::Namespace.Type::Method` (e.g.
  `MyFunctions::MyFunctions.Handlers::Handle`), a CLR reference, **not a file
  path**. Resolving it to the `.cs` `Module` node the plane indexed needs the
  `.csproj`/assembly-name mapping, which is out of scope this slice. So `cs` is
  deliberately **absent from `HANDLER_EXTS`** (`infra.rs`), and a C# Lambda counts
  `lambdas_handler_unresolved`, surfaced honestly, pinned by
  `tests/infra_linking.rs::csharp_lambda_handler_is_unresolved_runs_deferred`.
  (Contrast the Python plane, whose file-path handlers EARNED their `Runs` edge in
  Slice 9.)

### The namespace fqn convention (the ONE chosen, documented, tested)

A symbol's fqn is **namespace-qualified and type-nested with `.`**: in
`namespace App.Services { class Worker { void Run() {} } }` the class fqn is
`App.Services.Worker` and the method fqn is `App.Services.Worker.Run`. The
namespace (file-scoped `namespace N;` or block `namespace N { … }`) is a
dotted prefix on every type/member fqn, and nested namespaces compose
(`A { B { … } }` → `A.B`). This mirrors the Python plane's dotted-container
convention and matches C#'s own fully-qualified-name syntax. Pinned by
`strata-lang-cs/tests/extraction.rs::namespace_qualifies_type_and_member_fqns`
(both forms) and `::nested_namespaces_compose_with_dots`.

`struct` and `record` (and `record struct`) extract as **`Class`-kind** nodes
(they are "a type with members" for the code graph); `interface` is its own
`Interface` kind. Constructors are `Method`s named after the type; local
functions are `Function`s with no container.

## The model (resolution precedence)

A call site is resolved by the first rule that matches; the per-rule confidence
is a constant capped to its provenance band (design §4.1):

| rule | trigger | target | provenance · confidence |
|---|---|---|---|
| same-file | bare `M()`, `M` defined in this file | the local def | **Extracted 0.95** |
| this-method | `this.M()`, `M` on the enclosing type | that method | **Inferred 0.80** |
| cross-file-unique | bare `M()`, exactly one repo-wide method/function `M` | that symbol | **Inferred 0.80** |
| ambiguous | bare/unknown-receiver call with several same-named candidates | fan out to all | **Ambiguous 0.35** |
| (none) | unknown name / `this.Ghost()` / reflection / dynamic target | n/a | **no edge** (counted) |

`this`-resolution and unknown-receiver resolution mirror the Python plane's
`self.m()` / `UnknownReceiver` rules exactly, with `this` in place of `self`.

**Why no `import-matched` rule (unlike Python).** A C# `using` imports a
*namespace*, not a specific symbol: `using System.Text;` does not name
`StringBuilder`. So there is no honest per-name import binding to seed a call
from; a cross-file bare call resolves through the **unique repo-wide name** rule
instead (still Inferred, still capped). A `using X = Y` alias binds one name and
is recorded, but resolving an alias to its (often external) target needs the type
system, deferred to Roslyn (A3), surfaced as a miss. The plane still emits a
best-effort, module-granular `using → IMPORTS` edge when a using's namespace
matches a repo file's declared namespace (visibility only; it seeds no call
binding), and **invents no edge** for an external namespace.

## Calibrated confidences (design §4.1)

Each constant is `min(measured precision, provenance-band ceiling)`; calibration
informs the number *within* its band and can never break it. A heuristic edge
must never reach or exceed a RESOLVED (0.97) or EXTRACTED-1.0 confidence: "an
inference can never masquerade as a fact."

| constant | value | provenance | band justification |
|---|---:|---|---|
| `CONF_SAME_FILE` | **0.95** | Extracted | A same-file simple-name call matching a same-file def is the strongest static signal without a compiler. EXTRACTED **floor**, NOT 1.0: never outranks a RESOLVED 0.97 Roslyn fact, and overload collapse means it may point at a name-collapsed node, not the exact overload. |
| `CONF_THIS_METHOD` | **0.80** | Inferred | `this.M()` names this type, but `M` may be `virtual`/overridden or inherited from an unseen base: a confident guess, capped at the Inferred ceiling. |
| `CONF_CROSS_FILE_UNIQUE` | **0.80** | Inferred | A single repo-wide name match (a `using` imports a namespace, not a symbol, so there is no stronger import binding): a single guess, Inferred ceiling. |
| `CONF_AMBIGUOUS` | **0.35** | Ambiguous | Several same-named candidates the heuristic cannot disambiguate; fans out below the Ambiguous ceiling (< 0.40), honest "could be any of these". Reflection/`dynamic` land here at most. |

The band invariant is guarded non-vacuously over C# edges by
`crates/strata-lang-cs/tests/linking.rs::csharp_edges_satisfy_band_invariant_non_vacuously`
(which asserts the graph contains Extracted, Inferred, **and** Ambiguous C# edges
and that every one is in band) and, at the indexer level, by the
`tests/confidence_bands.rs` suite that iterates every edge in the assembled
graph.

## Coverage (committed corpus)

Measured over `tests/fixtures/accuracy/cs/`, a 3-file namespace
(`Models.cs`, `Service.cs`, `Reflect.cs`) exercising every rule: **7 call
sites.**

| outcome | count | what it is |
|---|---:|---|
| `calls_total` | 7 | every call site considered |
| `calls_same_file` | 1 | `Service.Run()` → `Helper()` (Extracted 0.95) |
| `calls_inferred` | 2 | `this.Helper()` (this-method) + `Build()` (unique cross-file name) |
| `calls_ambiguous` | 1 | `acct.Save()`, two `Save` methods (`User`, `Account`) → fan-out |
| `calls_unresolved` | 3 | `Ghost()` unknown name + `t.GetMethod("Run")` + `mi.Invoke(...)` reflection: **no edge, surfaced** |

The buckets do not sum to a tidy "resolved/unresolved" split by design: the three
`calls_unresolved` sites are the honesty signal: the unknown bare name and the
two reflective calls produce no edge rather than an invented one, and the
reflected `"Run"` string is never promoted to a call.

## CI floors

`cs_coverage_meets_documented_floors` gates: `calls_total ≥ 7`,
`calls_same_file ≥ 1`, `calls_inferred ≥ 2`, `calls_ambiguous ≥ 1`, and
`calls_unresolved ≥ 3` (the two-sided honesty pin: a regression that invented a
confident edge for `mi.Invoke(...)` would drop the unresolved count and fail
here). Floors sit at the measured values (the corpus is deterministic); they are
re-derived from this report whenever the corpus changes.
