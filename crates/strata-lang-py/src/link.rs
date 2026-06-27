//! The pure, deterministic Python graph linker (band-disciplined).
//!
//! [`assemble_python`] turns an already-analyzed Python file set (a `BTreeMap`
//! of `path → AnalyzedFile`) into the Python code-plane graph and adds it to a
//! [`Graph`]. It performs **no IO** and iterates in sorted order, so the same
//! input always yields the same graph. Python links *within its own resolution
//! world* — relative/absolute imports resolve to repo module paths, calls bind
//! to same-module defs, own-class `self.` methods, import-matched targets, or
//! unique repo-wide names — never across into the TS plane this slice.
//!
//! **Confidence discipline (design §4.1) is LAW.** Every heuristic confidence is
//! a doc-commented constant capped to its provenance band; a heuristic edge can
//! never reach EXTRACTED-1.0 or a RESOLVED tier, and an ambiguous one is always
//! strictly below 0.40. Dynamic dispatch — `getattr`, star imports,
//! monkey-patching — is **never guessed**: the extractor drops a `getattr(...)()`
//! callee, a star import binds no name, and an unresolved receiver fans out only
//! at the AMBIGUOUS band.

use std::collections::{BTreeMap, BTreeSet};

use strata_core::{
    AnalyzedFile, CallRef, Confidence, Edge, EdgeKind, Graph, ImportRef, Node, NodeKind,
    Provenance, Span, Uid,
};

/// The language/plane tag for Python code-plane UIDs (distinct from TS `"ts"`).
const LANG: &str = "py";

// ── Calibrated heuristic confidences (design §4.1) ───────────────────────────
//
// Stored confidence = min(measured precision, provenance-band ceiling): EXTRACTED
// band 0.95–1.0; INFERRED band 0.40–0.80; AMBIGUOUS band < 0.40. A heuristic edge
// must NEVER reach or exceed a RESOLVED (0.97) or EXTRACTED-1.0 confidence — "an
// inference can never masquerade as a fact." These are a *starting calibration*
// over the committed crate fixtures (corpus stated in
// `docs/accuracy/py-extraction.md`); the band guardrail
// (`tests/linking.rs::python_edges_satisfy_band_invariant_non_vacuously` and the
// indexer's `tests/confidence_bands.rs`) pins them in band.

/// A bare call `f()` whose target is a function/method **defined in the same
/// file**. Python name binding inside one module is deterministic — the local
/// `def` is the symbol the name resolves to — so this is the strongest static
/// signal we have: a same-module fact graded at the EXTRACTED band **floor**.
/// `0.95`, NOT `1.0`: even a same-file binding must never outrank a RESOLVED
/// (0.97) compiler-grade fact, and a re-binding (`f = other` after the def) is a
/// theoretical miss we do not chase. Provenance: Extracted.
const CONF_SAME_MODULE: f32 = 0.95;

/// A `self.m()` call resolved to a method `m` on the **enclosing class**. A
/// confident heuristic guess (the receiver `self` names this class), but still a
/// guess — `m` could be overridden, or come from a base class we cannot see — so
/// it sits at the INFERRED ceiling, never a fact. Provenance: Inferred.
const CONF_SELF_METHOD: f32 = 0.80;

/// A bare call `f()` where `f` is **import-matched**: a `from <mod> import f`
/// (or `import <mod>` alias) binds `f` to a module we resolved to a repo path,
/// and that module defines `f`. A strong cross-module signal, but the binding is
/// recovered heuristically (no type system), so INFERRED ceiling. Provenance:
/// Inferred.
const CONF_IMPORT_MATCHED: f32 = 0.80;

/// A bare call `f()` with no local def and no import binding, where exactly ONE
/// function named `f` exists **repo-wide**. A single confident candidate — but a
/// name match without an import is weaker than an import-matched one, so it also
/// caps at the INFERRED ceiling (a single guess, never a fact). Provenance:
/// Inferred.
const CONF_BARE_UNIQUE: f32 = 0.80;

/// An **ambiguous** call: a bare name or unknown-receiver method with SEVERAL
/// same-named candidates the heuristic cannot disambiguate. It fans out to all
/// candidates at the AMBIGUOUS band (strictly < 0.40) — the honest "could be any
/// of these" rather than a confident wrong pick. Provenance: Ambiguous.
const CONF_AMBIGUOUS: f32 = 0.35;

/// Per-repo Python link coverage, surfaced for tests and (later) `IndexStats`.
///
/// A "site" is one call site. The outcome buckets do **not** necessarily sum to
/// `calls_total`: a call the heuristic cannot point anywhere (an unknown bare
/// name, a `self.ghost()` with no matching method) counts only toward
/// `calls_total` — surfaced by absence, never invented.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PyLinkCoverage {
    /// Every call site considered.
    pub calls_total: usize,
    /// Same-module Extracted resolutions.
    pub calls_same_module: usize,
    /// Inferred resolutions (self-method, import-matched, or unique bare name).
    pub calls_inferred: usize,
    /// Ambiguous fan-outs (several same-named candidates).
    pub calls_ambiguous: usize,
    /// Sites that produced no edge (no candidate anywhere).
    pub calls_unresolved: usize,
}

/// Add the Python plane to `g` from an analyzed Python file set.
///
/// Idempotent by UID. Creates a `py`-tagged `Repo` node, a `Module` per file,
/// `Class`/`Function`/`Method` symbol nodes, the structural `MEMBER_OF`/`DEFINES`
/// pairs, module-granular `IMPORTS` edges, and the band-disciplined `CALLS`
/// edges. The Python plane uses its own `LANG` tag, so it never collides with the
/// TS plane in a mixed-language repo (cross-language linking is out of scope this
/// slice).
pub fn assemble_python(
    g: &mut Graph,
    repo_name: &str,
    analyzed: &BTreeMap<String, AnalyzedFile>,
) -> PyLinkCoverage {
    let mut cov = PyLinkCoverage::default();

    // The module keyset is the existence oracle for import resolution (we never
    // resolve to a module the file set does not contain).
    let module_paths: BTreeSet<String> = analyzed.keys().cloned().collect();

    // ── Repo node (py-tagged). ──
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
        // attach to a class defined later in the file).
        for sym in &file.symbols {
            let sym_uid = uid_symbol(repo_name, path, &sym.fqn);
            let container_uid = match &sym.container_fqn {
                Some(class_fqn) => {
                    let class_uid = uid_symbol(repo_name, path, class_fqn);
                    if g.get_node(&class_uid).is_some() {
                        class_uid
                    } else {
                        module_uid.clone()
                    }
                }
                None => module_uid.clone(),
            };
            add_structural_pair(g, &sym_uid, &container_uid);
        }
    }

    // ── Phase 2: import edges + per-file imported-name → resolved-module map. ──
    let mut imported_targets: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    for (path, file) in analyzed {
        let module_uid = uid_module(repo_name, path);
        let mut per_file: BTreeMap<String, String> = BTreeMap::new();
        for import in &file.imports {
            resolve_import(
                g,
                repo_name,
                path,
                &module_uid,
                import,
                &module_paths,
                &mut per_file,
            );
        }
        imported_targets.insert(path.clone(), per_file);
    }

    // ── Index for call resolution. ──
    let index = CallIndex::build(repo_name, analyzed);

    // ── Phase 3: call edges (band-disciplined). ──
    for (path, file) in analyzed {
        let empty = BTreeMap::new();
        let per_file_imports = imported_targets.get(path).unwrap_or(&empty);
        for call in &file.calls {
            resolve_call(
                g,
                repo_name,
                path,
                file,
                call,
                per_file_imports,
                &index,
                &mut cov,
            );
        }
    }

    cov
}

/// Resolve one import to its module-granular `IMPORTS` edge and update
/// `per_file` (the imported-name → resolved-module-path map call resolution
/// consumes). Only a relative/absolute import that resolves to a **repo module
/// path** seeds a binding; an external package (no matching module file) seeds
/// none — never an invented cross-package link.
///
/// A **star import** (`from a import *`) has empty `imported_names` (the
/// extractor binds nothing), so it still adds the module `IMPORTS` edge but seeds
/// no name binding — a star import never lets a call be "import-matched".
#[allow(clippy::too_many_arguments)]
fn resolve_import(
    g: &mut Graph,
    repo_name: &str,
    importer_path: &str,
    module_uid: &Uid,
    import: &ImportRef,
    module_paths: &BTreeSet<String>,
    per_file: &mut BTreeMap<String, String>,
) {
    let Some(target) = resolve_module(&import.specifier, importer_path, module_paths) else {
        // External package or unresolved relative path — no edge, no binding.
        return;
    };
    let target_uid = uid_module(repo_name, &target);
    g.add_edge(import_edge(module_uid, &target_uid));
    for name in &import.imported_names {
        per_file.insert(name.clone(), target.clone());
    }
}

/// Resolve a Python import specifier to a repo module path, or `None` (external /
/// unresolved). Mirrors the resolution world Python uses, bounded to what we can
/// know statically from the file set:
///
/// - **Relative** (`.mod`, `..pkg.sub`): count the leading dots — one dot = the
///   importer's package directory, each extra dot climbs one parent — then append
///   the dotted remainder as a path. Probe `<path>.py` and `<path>/__init__.py`.
/// - **Absolute** (`pkg.sub`, `helpers`): treat the dotted path as repo-relative
///   and probe `<path>.py` / `<path>/__init__.py`. A miss ⇒ external package
///   (`None`) — we never invent a link to a module the repo does not contain.
fn resolve_module(
    specifier: &str,
    importer_path: &str,
    module_paths: &BTreeSet<String>,
) -> Option<String> {
    if specifier.starts_with('.') {
        // Leading-dot run = relative level. `.` = current package (1 dot).
        let dots = specifier.chars().take_while(|&c| c == '.').count();
        let rest = &specifier[dots..]; // dotted remainder after the dots
        let importer_dir = parent_dir(importer_path);

        // One dot stays in the importer's package dir; each extra dot climbs one.
        let mut base = importer_dir;
        for _ in 0..dots.saturating_sub(1) {
            base = parent_dir(&base);
        }
        let sub = rest.replace('.', "/");
        let joined = join(&base, &sub);
        probe_module(&joined, module_paths)
    } else {
        // Absolute dotted path, probed repo-relative (bounded: repo modules only).
        let path = specifier.replace('.', "/");
        probe_module(&path, module_paths)
    }
}

/// Probe a `/`-normalized module base for a real file in the keyset: `<base>.py`
/// then `<base>/__init__.py`. A bare base (empty, e.g. `from . import x`) probes
/// `__init__.py` in the package dir. Returns the matching repo path or `None`.
fn probe_module(base: &str, module_paths: &BTreeSet<String>) -> Option<String> {
    let candidates = if base.is_empty() {
        vec!["__init__.py".to_string()]
    } else {
        vec![format!("{base}.py"), format!("{base}/__init__.py")]
    };
    candidates.into_iter().find(|c| module_paths.contains(c))
}

/// Resolve one call site to its `CALLS` edge(s) and tally [`PyLinkCoverage`].
///
/// Resolution precedence (most-confident first):
/// 1. **Same-module def** — a Function/Method named `callee` in this file
///    (bare call only) → Extracted 0.95, single edge, done.
/// 2. **`self.m()`** — a method `m` on the enclosing class → Inferred 0.80.
/// 3. **Import-matched** — `callee` bound by an import to a resolved module that
///    defines it → Inferred 0.80.
/// 4. **Unique bare name** — exactly one repo-wide function named `callee` →
///    Inferred 0.80.
/// 5. **Ambiguous** — several same-named candidates (bare or unknown receiver) →
///    Ambiguous 0.35, fan out to all.
///
/// Anything else (unknown bare name, `self.ghost()` with no method, a dynamic
/// receiver with zero candidates) produces **no edge** — surfaced, never invented.
#[allow(clippy::too_many_arguments)]
fn resolve_call(
    g: &mut Graph,
    repo_name: &str,
    path: &str,
    file: &AnalyzedFile,
    call: &CallRef,
    per_file_imports: &BTreeMap<String, String>,
    index: &CallIndex,
    cov: &mut PyLinkCoverage,
) {
    cov.calls_total += 1;
    let caller_uid = uid_enclosing(repo_name, path, &call.enclosing_fqn);

    match call.receiver.as_deref() {
        // ── Bare call `f()`. ──
        None => {
            // 1. Same-module def (Extracted) — the deterministic local binding.
            let local: Vec<Uid> = file
                .symbols
                .iter()
                .filter(|s| {
                    matches!(s.kind, NodeKind::Function | NodeKind::Method)
                        && s.name == call.callee_name
                })
                .map(|s| uid_symbol(repo_name, path, &s.fqn))
                .collect();
            if !local.is_empty() {
                // A same-file name resolves to its local def(s). Almost always
                // exactly one; if a file shadows a name (def + nested def of the
                // same name) we still grade same-module — they are all local
                // facts. Self-edges are skipped (a recursive call adds no impact).
                for target in &local {
                    add_call_edge(
                        g,
                        &caller_uid,
                        target,
                        Provenance::Extracted,
                        CONF_SAME_MODULE,
                    );
                }
                cov.calls_same_module += 1;
                return;
            }

            // 3. Import-matched (Inferred): the name is bound to a resolved module.
            if let Some(target_module) = per_file_imports.get(&call.callee_name) {
                let targets = index.symbols_named_in_module(target_module, &call.callee_name);
                if !targets.is_empty() {
                    for target in &targets {
                        add_call_edge(
                            g,
                            &caller_uid,
                            target,
                            Provenance::Inferred,
                            CONF_IMPORT_MATCHED,
                        );
                    }
                    cov.calls_inferred += 1;
                    return;
                }
            }

            // 4/5. Bare name repo-wide: unique → Inferred, several → Ambiguous.
            let repo_wide = index.functions_named(&call.callee_name);
            match repo_wide.len() {
                0 => cov.calls_unresolved += 1, // unknown name — no edge, surfaced.
                1 => {
                    add_call_edge(
                        g,
                        &caller_uid,
                        &repo_wide[0],
                        Provenance::Inferred,
                        CONF_BARE_UNIQUE,
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
        // ── `self.m()` — resolve to a method on the enclosing class. ──
        Some("self") => {
            if let Some(class_fqn) = enclosing_class(&call.enclosing_fqn) {
                let targets: Vec<Uid> = file
                    .symbols
                    .iter()
                    .filter(|s| {
                        s.kind == NodeKind::Method
                            && s.name == call.callee_name
                            && s.container_fqn.as_deref() == Some(class_fqn)
                    })
                    .map(|s| uid_symbol(repo_name, path, &s.fqn))
                    .collect();
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
            // No matching own-class method (a base-class method we cannot see, or
            // `self` outside a class) → no edge, surfaced.
            cov.calls_unresolved += 1;
        }
        // ── `other.m()` — unknown receiver type: fan out to same-named methods. ──
        Some(_) => {
            let methods = index.methods_named(&call.callee_name);
            match methods.len() {
                0 => cov.calls_unresolved += 1,
                _ => {
                    // Even a single same-named method is only an Ambiguous guess:
                    // without the receiver's type we cannot claim it is THE target
                    // (mirrors TS's UnknownReceiver — always Ambiguous).
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

/// Derive the enclosing class fqn from an enclosing method fqn:
/// `"C.m"` → `Some("C")`, `"A.B.m"` → `Some("A.B")`, `"f"` → `None`.
fn enclosing_class(enclosing_fqn: &str) -> Option<&str> {
    enclosing_fqn.rfind('.').map(|idx| &enclosing_fqn[..idx])
}

// ── Repo-wide call index ─────────────────────────────────────────────────────

/// Lookup tables for call resolution, built once per [`assemble_python`].
struct CallIndex {
    /// (module path, symbol name) → uids of Function/Method symbols there.
    by_module_name: BTreeMap<(String, String), Vec<Uid>>,
    /// symbol name → uids of all **Function** symbols repo-wide with that name.
    functions_by_name: BTreeMap<String, Vec<Uid>>,
    /// symbol name → uids of all **Method** symbols repo-wide with that name.
    methods_by_name: BTreeMap<String, Vec<Uid>>,
}

impl CallIndex {
    fn build(repo_name: &str, analyzed: &BTreeMap<String, AnalyzedFile>) -> CallIndex {
        let mut by_module_name: BTreeMap<(String, String), Vec<Uid>> = BTreeMap::new();
        let mut functions_by_name: BTreeMap<String, Vec<Uid>> = BTreeMap::new();
        let mut methods_by_name: BTreeMap<String, Vec<Uid>> = BTreeMap::new();
        for (path, file) in analyzed {
            for sym in &file.symbols {
                let uid = uid_symbol(repo_name, path, &sym.fqn);
                if matches!(sym.kind, NodeKind::Function | NodeKind::Method) {
                    by_module_name
                        .entry((path.clone(), sym.name.clone()))
                        .or_default()
                        .push(uid.clone());
                }
                match sym.kind {
                    NodeKind::Function => functions_by_name
                        .entry(sym.name.clone())
                        .or_default()
                        .push(uid),
                    NodeKind::Method => methods_by_name
                        .entry(sym.name.clone())
                        .or_default()
                        .push(uid),
                    _ => {}
                }
            }
        }
        CallIndex {
            by_module_name,
            functions_by_name,
            methods_by_name,
        }
    }

    /// Function/Method symbols named `name` defined in module `module_path`.
    fn symbols_named_in_module(&self, module_path: &str, name: &str) -> Vec<Uid> {
        self.by_module_name
            .get(&(module_path.to_string(), name.to_string()))
            .cloned()
            .unwrap_or_default()
    }

    /// All **Function** symbols repo-wide named `name` (the bare-name rule's
    /// candidates: a module-level function, not a method).
    fn functions_named(&self, name: &str) -> Vec<Uid> {
        self.functions_by_name
            .get(name)
            .cloned()
            .unwrap_or_default()
    }

    /// All **Method** symbols repo-wide named `name` (the unknown-receiver rule).
    fn methods_named(&self, name: &str) -> Vec<Uid> {
        self.methods_by_name.get(name).cloned().unwrap_or_default()
    }
}

// ── Uid helpers (py-tagged) ──────────────────────────────────────────────────

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
/// enclosing function/method symbol, or the file's Module node at top level.
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

// ── Path helpers (mirroring the TS resolver's normalization) ─────────────────

/// The directory portion of a file path (everything up to the last `/`), or `""`.
fn parent_dir(path: &str) -> String {
    match path.rfind('/') {
        Some(idx) => path[..idx].to_string(),
        None => String::new(),
    }
}

/// Join a base directory and a sub-path with a single `/`. An empty base yields
/// the sub-path; an empty sub yields the base.
fn join(base: &str, sub: &str) -> String {
    if base.is_empty() {
        return sub.to_string();
    }
    if sub.is_empty() {
        return base.to_string();
    }
    format!("{}/{}", base.trim_end_matches('/'), sub)
}

/// The base file name (text after the last `/`), or the whole path if none.
fn base_name(path: &str) -> &str {
    match path.rfind('/') {
        Some(idx) => &path[idx + 1..],
        None => path,
    }
}
