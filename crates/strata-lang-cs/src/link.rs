//! The pure, deterministic C# graph linker (band-disciplined).
//!
//! [`assemble_csharp`] turns an already-analyzed C# file set (a `BTreeMap` of
//! `path → AnalyzedFile`) into the C# code-plane graph and adds it to a [`Graph`].
//! It performs **no IO** and iterates in sorted order, so the same input always
//! yields the same graph. C# links *within its own resolution world* — calls bind
//! to same-file defs, own-type `this.` methods, or unique repo-wide names — never
//! across into the TS/Python planes this slice.
//!
//! **Confidence discipline (design §4.1) is LAW.** Every heuristic confidence is a
//! doc-commented constant capped to its provenance band; a heuristic edge can
//! never reach EXTRACTED-1.0 or a RESOLVED tier, and an ambiguous one is always
//! strictly below 0.40. Reflection and dynamic dispatch — `GetMethod`/`Invoke`,
//! `dynamic` receivers, delegate indirection — are **never guessed**: the extractor
//! records `t.GetMethod("Run")` as a member call to `GetMethod` (the `"Run"` string
//! is an argument, not a callee), and an unresolved receiver fans out only at the
//! AMBIGUOUS band, or produces no edge at all.
//!
//! **Why no `import-matched` rule (unlike Python).** A C# `using` imports a
//! *namespace*, not a specific symbol — `using System.Text;` does not name
//! `StringBuilder`. So there is no honest per-name import binding to seed a call
//! from; a cross-file bare call resolves through the **unique repo-wide name** rule
//! instead (still Inferred, still capped). A `using X = Y` alias binds a single
//! name and is recorded, but resolving an alias to its (often external) target
//! needs the type system — deferred to Roslyn (A3), surfaced as a miss, never
//! invented.

use std::collections::BTreeMap;

use strata_core::{
    AnalyzedFile, CallRef, Confidence, Edge, EdgeKind, Graph, Node, NodeKind, Provenance, Span, Uid,
};

/// The language/plane tag for C# code-plane UIDs (distinct from TS `"ts"` and
/// Python `"py"`), so a mixed-language repo never collides UIDs across planes.
const LANG: &str = "cs";

// ── Calibrated heuristic confidences (design §4.1) ───────────────────────────
//
// Stored confidence = min(measured precision, provenance-band ceiling): EXTRACTED
// band 0.95–1.0; INFERRED band 0.40–0.80; AMBIGUOUS band < 0.40. A heuristic edge
// must NEVER reach or exceed a RESOLVED (0.97) or EXTRACTED-1.0 confidence — "an
// inference can never masquerade as a fact." These are a *starting calibration*
// over the committed crate fixtures (corpus stated in
// `docs/accuracy/cs-extraction.md`); the band guardrail
// (`tests/linking.rs::csharp_edges_satisfy_band_invariant_non_vacuously` and the
// indexer's `tests/confidence_bands.rs`) pins them in band.

/// A bare call `M()` whose target is a method/function **defined in the same
/// file**. C# name resolution inside one type is *not* as deterministic as
/// Python's module binding (it depends on overload resolution and inherited
/// members), but a same-file simple-name call that matches a same-file definition
/// is the strongest static signal we have without a compiler — graded at the
/// EXTRACTED band **floor**. `0.95`, NOT `1.0`: never outranks a RESOLVED (0.97)
/// Roslyn fact (Track A3), and overload-set collapse means we may point at a
/// name-collapsed node rather than the exact overload. Provenance: Extracted.
const CONF_SAME_FILE: f32 = 0.95;

/// A `this.M()` call resolved to a method `M` on the **enclosing type**. A
/// confident heuristic guess (the receiver `this` names this type), but still a
/// guess — `M` may be `virtual`/overridden, or inherited from a base type we
/// cannot see — so it sits at the INFERRED ceiling, never a fact. Mirrors the
/// Python plane's `self.m()` rule and the TS plane's `this.method()` rule.
/// Provenance: Inferred.
const CONF_THIS_METHOD: f32 = 0.80;

/// A bare call `M()` with no same-file def, where exactly ONE method/function
/// named `M` exists **repo-wide**. A single confident candidate recovered by name
/// (a C# `using` imports a namespace, not a symbol, so there is no import binding
/// to lean on) — a single heuristic guess, capped at the INFERRED ceiling, never a
/// fact. Provenance: Inferred.
const CONF_CROSS_FILE_UNIQUE: f32 = 0.80;

/// An **ambiguous** call: a bare name or unknown-receiver method with SEVERAL
/// same-named candidates the heuristic cannot disambiguate. It fans out to all
/// candidates at the AMBIGUOUS band (strictly < 0.40) — the honest "could be any
/// of these" rather than a confident wrong pick. Reflection (`mi.Invoke()`) and
/// `dynamic` receivers land here at most, never higher. Provenance: Ambiguous.
const CONF_AMBIGUOUS: f32 = 0.35;

/// Per-repo C# link coverage, surfaced for tests and (later) `IndexStats`.
///
/// A "site" is one call site. The outcome buckets do **not** necessarily sum to
/// `calls_total`: a call the heuristic cannot point anywhere (an unknown bare
/// name, a `this.Ghost()` with no matching method, a reflection `Invoke`) counts
/// only toward `calls_total` — surfaced by absence, never invented.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CsLinkCoverage {
    /// Every call site considered.
    pub calls_total: usize,
    /// Same-file Extracted resolutions.
    pub calls_same_file: usize,
    /// Inferred resolutions (this-method or unique cross-file name).
    pub calls_inferred: usize,
    /// Ambiguous fan-outs (several same-named candidates / unknown receiver).
    pub calls_ambiguous: usize,
    /// Sites that produced no edge (no candidate anywhere — incl. reflection).
    pub calls_unresolved: usize,
}

/// Add the C# plane to `g` from an analyzed C# file set.
///
/// Idempotent by UID. Creates a `cs`-tagged `Repo` node, a `Module` per file,
/// `Class`/`Interface`/`Function`/`Method` symbol nodes, the structural
/// `MEMBER_OF`/`DEFINES` pairs, module-granular `IMPORTS` edges for repo-resolvable
/// usings, and the band-disciplined `CALLS` edges. The C# plane uses its own
/// `LANG` tag, so it never collides with the TS/Python planes in a mixed-language
/// repo (cross-language linking is out of scope this slice).
pub fn assemble_csharp(
    g: &mut Graph,
    repo_name: &str,
    analyzed: &BTreeMap<String, AnalyzedFile>,
) -> CsLinkCoverage {
    let mut cov = CsLinkCoverage::default();

    // ── Repo node (cs-tagged). ──
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
        // Structural edges after all symbol nodes in the file exist (a method may
        // attach to a type declared later in the file).
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

    // ── Phase 2: import edges. A `using` names a namespace, not a file, so it
    // resolves to a repo Module only when a file's *namespace* (encoded as the
    // dotted prefix of its symbols' fqns) matches the using's specifier. This is a
    // best-effort module-granular IMPORTS edge for visibility; it seeds NO call
    // binding (the unique-name rule does cross-file calls). A using that names an
    // external/namespace-only target adds no edge — never an invented link. ──
    let ns_index = NamespaceIndex::build(repo_name, analyzed);
    for (path, file) in analyzed {
        let module_uid = uid_module(repo_name, path);
        for import in &file.imports {
            for target_module in ns_index.modules_in_namespace(&import.specifier) {
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

/// Resolve one call site to its `CALLS` edge(s) and tally [`CsLinkCoverage`].
///
/// Resolution precedence (most-confident first):
/// 1. **Same-file def** — a Function/Method named `callee` in this file (bare call
///    only) → Extracted 0.95, single edge (per same-file def), done.
/// 2. **`this.M()`** — a method `M` on the enclosing type → Inferred 0.80.
/// 3. **Unique cross-file name** — exactly one repo-wide method/function named
///    `callee` (bare call) → Inferred 0.80.
/// 4. **Ambiguous** — several same-named candidates (bare or unknown receiver) →
///    Ambiguous 0.35, fan out to all.
///
/// Anything else (unknown bare name, `this.Ghost()` with no method, a reflection
/// `mi.Invoke()` / unknown receiver with zero candidates) produces **no edge** —
/// surfaced, never invented.
fn resolve_call(
    g: &mut Graph,
    repo_name: &str,
    path: &str,
    file: &AnalyzedFile,
    call: &CallRef,
    index: &CallIndex,
    cov: &mut CsLinkCoverage,
) {
    cov.calls_total += 1;
    let caller_uid = uid_enclosing(repo_name, path, &call.enclosing_fqn);

    match call.receiver.as_deref() {
        // ── Bare call `M()`. ──
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
            // Overloads collapse to one fqn, so `local` may contain duplicate UIDs;
            // dedup so we emit one edge per distinct target node.
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

            // 3/4. Cross-file repo-wide: unique → Inferred, several → Ambiguous.
            let repo_wide = dedup(index.callables_named(&call.callee_name));
            match repo_wide.len() {
                0 => cov.calls_unresolved += 1, // unknown name — no edge, surfaced.
                1 => {
                    add_call_edge(
                        g,
                        &caller_uid,
                        &repo_wide[0],
                        Provenance::Inferred,
                        CONF_CROSS_FILE_UNIQUE,
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
        // ── `this.M()` — resolve to a method on the enclosing type. ──
        Some("this") => {
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
                            CONF_THIS_METHOD,
                        );
                    }
                    cov.calls_inferred += 1;
                    return;
                }
            }
            // No matching own-type method (a base-type method we cannot see, or
            // `this` outside a type) → no edge, surfaced.
            cov.calls_unresolved += 1;
        }
        // ── `other.M()` — unknown receiver type: fan out to same-named methods. ──
        // This is also where reflection lands: `mi.Invoke(...)` is `Invoke` with
        // receiver `mi`; with no `Invoke` method defined in the repo it resolves to
        // nothing (unresolved), and even if one existed it would be at most an
        // Ambiguous guess — never a confident reflective dispatch.
        Some(_) => {
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

/// Drop duplicate UIDs preserving first-seen order (overload collapse can yield
/// repeated target UIDs from same-fqn symbols).
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
/// `"App.C.M"` → `Some("App.C")`, `"C.M"` → `Some("C")`, `"f"` → `None`.
/// The inverse of `fqn_of` for a member: strip the trailing `.member`.
fn enclosing_type(enclosing_fqn: &str) -> Option<&str> {
    enclosing_fqn.rfind('.').map(|idx| &enclosing_fqn[..idx])
}

// ── Repo-wide call index ─────────────────────────────────────────────────────

/// Lookup tables for call resolution, built once per [`assemble_csharp`].
struct CallIndex {
    /// symbol name → uids of all **Function or Method** symbols repo-wide with that
    /// name (the bare-name cross-file rule's candidates).
    callables_by_name: BTreeMap<String, Vec<Uid>>,
    /// symbol name → uids of all **Method** symbols repo-wide (the unknown-receiver
    /// rule's candidates).
    methods_by_name: BTreeMap<String, Vec<Uid>>,
}

impl CallIndex {
    fn build(repo_name: &str, analyzed: &BTreeMap<String, AnalyzedFile>) -> CallIndex {
        let mut callables_by_name: BTreeMap<String, Vec<Uid>> = BTreeMap::new();
        let mut methods_by_name: BTreeMap<String, Vec<Uid>> = BTreeMap::new();
        for (path, file) in analyzed {
            for sym in &file.symbols {
                let uid = uid_symbol(repo_name, path, &sym.fqn);
                match sym.kind {
                    NodeKind::Function | NodeKind::Method => {
                        callables_by_name
                            .entry(sym.name.clone())
                            .or_default()
                            .push(uid.clone());
                        if sym.kind == NodeKind::Method {
                            methods_by_name
                                .entry(sym.name.clone())
                                .or_default()
                                .push(uid);
                        }
                    }
                    _ => {}
                }
            }
        }
        CallIndex {
            callables_by_name,
            methods_by_name,
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
}

/// Maps a namespace string to the Module UIDs whose types live in it. Used only
/// for the best-effort `using → IMPORTS` edge; never for call binding.
struct NamespaceIndex {
    modules_by_ns: BTreeMap<String, std::collections::BTreeSet<Uid>>,
}

impl NamespaceIndex {
    fn build(repo_name: &str, analyzed: &BTreeMap<String, AnalyzedFile>) -> NamespaceIndex {
        let mut modules_by_ns: BTreeMap<String, std::collections::BTreeSet<Uid>> = BTreeMap::new();
        for (path, file) in analyzed {
            let module_uid = uid_module(repo_name, path);
            for sym in &file.symbols {
                // A top-level type's namespace is its fqn minus the type name. Only
                // types (Class/Interface) define a namespace surface for usings.
                if matches!(sym.kind, NodeKind::Class | NodeKind::Interface) {
                    if let Some(ns) = sym.fqn.rfind('.').map(|i| &sym.fqn[..i]) {
                        modules_by_ns
                            .entry(ns.to_string())
                            .or_default()
                            .insert(module_uid.clone());
                    }
                }
            }
        }
        NamespaceIndex { modules_by_ns }
    }

    /// Module UIDs whose types are declared in exactly `namespace`.
    fn modules_in_namespace(&self, namespace: &str) -> Vec<Uid> {
        self.modules_by_ns
            .get(namespace)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default()
    }
}

// ── Uid helpers (cs-tagged) ──────────────────────────────────────────────────

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
/// enclosing method/function symbol, or the file's Module node at top level.
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
