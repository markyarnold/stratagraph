//! The pure, deterministic Rust graph linker (band-disciplined).
//!
//! [`assemble_rust`] turns an already-analyzed Rust file set (a `BTreeMap` of
//! `path → AnalyzedFile`) into the Rust code-plane graph and adds it to a
//! [`Graph`]. It performs **no IO** and iterates in sorted order, so the same
//! input always yields the same graph. Rust links *within its own resolution
//! world* — calls bind to same-file defs, own-type `self.` methods, or unique
//! repo-wide names — never across into the TS/Python/C# planes this slice.
//!
//! **Confidence discipline (design §4.1) is LAW.** Every heuristic confidence is a
//! doc-commented constant capped to its provenance band; a heuristic edge can
//! never reach EXTRACTED-1.0 or a RESOLVED tier, and an ambiguous one is always
//! strictly below 0.40. **Macros and trait dispatch are never guessed**: a
//! `foo!(…)` macro is not even a call (the extractor drops it — it is a
//! `macro_invocation`, not a `call_expression`), and a method call on an
//! unknown-type / trait-object receiver fans out only at the AMBIGUOUS band, or
//! produces no edge at all. The Rust analogue of the C# plane's
//! reflection-never-invented rule.
//!
//! **Why no per-name `import-matched` call rule.** A `use crate::foo::bar` names a
//! path, but resolving it to a *specific* repo symbol for call seeding needs the
//! module/type system (a `bar` could be a fn, a type, a re-export). So a cross-file
//! bare call resolves through the **unique repo-wide name** rule instead (still
//! Inferred, still capped). The plane still emits a best-effort, module-granular
//! `use → IMPORTS` edge when a `use` path matches a repo file's module path
//! (visibility only; it seeds no call binding), and **invents no edge** for an
//! external crate path.

use std::collections::BTreeMap;

use strata_core::{
    AnalyzedFile, CallRef, Confidence, Edge, EdgeKind, Graph, Node, NodeKind, Provenance, Span, Uid,
};

/// The language/plane tag for Rust code-plane UIDs (distinct from TS `"ts"`,
/// Python `"py"`, and C# `"cs"`), so a mixed-language repo never collides UIDs
/// across planes.
const LANG: &str = "rust";

// ── Calibrated heuristic confidences (design §4.1) ───────────────────────────
//
// Stored confidence = min(measured precision, provenance-band ceiling): EXTRACTED
// band 0.95–1.0; INFERRED band 0.40–0.80; AMBIGUOUS band < 0.40. A heuristic edge
// must NEVER reach or exceed a RESOLVED (0.97) or EXTRACTED-1.0 confidence — "an
// inference can never masquerade as a fact." These are a *starting calibration*
// over the committed crate fixtures (corpus stated in
// `docs/accuracy/rust-extraction.md`); the band guardrail
// (`tests/linking.rs::rust_edges_satisfy_band_invariant_non_vacuously` and the
// indexer's `tests/confidence_bands.rs`) pins them in band.

/// A bare call `f()` whose target is a fn/method **defined in the same file**.
/// Rust name resolution inside one file is *not* as deterministic as a compiler's
/// (it depends on imports in scope, trait method resolution, and shadowing), but a
/// same-file simple-name call that matches a same-file definition is the strongest
/// static signal we have without the type system — graded at the EXTRACTED band
/// **floor**. `0.95`, NOT `1.0`: never outranks a RESOLVED (0.97) rust-analyzer
/// fact (a later compiler-precision slice), and the same-name match may point at a
/// like-named item in a different module within the file. Provenance: Extracted.
const CONF_SAME_FILE: f32 = 0.95;

/// A `self.m()` call resolved to a method `m` on the **enclosing type** (the impl
/// the call sits in). A confident heuristic guess (the receiver `self` names this
/// type), but still a guess — `m` may come from a trait `impl` we cannot see, or be
/// overridden by a blanket impl — so it sits at the INFERRED ceiling, never a fact.
/// The Rust analogue of the C# plane's `this.M()` rule and the Python plane's
/// `self.m()` rule. Provenance: Inferred.
const CONF_SELF_METHOD: f32 = 0.80;

/// A bare call `f()` with no same-file def, where exactly ONE fn/method named `f`
/// exists **repo-wide**. A single confident candidate recovered by name (resolving
/// the exact `use`-path binding needs the module/type system, so there is no
/// stronger import binding to lean on) — a single heuristic guess, capped at the
/// INFERRED ceiling, never a fact. Provenance: Inferred.
const CONF_CROSS_MODULE_UNIQUE: f32 = 0.80;

/// A **type-qualified** call `Type::method()` (a `::`-scoped path qualifier whose
/// last segment names a type) resolved to the UNIQUE type+method match in a
/// *different file* than the call. The explicit `Type::` qualifier is a strong
/// signal — far stronger than a bare same-name guess — because the author named the
/// type: we resolve to exactly the method on that type instead of fanning out to
/// every same-named method. Still a heuristic, never a compiler fact: a `Type` name
/// can collide across modules (two `Config`s), a method may be inherited from a
/// trait we cannot see, and we match on the type's *last path segment* not its full
/// path — so it sits at the INFERRED ceiling, never EXTRACTED/RESOLVED. (When the
/// unique target is in the SAME file, the call earns the stronger `CONF_SAME_FILE`
/// Extracted band instead; when several types share the name + method, it degrades
/// to an Ambiguous fan-out over just those.) Provenance: Inferred.
const CONF_TYPE_QUALIFIED: f32 = 0.80;

/// An **ambiguous** call: a bare name or unknown-receiver method with SEVERAL
/// same-named candidates the heuristic cannot disambiguate. It fans out to all
/// candidates at the AMBIGUOUS band (strictly < 0.40) — the honest "could be any of
/// these" rather than a confident wrong pick. Trait dispatch on an unknown-type /
/// trait-object receiver lands here at most, never higher. (A macro never reaches
/// resolution at all — it is not a call.) Provenance: Ambiguous.
const CONF_AMBIGUOUS: f32 = 0.35;

/// Per-repo Rust link coverage, surfaced for tests and (later) `IndexStats`.
///
/// A "site" is one call site. The outcome buckets do **not** necessarily sum to
/// `calls_total`: a call the heuristic cannot point anywhere (an unknown bare name,
/// a `self.ghost()` with no matching method, a method on an unknown receiver with
/// no repo candidate) counts only toward `calls_total` — surfaced by absence, never
/// invented. Macro invocations never enter this tally because the extractor never
/// records them as calls.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RustLinkCoverage {
    /// Every call site considered.
    pub calls_total: usize,
    /// Same-file Extracted resolutions.
    pub calls_same_file: usize,
    /// Inferred resolutions (self-method or unique cross-module name).
    pub calls_inferred: usize,
    /// Ambiguous fan-outs (several same-named candidates / unknown receiver).
    pub calls_ambiguous: usize,
    /// Sites that produced no edge (no candidate anywhere — incl. trait dispatch
    /// with no repo candidate).
    pub calls_unresolved: usize,
}

/// Add the Rust plane to `g` from an analyzed Rust file set.
///
/// Idempotent by UID. Creates a `rust`-tagged `Repo` node, a `Module` per file,
/// `Class`/`Interface`/`Function`/`Method` symbol nodes, the structural
/// `MEMBER_OF`/`DEFINES` pairs, module-granular `IMPORTS` edges for repo-resolvable
/// `use`s, and the band-disciplined `CALLS` edges. The Rust plane uses its own
/// `LANG` tag, so it never collides with the TS/Python/C# planes in a
/// mixed-language repo (cross-language linking is out of scope this slice).
pub fn assemble_rust(
    g: &mut Graph,
    repo_name: &str,
    analyzed: &BTreeMap<String, AnalyzedFile>,
) -> RustLinkCoverage {
    let mut cov = RustLinkCoverage::default();

    // ── Repo node (rust-tagged). ──
    let repo_uid = uid_repo(repo_name);
    g.add_node(extracted_node(
        repo_uid.clone(),
        NodeKind::Repo,
        repo_name,
        repo_name,
        "",
        Span::default(),
    ));

    // ── Phase 1: Module + symbol nodes, structural edges. ──
    for (path, file) in analyzed {
        let module_uid = uid_module(repo_name, path);
        g.add_node(extracted_node(
            module_uid.clone(),
            NodeKind::Module,
            base_name(path),
            "<module>",
            path,
            Span::default(),
        ));
        add_structural_pair(g, &module_uid, &repo_uid);

        for sym in &file.symbols {
            let sym_uid = uid_symbol(repo_name, path, &sym.fqn);
            g.add_node(extracted_node(
                sym_uid, sym.kind, &sym.name, &sym.fqn, path, sym.span,
            ));
        }
        // Structural edges after all symbol nodes in the file exist (a method's
        // impl type may be declared later in the file than the impl block).
        for sym in &file.symbols {
            let sym_uid = uid_symbol(repo_name, path, &sym.fqn);
            let container_uid = match &sym.container_fqn {
                Some(type_fqn) => {
                    let type_uid = uid_symbol(repo_name, path, type_fqn);
                    if g.get_node(&type_uid).is_some() {
                        type_uid
                    } else {
                        module_uid.clone()
                    }
                }
                None => module_uid.clone(),
            };
            add_structural_pair(g, &sym_uid, &container_uid);
        }
    }

    // ── Phase 2: import edges. A `use` names a path, not a file, so it resolves to
    // a repo Module only when a file's *module path* (encoded as the `::`-prefix of
    // its symbols' fqns) matches the `use` specifier (or a prefix of it, since
    // `use crate::foo::Bar` names a symbol inside module `crate::foo`). This is a
    // best-effort module-granular IMPORTS edge for visibility; it seeds NO call
    // binding (the unique-name rule does cross-module calls). A `use` of an external
    // crate path adds no edge — never an invented link. ──
    let mod_index = ModulePathIndex::build(repo_name, analyzed);
    for (path, file) in analyzed {
        let module_uid = uid_module(repo_name, path);
        for import in &file.imports {
            for target_module in mod_index.modules_for_use(&import.specifier) {
                if target_module != module_uid {
                    g.add_edge(import_edge(&module_uid, &target_module));
                }
            }
        }
    }

    // ── Index for call resolution. ──
    let index = CallIndex::build(repo_name, analyzed);

    // ── Phase 3: call edges (band-disciplined). ──
    for (path, file) in analyzed {
        for call in &file.calls {
            resolve_call(g, repo_name, path, file, call, &index, &mut cov);
        }
    }

    cov
}

/// Resolve one call site to its `CALLS` edge(s) and tally [`RustLinkCoverage`].
///
/// The receiver shape (carried by `CallRef`) selects the rule:
///
/// - **Bare call `f()`** (`receiver: None`):
///   1. **Same-file def** — a Function/Method named `callee` in this file →
///      Extracted 0.95, single edge per same-file def, done.
///   2. **Unique cross-module name** — exactly one repo-wide fn/method named
///      `callee` → Inferred 0.80; several → Ambiguous 0.35 fan-out; zero → no edge.
/// - **`self.m()` / `Self::m()`** (a `.`-self field receiver OR a `::`-scoped `Self`
///   qualifier) — a method `m` on the **enclosing type** → Inferred 0.80; none → no
///   edge (a trait-default method we cannot see — honest miss).
/// - **Type-qualified `Type::method()`** (`receiver_is_path`, qualifier ≠ `Self`) —
///   methods named `callee` on a type whose last path segment == the qualifier's
///   last segment: exactly 1 → exact edge (Extracted 0.95 if same file, else
///   Inferred `CONF_TYPE_QUALIFIED` 0.80); several (a type-name collision) →
///   Ambiguous 0.35 over just those; **zero** → branch on the qualifier: if it names
///   a KNOWN type (the type exists but lacks the method), stay honest — fan out
///   same-named methods at Ambiguous 0.35, never a confident pick of an unrelated
///   method; if it names NO type (a module path `mod::func()`), fall back to the
///   bare-name rule (unique → Inferred 0.80, several → Ambiguous, zero → no edge),
///   which resolves the genuine scoped free-fn call.
/// - **Field receiver `obj.m()`** (`receiver_is_path == false`, not self) — an
///   instance call on an unknown-type receiver: fan out to same-named **methods** at
///   Ambiguous 0.35; zero → no edge. Resolving the concrete receiver type needs type
///   inference (deferred), so this stays honestly ambiguous — never a confident pick.
///
/// Anything that points nowhere produces **no edge** — surfaced, never invented.
/// Macro invocations are not calls and never reach this function.
fn resolve_call(
    g: &mut Graph,
    repo_name: &str,
    path: &str,
    file: &AnalyzedFile,
    call: &CallRef,
    index: &CallIndex,
    cov: &mut RustLinkCoverage,
) {
    cov.calls_total += 1;
    let caller_uid = uid_enclosing(repo_name, path, &call.enclosing_fqn);

    match call.receiver.as_deref() {
        // ── Bare call `f()`. ──
        None => {
            // 1. Same-file def (Extracted) — strongest static signal.
            let local: Vec<Uid> = file
                .symbols
                .iter()
                .filter(|s| {
                    matches!(s.kind, NodeKind::Function | NodeKind::Method)
                        && s.name == call.callee_name
                })
                .map(|s| uid_symbol(repo_name, path, &s.fqn))
                .collect();
            let local = dedup(local);
            if !local.is_empty() {
                for target in &local {
                    add_call_edge(
                        g,
                        &caller_uid,
                        target,
                        Provenance::Extracted,
                        CONF_SAME_FILE,
                    );
                }
                cov.calls_same_file += 1;
                return;
            }

            // 3/4. Cross-module repo-wide: unique → Inferred, several → Ambiguous.
            let repo_wide = dedup(index.callables_named(&call.callee_name));
            match repo_wide.len() {
                0 => cov.calls_unresolved += 1, // unknown name — no edge, surfaced.
                1 => {
                    add_call_edge(
                        g,
                        &caller_uid,
                        &repo_wide[0],
                        Provenance::Inferred,
                        CONF_CROSS_MODULE_UNIQUE,
                    );
                    cov.calls_inferred += 1;
                }
                _ => {
                    for target in &repo_wide {
                        add_call_edge(
                            g,
                            &caller_uid,
                            target,
                            Provenance::Ambiguous,
                            CONF_AMBIGUOUS,
                        );
                    }
                    cov.calls_ambiguous += 1;
                }
            }
        }
        // ── A call with a receiver. The receiver shape decides the rule. ──
        Some(receiver) => {
            // `self`/`&self` (a `.` field receiver) OR a `::`-scoped `Self`
            // qualifier both name the ENCLOSING type — resolve to its method.
            let is_self = matches!(receiver, "self" | "&self")
                || (call.receiver_is_path && last_segment(receiver) == "Self");

            if is_self {
                resolve_self_method(g, repo_name, path, file, call, cov);
            } else if call.receiver_is_path {
                // ── Type-qualified `Type::method()`: the qualifier's last segment
                // names a type. Resolve to that type's method exactly. ──
                resolve_type_qualified(g, repo_name, path, file, call, index, cov);
            } else {
                // ── `obj.m()` — unknown receiver type: fan out to same-named
                // methods at the AMBIGUOUS band. This is where trait dispatch lands:
                // a `t.method()` on a trait-object / generic / unknown-type receiver
                // resolves to same-named methods only ambiguously — never a confident
                // concrete-impl dispatch; with no repo candidate it produces no edge.
                // Resolving the concrete receiver type needs type inference (deferred).
                let methods = dedup(index.methods_named(&call.callee_name));
                match methods.len() {
                    0 => cov.calls_unresolved += 1,
                    _ => {
                        for target in &methods {
                            add_call_edge(
                                g,
                                &caller_uid,
                                target,
                                Provenance::Ambiguous,
                                CONF_AMBIGUOUS,
                            );
                        }
                        cov.calls_ambiguous += 1;
                    }
                }
            }
        }
    }
}

/// Resolve a `self.m()` / `Self::m()` call to a method `m` on the **enclosing
/// type** (the impl the call sits in), at the Inferred self-method band. No
/// matching own-type method (a trait-impl method we cannot see, or `self`/`Self`
/// outside a type) → no edge, surfaced. Tallies into `cov`.
fn resolve_self_method(
    g: &mut Graph,
    repo_name: &str,
    path: &str,
    file: &AnalyzedFile,
    call: &CallRef,
    cov: &mut RustLinkCoverage,
) {
    let caller_uid = uid_enclosing(repo_name, path, &call.enclosing_fqn);
    if let Some(type_fqn) = enclosing_type(&call.enclosing_fqn) {
        let targets: Vec<Uid> = dedup(
            file.symbols
                .iter()
                .filter(|s| {
                    s.kind == NodeKind::Method
                        && s.name == call.callee_name
                        && s.container_fqn.as_deref() == Some(type_fqn)
                })
                .map(|s| uid_symbol(repo_name, path, &s.fqn))
                .collect(),
        );
        if !targets.is_empty() {
            for target in &targets {
                add_call_edge(
                    g,
                    &caller_uid,
                    target,
                    Provenance::Inferred,
                    CONF_SELF_METHOD,
                );
            }
            cov.calls_inferred += 1;
            return;
        }
    }
    cov.calls_unresolved += 1;
}

/// Resolve a type-qualified `Type::method()` call (`receiver_is_path`, qualifier ≠
/// `Self`): take the qualifier's last `::` segment as a type name and find methods
/// named `callee` on a type of that name.
///
/// - exactly **1** → an exact edge: Extracted `CONF_SAME_FILE` if the target is in
///   the SAME file as the call (the strongest static signal), else Inferred
///   `CONF_TYPE_QUALIFIED`.
/// - **>1** (several types share the plain name, each with the method) → Ambiguous
///   fan-out over just those — narrower and honest, never a confident pick.
/// - **0** → the qualifier shape decides, because a `Type::` qualifier whose type
///   lacks the method and a `mod::` module qualifier both arrive identically (a path
///   receiver with zero type-method matches), and confusing them is the
///   confident-wrong trap:
///   - **`type_name` IS a known type** (`is_known_type`) — the type exists but owns
///     no in-repo method by that name (a trait method we cannot see, or a stale/typo
///     call). The qualifier named *that* type and no other, so we must NOT confidently
///     bind to a same-named method on a different type. Stay honest: fan out
///     `methods_named` at Ambiguous `CONF_AMBIGUOUS`, or no edge if none — never a
///     `callables_named`-unique Inferred pick. (Matches develop's pre-slice-23
///     behavior for `Foo::bar()`.)
///   - **`type_name` is NOT a known type** — a module path `mod::func()` (or external).
///     Fall back to the repo-wide name rule (`callables_named`): unique → Inferred,
///     several → Ambiguous, zero → no edge. This is what resolves a genuine scoped
///     free-fn call.
///
/// Tallies into `cov` by the band actually emitted.
fn resolve_type_qualified(
    g: &mut Graph,
    repo_name: &str,
    path: &str,
    file: &AnalyzedFile,
    call: &CallRef,
    index: &CallIndex,
    cov: &mut RustLinkCoverage,
) {
    let caller_uid = uid_enclosing(repo_name, path, &call.enclosing_fqn);
    let receiver = call.receiver.as_deref().unwrap_or_default();
    let type_name = last_segment(receiver);
    let on_type = dedup(index.methods_on_type_named(type_name, &call.callee_name));
    match on_type.len() {
        // No method on a type of that name. The qualifier shape decides what is
        // honest here — a TYPE qualifier whose type lacks the method must NOT be
        // confidently bound to an unrelated free fn/method.
        0 if index.is_known_type(type_name) => {
            // The named type EXISTS but owns no in-repo method by that name — a trait
            // method we cannot see, or a stale/typo call. The `Type::` qualifier never
            // named any other type, so a confident bind to a same-named method on a
            // DIFFERENT type would be confident-WRONG. Stay honest: fan out same-named
            // methods at the AMBIGUOUS band (< 0.40), or — with no same-named method
            // anywhere — emit no edge. Never `callables_named`-unique → Inferred here.
            let methods = dedup(index.methods_named(&call.callee_name));
            match methods.len() {
                0 => cov.calls_unresolved += 1,
                _ => {
                    for target in &methods {
                        add_call_edge(
                            g,
                            &caller_uid,
                            target,
                            Provenance::Ambiguous,
                            CONF_AMBIGUOUS,
                        );
                    }
                    cov.calls_ambiguous += 1;
                }
            }
        }
        // `type_name` names NO known type → it is a MODULE path (`mod::func()`) or an
        // external path. Fall back to the repo-wide name rule, which resolves a
        // genuine scoped free-fn call: unique → Inferred, several → Ambiguous, zero →
        // no edge. (This is the legit fallback the type-qualified slice added.)
        0 => {
            let repo_wide = dedup(index.callables_named(&call.callee_name));
            match repo_wide.len() {
                0 => cov.calls_unresolved += 1,
                1 => {
                    add_call_edge(
                        g,
                        &caller_uid,
                        &repo_wide[0],
                        Provenance::Inferred,
                        CONF_CROSS_MODULE_UNIQUE,
                    );
                    cov.calls_inferred += 1;
                }
                _ => {
                    for target in &repo_wide {
                        add_call_edge(
                            g,
                            &caller_uid,
                            target,
                            Provenance::Ambiguous,
                            CONF_AMBIGUOUS,
                        );
                    }
                    cov.calls_ambiguous += 1;
                }
            }
        }
        // Exactly one type+method match → an exact edge. Same-file targets earn the
        // stronger Extracted band; cross-file ones the Inferred type-qualified band.
        1 => {
            let same_file = file
                .symbols
                .iter()
                .filter(|s| s.kind == NodeKind::Method && s.name == call.callee_name)
                .filter(|s| {
                    s.container_fqn
                        .as_deref()
                        .map(last_segment)
                        .is_some_and(|t| t == type_name)
                })
                .any(|s| uid_symbol(repo_name, path, &s.fqn) == on_type[0]);
            if same_file {
                add_call_edge(
                    g,
                    &caller_uid,
                    &on_type[0],
                    Provenance::Extracted,
                    CONF_SAME_FILE,
                );
                cov.calls_same_file += 1;
            } else {
                add_call_edge(
                    g,
                    &caller_uid,
                    &on_type[0],
                    Provenance::Inferred,
                    CONF_TYPE_QUALIFIED,
                );
                cov.calls_inferred += 1;
            }
        }
        // Several types share the plain name, each with the method → Ambiguous over
        // just those (still narrower than the whole repo-wide method set).
        _ => {
            for target in &on_type {
                add_call_edge(
                    g,
                    &caller_uid,
                    target,
                    Provenance::Ambiguous,
                    CONF_AMBIGUOUS,
                );
            }
            cov.calls_ambiguous += 1;
        }
    }
}

/// Drop duplicate UIDs preserving first-seen order (same-name items can yield
/// repeated target UIDs).
fn dedup(uids: Vec<Uid>) -> Vec<Uid> {
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::with_capacity(uids.len());
    for u in uids {
        if seen.insert(u.as_str().to_string()) {
            out.push(u);
        }
    }
    out
}

/// Add a `CALLS` edge unless it is a self-edge (a recursive call to the caller's
/// own node adds no impact information).
fn add_call_edge(g: &mut Graph, src: &Uid, dst: &Uid, prov: Provenance, conf: f32) {
    if src == dst {
        return;
    }
    g.add_edge(Edge {
        src: src.clone(),
        dst: dst.clone(),
        kind: EdgeKind::Calls,
        provenance: prov,
        confidence: Confidence::new(conf),
    });
}

/// Derive the enclosing type fqn from an enclosing method fqn:
/// `"m::C::f"` → `Some("m::C")`, `"C::f"` → `Some("C")`, `"f"` → `None`.
/// The inverse of `fqn_of` for a member: strip the trailing `::member`.
fn enclosing_type(enclosing_fqn: &str) -> Option<&str> {
    enclosing_fqn.rfind("::").map(|idx| &enclosing_fqn[..idx])
}

// ── Repo-wide call index ─────────────────────────────────────────────────────

/// Lookup tables for call resolution, built once per [`assemble_rust`].
struct CallIndex {
    /// symbol name → uids of all **Function or Method** symbols repo-wide with that
    /// name (the bare-name cross-module rule's candidates).
    callables_by_name: BTreeMap<String, Vec<Uid>>,
    /// symbol name → uids of all **Method** symbols repo-wide (the unknown-receiver
    /// rule's candidates).
    methods_by_name: BTreeMap<String, Vec<Uid>>,
    /// `(type-name, method-name)` → uids of all **Method** symbols repo-wide whose
    /// owning type's *last path segment* is `type-name` and which are named
    /// `method-name` (the type-qualified `Type::method()` rule's candidates). Keyed
    /// on the type's last segment because a call qualifier is matched by its last
    /// `::` segment (`a::b::Config::new()` and `Config::new()` both name type
    /// `Config`); the value is every method of any type with that plain name, so a
    /// genuine cross-module name collision (two `Config`s) yields several and
    /// degrades honestly to Ambiguous.
    methods_by_type_and_name: BTreeMap<(String, String), Vec<Uid>>,
    /// The *last `::` segment* of every type-like symbol repo-wide — `Class`
    /// (struct/enum/union) and `Interface` (trait). Lets the type-qualified rule tell
    /// a **type qualifier** (`Foo::bar()`, where `Foo` names a known type) apart from
    /// a **module qualifier** (`mod::func()`, where `mod` names no type). Both arrive
    /// as `receiver_is_path == true` with zero type-method matches, so without this
    /// set the free-fn `callables_named` fallback would wrongly fire for a
    /// `Foo::bar()` whose type lacks the method — confidently binding to an unrelated
    /// `bar` the qualifier never named. Keyed on the last segment for the same reason
    /// as `methods_by_type_and_name`: a call qualifier is matched by its last `::`
    /// segment.
    type_names: std::collections::BTreeSet<String>,
}

impl CallIndex {
    fn build(repo_name: &str, analyzed: &BTreeMap<String, AnalyzedFile>) -> CallIndex {
        let mut callables_by_name: BTreeMap<String, Vec<Uid>> = BTreeMap::new();
        let mut methods_by_name: BTreeMap<String, Vec<Uid>> = BTreeMap::new();
        let mut methods_by_type_and_name: BTreeMap<(String, String), Vec<Uid>> = BTreeMap::new();
        let mut type_names: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for (path, file) in analyzed {
            for sym in &file.symbols {
                let uid = uid_symbol(repo_name, path, &sym.fqn);
                match sym.kind {
                    // A type-like symbol: struct/enum/union extract as `Class`, a
                    // trait as `Interface` (see `analyze.rs`). Record its plain name
                    // (last `::` segment) so the type-qualified rule can recognize a
                    // `Foo::bar()` qualifier that names a known type but whose type
                    // lacks the method — and stay honest instead of falling back to an
                    // unrelated free fn.
                    NodeKind::Class | NodeKind::Interface => {
                        let name = last_segment(&sym.name);
                        if !name.is_empty() {
                            type_names.insert(name.to_string());
                        }
                    }
                    NodeKind::Function | NodeKind::Method => {
                        callables_by_name
                            .entry(sym.name.clone())
                            .or_default()
                            .push(uid.clone());
                        if sym.kind == NodeKind::Method {
                            methods_by_name
                                .entry(sym.name.clone())
                                .or_default()
                                .push(uid.clone());
                            // Index the method under its owning type's last segment so
                            // a `Type::method()` qualifier resolves to it. A method
                            // whose container fqn we cannot read (None) contributes no
                            // type-qualified entry — surfaced by absence, never faked.
                            if let Some(type_name) = sym
                                .container_fqn
                                .as_deref()
                                .map(last_segment)
                                .filter(|t| !t.is_empty())
                            {
                                methods_by_type_and_name
                                    .entry((type_name.to_string(), sym.name.clone()))
                                    .or_default()
                                    .push(uid);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        CallIndex {
            callables_by_name,
            methods_by_name,
            methods_by_type_and_name,
            type_names,
        }
    }

    /// All Function/Method symbols repo-wide named `name`.
    fn callables_named(&self, name: &str) -> Vec<Uid> {
        self.callables_by_name
            .get(name)
            .cloned()
            .unwrap_or_default()
    }

    /// All **Method** symbols repo-wide named `name` (the unknown-receiver rule).
    fn methods_named(&self, name: &str) -> Vec<Uid> {
        self.methods_by_name.get(name).cloned().unwrap_or_default()
    }

    /// All **Method** symbols repo-wide named `method` whose owning type's last path
    /// segment is `type_name` (the type-qualified `Type::method()` rule). Empty when
    /// no type of that name has such a method — the caller then branches on whether
    /// `type_name` is a known type (see [`CallIndex::is_known_type`]).
    fn methods_on_type_named(&self, type_name: &str, method: &str) -> Vec<Uid> {
        self.methods_by_type_and_name
            .get(&(type_name.to_string(), method.to_string()))
            .cloned()
            .unwrap_or_default()
    }

    /// Whether `name` (a call qualifier's last `::` segment) names a type-like symbol
    /// (`Class`/`Interface`) repo-wide. Used by the type-qualified rule to tell a
    /// **type qualifier** whose type lacks the method (stay honest — never a confident
    /// pick of an unrelated method) apart from a **module qualifier** `mod::func()`
    /// (the genuine free-fn fallback).
    fn is_known_type(&self, name: &str) -> bool {
        self.type_names.contains(name)
    }
}

/// The last `::`-separated segment of a path (`a::b::C` → `C`, `C` → `C`). Mirrors
/// the analyzer's `last_segment`; the type qualifier of a call (`a::b::Type`) and a
/// method's owning-type fqn (`mod::Type`) are both matched by their last segment.
fn last_segment(path: &str) -> &str {
    match path.rfind("::") {
        Some(idx) => &path[idx + 2..],
        None => path,
    }
}

/// Maps a module-path string to the Module UIDs whose items live in it. Used only
/// for the best-effort `use → IMPORTS` edge; never for call binding.
struct ModulePathIndex {
    modules_by_path: BTreeMap<String, std::collections::BTreeSet<Uid>>,
}

impl ModulePathIndex {
    fn build(repo_name: &str, analyzed: &BTreeMap<String, AnalyzedFile>) -> ModulePathIndex {
        let mut modules_by_path: BTreeMap<String, std::collections::BTreeSet<Uid>> =
            BTreeMap::new();
        for (path, file) in analyzed {
            let module_uid = uid_module(repo_name, path);
            for sym in &file.symbols {
                // An item's module path is its fqn minus the item's own name (and any
                // enclosing type). We index the module-path prefix of every top-level
                // item so a `use that_module::Item` can resolve to this file.
                if let Some(prefix) = sym.fqn.rfind("::").map(|i| &sym.fqn[..i]) {
                    modules_by_path
                        .entry(prefix.to_string())
                        .or_default()
                        .insert(module_uid.clone());
                }
            }
        }
        ModulePathIndex { modules_by_path }
    }

    /// Module UIDs a `use` of `specifier` could refer to. A `use a::b::C` names a
    /// symbol `C` inside module `a::b`, so the resolvable module path is the
    /// specifier itself (`a::b::C` is a module re-export) OR its parent (`a::b`).
    /// A leading `crate::`/`self::` root is stripped first (the module paths this
    /// index keys on are crate-root-relative but carry no `crate` segment), so the
    /// dominant `use crate::module::Item` style resolves; `super::` is NOT stripped
    /// (it is parent-relative — resolving it needs the file tree, deferred). An
    /// external-crate path matches no module and yields nothing — never an invented
    /// link.
    fn modules_for_use(&self, specifier: &str) -> Vec<Uid> {
        let stripped = specifier
            .strip_prefix("crate::")
            .or_else(|| specifier.strip_prefix("self::"))
            .unwrap_or(specifier);
        let mut out: std::collections::BTreeSet<Uid> = std::collections::BTreeSet::new();
        if let Some(s) = self.modules_by_path.get(stripped) {
            out.extend(s.iter().cloned());
        }
        if let Some(parent) = stripped.rfind("::").map(|i| &stripped[..i]) {
            if let Some(s) = self.modules_by_path.get(parent) {
                out.extend(s.iter().cloned());
            }
        }
        out.into_iter().collect()
    }
}

// ── Uid helpers (rust-tagged) ────────────────────────────────────────────────

fn uid_repo(repo_name: &str) -> Uid {
    Uid::new(LANG, repo_name, "", repo_name, "")
}

fn uid_module(repo_name: &str, path: &str) -> Uid {
    Uid::new(LANG, repo_name, path, "<module>", "")
}

fn uid_symbol(repo_name: &str, path: &str, fqn: &str) -> Uid {
    Uid::new(LANG, repo_name, path, fqn, "")
}

/// The node a site enclosed by `enclosing_fqn` in `path` is attributed to: the
/// enclosing fn/method symbol, or the file's Module node at top level.
fn uid_enclosing(repo_name: &str, path: &str, enclosing_fqn: &str) -> Uid {
    if enclosing_fqn.is_empty() {
        uid_module(repo_name, path)
    } else {
        uid_symbol(repo_name, path, enclosing_fqn)
    }
}

// ── Node / edge helpers ──────────────────────────────────────────────────────

fn extracted_node(uid: Uid, kind: NodeKind, name: &str, fqn: &str, path: &str, span: Span) -> Node {
    Node {
        uid,
        kind,
        name: name.to_string(),
        fqn: fqn.to_string(),
        path: path.to_string(),
        span,
        provenance: Provenance::Extracted,
        confidence: Confidence::new(1.0),
    }
}

/// Add the reciprocal MEMBER_OF / DEFINES pair (both Extracted 1.0).
fn add_structural_pair(g: &mut Graph, member: &Uid, container: &Uid) {
    g.add_edge(extracted_edge(member, container, EdgeKind::MemberOf));
    g.add_edge(extracted_edge(container, member, EdgeKind::Defines));
}

fn extracted_edge(src: &Uid, dst: &Uid, kind: EdgeKind) -> Edge {
    Edge {
        src: src.clone(),
        dst: dst.clone(),
        kind,
        provenance: Provenance::Extracted,
        confidence: Confidence::new(1.0),
    }
}

fn import_edge(src: &Uid, dst: &Uid) -> Edge {
    extracted_edge(src, dst, EdgeKind::Imports)
}

// ── Path helpers ─────────────────────────────────────────────────────────────

/// The base file name (text after the last `/`), or the whole path if none.
fn base_name(path: &str) -> &str {
    match path.rfind('/') {
        Some(idx) => &path[idx + 1..],
        None => path,
    }
}
