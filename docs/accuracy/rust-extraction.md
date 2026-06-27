# Rust Code-Plane Extraction & Linking

Measured behaviour of Strata's Rust code plane (`strata-lang-rust`, Slice 21):
Tree-sitter-rust extraction of fns/structs/enums/unions/traits/impls/mods/uses/
calls, and the **band-disciplined** intra-repo call/use linking layered on top.
Rust links *within its own resolution world* this slice: there is no
cross-language edge to the TS/JS, Python, or C# plane, and **no compiler-grade
ground truth** (this is Tree-sitter, not rust-analyzer/SCIP; compiler precision is
a later slice). So the honesty story here is about **what is linked, at what
confidence, and what is deliberately left unlinked**, not a precision/recall score
against an oracle.

The numbers below are produced by `assemble_rust` over a committed, hermetic
corpus (`crates/strata-index/tests/fixtures/accuracy/rust/`). They are kept honest
two ways, the same discipline as `cs-extraction.md` / `py-extraction.md` /
`ts-resolution.md`:

- **`tests/rust_extraction.rs::rust_coverage_meets_documented_floors`** fails the
  build if any gated count regresses below its floor.
- **`tests/rust_extraction.rs::rust_coverage_matches_committed_numbers`** asserts
  the live `RustLinkCoverage` equals the numbers tabulated below, so this report
  cannot silently drift.

Regenerate the raw figures with:

```
cargo test -p strata-index --test rust_extraction -- --ignored --nocapture print_rust_coverage
```

## De-risk verdict (the grammar/core compatibility matrix)

The Rust plane was gated on a hard compatibility question, proved before any code
was written:

| component | version | result |
|---|---|---|
| workspace `tree-sitter` core | **0.25.10** | the shared parser core (same as TS/Python/C#) |
| `tree-sitter-rust` grammar | **0.24.2** (latest published) | loads cleanly into core 0.25 |
| `Parser::set_language` against core 0.25 | n/a | **Ok**: clean parse of a free fn / struct / enum / union / trait+sigs / impl Type / impl Trait for Type / mod / use-with-aliases-groups-globs-prefixes / calls (incl. `Type::assoc`, `self.m`, `obj.m`, chained) / macro file, `has_error == false` |

The grammar crate's `0.24` semver is the *grammar's own* version, **not** its
Tree-sitter ABI: it targets an ABI core 0.25 supports. So a single shared
`tree-sitter 0.25.10` is used by every language plane; the Rust grammar is the only
new dependency, pinned exactly (`=0.24.2`). **Not blocked.** The de-risk is itself
pinned as a test (`strata-lang-rust/src/lib.rs::wiring_smoke::parses_rust`) so a
future grammar/core bump that breaks the ABI fails loudly rather than silently
emptying every `.rs` file.

## Honesty / scope caveat

**The corpus is small (8 call sites across a 3-file module set).** These numbers
are a *starting calibration* of the linking behaviour, not a statistically
authoritative accuracy claim, exactly as the C#/Python reports are for their small
corpora. The durable deliverables are the extraction crate, the band-capped
confidence constants, the per-outcome coverage counters, the CI gate, and this
report, all of which sharpen automatically as the corpus grows.

### What is deliberately NOT extracted/linked this slice

This is the load-bearing honesty of the plane (a missed link is surfaced, never
faked):

- **Compiler precision is a later slice, not this one.** No trait-method
  resolution to a concrete impl, no generic monomorphisation, no macro expansion,
  no cross-crate symbol resolution. This plane is Tree-sitter heuristics with
  honest provenance; every call edge is capped strictly below a RESOLVED (compiler)
  confidence.
- **Macros are NEVER faked into calls (the load-bearing honesty pin).** A
  `foo!(…)` is a `macro_invocation` AST node, a *different kind* from a function
  call (`call_expression`). The extractor matches `call_expression` only, so
  `println!`, `vec!`, `assert_eq!`, and a user `my_macro!()` produce **no** call
  edge, and the macro's expansion is **never guessed**, even when the macro body
  textually names a real fn (`macro_rules! call_run { () => { run() } }` does not
  link to `run`). This is the Rust analogue of the C# plane's
  reflection-never-invented rule. `macro_rules!` definitions are not symbols this
  slice. The corpus's `macros.rs` contains three macro invocations that contribute
  **zero** call sites, the strongest form of the honesty signal (they never even
  enter the tally).
- **Instance `obj.m()` (field receiver) and trait dispatch are never resolved to a
  concrete impl.** A `.`-receiver call on a value (`obj.method()`, `acct.save()`),
  including a trait-object / generic / unknown-type receiver, resolves to
  same-named methods only at the AMBIGUOUS band (a fan-out), or with no repo
  candidate to **no edge at all**. The receiver names a value, not a type, so the
  heuristic cannot know its concrete type; picking one method needs receiver-type
  inference (track A3). This is the deliberate counterpart to the type-qualified
  precision below: an explicit `Type::method()` qualifier *does* resolve to exactly
  that type's method, because the type is named in the syntax.
- **Type-qualified `Type::method()` calls DO resolve precisely (slice 23).** A
  `::`-scoped call (`IndexStamp::read()`, `Foo::new()`, `Self::helper()`) carries a
  path qualifier distinguishable from a `.` field receiver (via
  `CallRef::receiver_is_path`), so the linker resolves it to exactly the method on
  the named type (or the enclosing type for `Self::`) instead of fanning out
  ambiguously to every same-named method. A type-name collision (two `Config`s each
  with the method) still fans out, honestly Ambiguous over just those. **When the
  type's method is not found, the qualifier shape decides what is honest, never a
  confident pick of an unrelated method.** A `Foo::bar()` whose named type `Foo`
  *exists* but owns no in-repo method `bar` (a trait method we cannot see, or a
  stale/typo call) stays honest: it fans out same-named methods at the **Ambiguous**
  band (or no edge if none); it is NOT confidently bound to a same-named `bar` on
  some unrelated type the qualifier never named. Only a qualifier naming **no** known
  type (a genuine module path `mod::func()`) falls back to the repo-wide name rule
  (which is what lets a free-fn call resolve). The linker tells the two apart with a
  repo-wide index of known type names; without it, a type qualifier lacking the
  method and a module qualifier are indistinguishable (both a path receiver with zero
  type-method matches), and binding the former to a unique free fn would be a
  confident-WRONG edge. This is a Tree-sitter heuristic (matched on the qualifier's
  last segment), capped at the Inferred band cross-file (Extracted only when the
  unique target is same-file), never a compiler fact.
- **Py/C#/TS have no equivalent (`.` is overloaded, deferred).** Only the Rust
  analyzer sets `receiver_is_path: true`, because Rust's grammar gives a `::`-scoped
  call (`scoped_identifier`) a *distinct node kind* from a `.` call
  (`field_expression`). In TS (`member_expression`), Python (`attribute`), and C#
  (`member_access_expression`) a static/class-qualified call `Type.method()` and an
  instance call `obj.method()` are the **same AST node**: separating them needs
  receiver-type inference (is the receiver a type or a value?), which a capitalized-
  name heuristic cannot do reliably. Those analyzers set `receiver_is_path: false`
  for every member call and defer type-qualified precision to A3; their extraction
  output is otherwise unchanged by this slice.
- **Contract plane.** Rust contributes **no** `routes`/`http_calls`/
  `gql_documents`/`resolver_entries`. actix/axum routing macros/attributes and
  `reqwest`/`hyper` consumer calls are a later enhancement. A Rust file is a full
  code-plane citizen but adds no contract producers/consumers yet.
- **Inheritance / trait-impl edges.** A type's trait impls (`impl Trait for Type`)
  are parsed to attribute the methods to `Type`, but **no `Implements` edge** is
  emitted (the frozen `AnalyzedFile` model carries no bases field), identical to
  how the TS/Python/C# analyzers treat inheritance. A `self.m()` that resolves only
  via a trait default method on an unseen trait is therefore an honest miss, not an
  invented edge.
- **Infra `Runs` for Rust (deferred).** A Rust (cargo-lambda) Lambda's `Handler` is
  conventionally `bootstrap` (the `provided.al2` entrypoint), and the deployed
  artifact maps to a Cargo **binary name** (`[[bin]] name` / `package.name` in
  `Cargo.toml`, e.g. via `cargo lambda build --bin <name>`), **not a `.rs` file
  path**. Resolving it to the `main.rs`/`bin/*.rs` `Module` node the plane indexed
  needs the `Cargo.toml` target-table mapping, which is out of scope this slice. So
  `rs` is deliberately **absent from `HANDLER_EXTS`** (`infra.rs`), and a Rust
  Lambda counts `lambdas_handler_unresolved`, surfaced honestly. (Contrast the
  Python plane, whose file-path handlers EARNED their `Runs` edge in Slice 9.)

### The fqn convention (the ONE chosen, documented, tested)

A symbol's fqn is **module-path-qualified and type-nested with `::`**, Rust's own
path syntax: in `mod outer { struct MyStruct; impl MyStruct { fn method(&self) {} }
}` the struct fqn is `outer::MyStruct` and the method fqn is
`outer::MyStruct::method`. The inline module path is a `::`-joined prefix on every
item fqn, and nested modules compose (`mod a { mod b { … } }` → `a::b`). A free
`fn` is `module::fn` (or its bare name at crate root). Pinned by
`strata-lang-rust/tests/extraction.rs::module_nesting_qualifies_fqns`.

`struct`, `enum`, and `union` extract as **`Class`-kind** nodes (they are "a type
with members / variants / fields" for the code graph); a `trait` is its own
`Interface` kind, and its method signatures (and default methods) are Methods of
the trait. An `impl Type { … }` / `impl Trait for Type { … }` attributes its `fn`s
to `Type` (the impl's self type, **generics stripped**: `impl<T> Container<T>` →
methods of `Container`, so `impl`s of different types keep distinct method fqns).
Associated fns (no `self`) and methods (`self`/`&self`/`&mut self`) are both
Methods, no special-casing beyond the container check. Pinned by
`::structs_enums_unions_are_class_kind`,
`::trait_is_interface_with_method_sigs_as_methods`,
`::impl_block_methods_attribute_to_the_self_type`,
`::impl_trait_for_type_attributes_methods_to_the_type`, and
`::generic_impl_strips_generics_from_self_type`.

## The model (resolution precedence)

A call site is resolved by the first rule that matches; the per-rule confidence is
a constant capped to its provenance band (design §4.1):

| rule | trigger | target | provenance · confidence |
|---|---|---|---|
| same-file | bare `f()`, `f` defined in this file | the local def | **Extracted 0.95** |
| self-method | `self.m()` **or** `Self::m()`, `m` on the enclosing type | that method | **Inferred 0.80** |
| type-qualified (same file) | `Type::m()`, the unique `Type`+`m` is in this file | that method | **Extracted 0.95** |
| type-qualified (cross file) | `Type::m()`, the unique `Type`+`m` is in another file | that method | **Inferred 0.80** |
| type-qualified (collision) | `Type::m()`, several types named `Type` each have `m` | fan out to those | **Ambiguous 0.35** |
| type-qualified (known type, no method) | `Type::m()`, `Type` is a known type but no `Type::m` exists in-repo | fan out same-named **methods** (honest, never a confident pick of an unrelated method); none → no edge | **Ambiguous 0.35** |
| cross-module-unique | bare `f()` **or** scoped `mod::f()` (qualifier names **no** type) with no type match, exactly one repo-wide fn/method `f` | that symbol | **Inferred 0.80** |
| ambiguous | bare / module-qualifier fallback / known-type-lacks-method / unknown-receiver call with several same-named candidates | fan out to all | **Ambiguous 0.35** |
| (none) | unknown name / `self.ghost()` / `Type::m()` with no same-named method anywhere / unknown-receiver dispatch with no candidate | n/a | **no edge** (counted) |
| (n/a) | a `foo!(…)` macro | n/a | **not a call** (never tallied) |

The receiver shape, carried by `CallRef::receiver_is_path` (a `::`-scoped path
qualifier vs a `.` field receiver, set by the analyzer from the `scoped_identifier`
vs `field_expression` node), selects the rule. The crucial precision rule is
**type-qualified**: a scoped `Type::method()` names its type explicitly, so the
linker resolves to exactly that type's method (the qualifier's last `::` segment
matched against the method's owning-type last segment) instead of fanning out to
every same-named method. A `Self::m()` scoped call resolves like `self.m()` (the
enclosing type). When the named type owns no such method, the **qualifier shape**
decides, and the two cases must not be confused, or the linker would emit a
confident-WRONG edge. A `Foo::bar()` whose qualifier names a **known type** that
simply lacks the method stays honest (Ambiguous fan-out over same-named methods, or
no edge), never a confident bind to a same-named method on an unrelated type. Only
a qualifier naming **no** type (a free-fn call `mod::func()`) falls back to the
bare-name rule (which is what lets `mod::func()` resolve at all; it previously hit
the methods-only path and resolved to nothing). The linker distinguishes the two
with a repo-wide index of known type names (struct/enum/union/trait); both arrive as
a `::`-scoped path receiver with zero type-method matches, so without that index the
free-fn fallback would wrongly fire for a type qualifier lacking the method.

`self`-resolution and the **field-receiver** (instance `obj.m()`) unknown-receiver
fan-out mirror the C# plane's `this.M()` / unknown-receiver rules exactly, with
`self` in place of `this`.

**The remaining bound: instance `obj.m()` (field receiver) stays Ambiguous.** A
`.`-receiver call on a variable/expression (`obj.read()`, `acct.save()`) names a
*value*, not a type, so the heuristic cannot know the receiver's concrete type and
fans out to every same-named method at the Ambiguous band (or no edge with no
candidate). Resolving the receiver's type to pick the one method needs type
inference, deferred (track A3). This is the honest counterpart to the
type-qualified win: an explicit `Type::` qualifier resolves precisely; an instance
receiver does not.

**Why no per-name `import-matched` rule (unlike Python).** A `use crate::foo::bar`
names a path, but binding it to a *specific* repo symbol for call seeding needs the
module/type system (a `bar` could be a fn, a type, or a re-export). So a
cross-module bare call resolves through the **unique repo-wide name** rule instead
(still Inferred, still capped). The plane still emits a best-effort, module-granular
`use → IMPORTS` edge when a `use` path (with a leading `crate::`/`self::` root
stripped) matches a repo file's module path (visibility only; it seeds no call
binding), and **invents no edge** for an external-crate path.

## Calibrated confidences (design §4.1)

Each constant is `min(measured precision, provenance-band ceiling)`; calibration
informs the number *within* its band and can never break it. A heuristic edge must
never reach or exceed a RESOLVED (0.97) or EXTRACTED-1.0 confidence: "an inference
can never masquerade as a fact."

| constant | value | provenance | band justification |
|---|---:|---|---|
| `CONF_SAME_FILE` | **0.95** | Extracted | A same-file simple-name call matching a same-file def is the strongest static signal without a compiler. EXTRACTED **floor**, NOT 1.0: never outranks a RESOLVED 0.97 rust-analyzer fact, and the same-name match may point at a like-named item in another module within the file. |
| `CONF_SELF_METHOD` | **0.80** | Inferred | `self.m()` / `Self::m()` names this type, but `m` may come from a trait impl we cannot see or a blanket impl: a confident guess, capped at the Inferred ceiling. |
| `CONF_CROSS_MODULE_UNIQUE` | **0.80** | Inferred | A single repo-wide name match (resolving the exact `use`-path binding needs the type system, so there is no stronger import binding): a single guess, Inferred ceiling. |
| `CONF_TYPE_QUALIFIED` | **0.80** | Inferred | A `Type::method()` call resolved via the explicit `Type::` qualifier to the unique type+method match in another file. The author named the type, so this is stronger than a bare-name guess; but a type name can collide across modules, the method may be trait-inherited, and we match on the type's *last path segment*, so it is a confident heuristic, never a compiler fact: Inferred ceiling. (A same-file unique target earns `CONF_SAME_FILE` instead; a type-name collision degrades to `CONF_AMBIGUOUS`.) |
| `CONF_AMBIGUOUS` | **0.35** | Ambiguous | Several same-named candidates the heuristic cannot disambiguate; fans out below the Ambiguous ceiling (< 0.40), an honest "could be any of these". An instance `obj.m()` on an unknown-type receiver (and a `Type::m()` whose type name collides) lands here at most. |

The band invariant is guarded non-vacuously over Rust edges by
`crates/strata-lang-rust/tests/linking.rs::rust_edges_satisfy_band_invariant_non_vacuously`
(which asserts the graph contains Extracted, Inferred, **and** Ambiguous Rust edges
and that every one is in band) and, at the indexer level, by the
`tests/confidence_bands.rs` suite that iterates every edge in the assembled graph.

## Coverage (committed corpus)

Measured over `tests/fixtures/accuracy/rust/`, a 3-file module set
(`models.rs`, `service.rs`, `macros.rs`) exercising every rule: **8 call sites.**

| outcome | count | what it is |
|---|---:|---|
| `calls_total` | 8 | every call site considered (the three macros in `macros.rs` are NOT calls, so they are not here) |
| `calls_same_file` | 2 | `service::run()` → `helper()` + `macros::drive()` → `local_work()` (Extracted 0.95) |
| `calls_inferred` | 3 | `self.compute()` (self-method) + `build_one()` (unique cross-module name) + `User::save()` (type-qualified, exactly `User::save`, Inferred 0.80) |
| `calls_ambiguous` | 1 | `acct.save()`: instance receiver, two `save` methods (`User`, `Account`) → fan-out |
| `calls_unresolved` | 2 | `ghost()` unknown name + `acct.absent()` unknown-receiver dispatch with no candidate (**no edge, surfaced**) |

The contrast between the two `save` sites is the slice-23 demonstration: the
**instance** call `acct.save()` (line 4) is Ambiguous because the receiver names a
value of unknown type, while the **type-qualified** call `User::save()` (line 4b)
resolves to exactly `User::save` because the type is named in the syntax: the same
method, precise via the explicit qualifier.

The buckets happen to sum to `calls_total` here, but that is not enforced by
design: the two `calls_unresolved` sites are the honesty signal, the unknown bare
name and the no-candidate unknown-receiver dispatch produce no edge rather than an
invented one. And the **three macro invocations** in `macros.rs` are the strongest
honesty signal of all: they never even enter the tally, because a macro is not a
call.

## CI floors

`rust_coverage_meets_documented_floors` gates: `calls_total ≥ 8`,
`calls_same_file ≥ 2`, `calls_inferred ≥ 3`, `calls_ambiguous ≥ 1`, and
`calls_unresolved ≥ 2` (the honesty pin: a regression that invented a confident
edge for `ghost()` or `acct.absent()` would drop the unresolved count and fail
here; a regression that faked a macro into a call would raise `calls_total` and
fail the consistency test). Floors sit at the measured values (the corpus is
deterministic); they are re-derived from this report whenever the corpus changes.
