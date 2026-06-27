//! The pure, deterministic graph builder.
//!
//! [`assemble_graph`] turns an already-analyzed file set (a `BTreeMap` of
//! `path → AnalyzedFile`) into the cross-file code-plane graph.  It performs
//! **no IO** and iterates in sorted order, so the same input always yields the
//! same graph.  All cross-file resolution substance — import edges and the
//! recall-biased call edges — lives here.
//!
//! [`build_graph`] is the convenience wrapper used by M4 tests: it analyzes
//! every source file with [`TsAnalyzer`] and then calls [`assemble_graph`].

use std::collections::{BTreeMap, BTreeSet};

use strata_core::{
    AnalyzedFile, CallRef, Confidence, Edge, EdgeKind, Graph, ImportRef, Node, NodeKind,
    Provenance, Span, Uid,
};
use strata_lang_ts::{analyze, resolve, ResolveOptions, ResolveResult};
use strata_scip::ScipResolver;

use crate::fs::BTreeMapModuleFs;
use crate::scip_merge::{MappedTarget, SiteResolver};

const LANG: &str = "ts";
const EXTERNAL_PKG: &str = "<external>";

/// Confidence assigned to a precise (SCIP-resolved) edge (spec A1).
const RESOLVED_CONFIDENCE: f32 = 0.97;

// ── Calibrated heuristic confidences (spec A4) ───────────────────────────────
//
// Stored confidence = min(measured precision, provenance-band ceiling per design
// §4.1): INFERRED band 0.4–0.80; AMBIGUOUS band < 0.40. A heuristic edge must
// NEVER reach or exceed a RESOLVED (0.97) or EXTRACTED (1.0) confidence —
// "an inference can never masquerade as a fact." Calibration informs the number
// WITHIN its band; it cannot break the band. Raw measured precision is recorded
// in the comment; the stored constant is min(measured, ceiling).
//
// Under-populated classes (< 5 sites) keep their slice-1 prior and are tagged
// `uncalibrated`. Provenance — corpus, per-class site counts — is in
// `docs/accuracy/ts-resolution.md`; the gate test (`tests/accuracy_gate.rs`)
// and the report-consistency test keep the precision/recall metrics honest.
// The `tests/confidence_bands.rs` test guards the band invariant. 2026-06-08.

/// Bare call `foo()` resolving to a single heuristic candidate.
/// Provenance: Inferred (band 0.40–0.80, ceiling 0.80).
/// Stored confidence = min(measured 1.00, INFERRED ceiling 0.80) = **0.80**.
/// Raw measured precision was 1.00 over 7 `BareSingle` sites (all 5 emitted
/// bare-single edges were correct), but 1.00 would breach the Inferred ceiling
/// and masquerade as a RESOLVED/EXTRACTED fact. Capped per §4.1.
pub const CONF_BARE_SINGLE: f32 = 0.80;
/// Bare call `foo()` over-including several same-named candidates.
/// Provenance: Ambiguous (band < 0.40).
/// `// uncalibrated: only 1 site` — keeps the slice-1 prior (measured precision
/// on the single site was 0.50, too few to adopt). Already within band.
pub const CONF_BARE_MULTI: f32 = 0.35;
/// `this.method()` resolved within the enclosing class.
/// Provenance: Inferred (band 0.40–0.80, ceiling 0.80).
/// `// uncalibrated: only 2 sites` — keeps the slice-1 prior (measured precision
/// 1.00 on 2 sites, too few to adopt). Already at the ceiling; in-band.
pub const CONF_THIS_METHOD: f32 = 0.8;
/// `other.method()` with no type info (all same-named methods repo-wide).
/// Provenance: Ambiguous (band < 0.40, ceiling exclusive).
/// Stored confidence = min(measured 0.50, AMBIGUOUS ceiling 0.39) = **0.39**.
/// Raw measured precision was 0.50 over 7 `UnknownReceiver` sites, but 0.50
/// exceeds the Ambiguous ceiling and would outrank Inferred edges. Capped per §4.1.
pub const CONF_UNKNOWN_RECEIVER: f32 = 0.39;

/// Per-repo resolution coverage, surfaced through `IndexStats` (spec A5).
///
/// A "site" is one call site or one imported-name occurrence. The three outcome
/// buckets do **not** necessarily sum to `sites_total`: a site that SCIP cannot
/// resolve *and* the heuristic cannot point anywhere (e.g. an unresolved import)
/// counts only toward `sites_total`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ResolutionCoverage {
    /// Every call/import-name site considered.
    pub sites_total: usize,
    /// Sites resolved precisely by SCIP (one `RESOLVED` edge each).
    pub sites_resolved: usize,
    /// Sites that fell back to the heuristic with a non-ambiguous result.
    pub sites_heuristic: usize,
    /// Sites that fell back to the heuristic with an ambiguous result.
    pub sites_ambiguous: usize,
}

/// The per-site outcome, used to tally [`ResolutionCoverage`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SiteOutcome {
    Resolved,
    Heuristic,
    Ambiguous,
    /// Considered but produced no edge (no SCIP cover, no heuristic candidate).
    None,
}

/// PURE, deterministic graph construction from an already-analyzed file set.
///
/// `analyzed`: sorted map of normalized repo-relative path → `AnalyzedFile`.
/// `repo_name`: the name for the `Repo` node and the `package` component of
/// every in-repo `Uid`.  `opts`: resolver options (from tsconfig).
///
/// The function is deterministic: given the same `analyzed` map it always
/// produces the same graph.  The `BTreeMap` guarantees sorted-key iteration.
///
/// This is exactly the slice-1 heuristic build — it delegates to
/// [`assemble_graph_with_scip`] with `scip = None`, which is byte-identical to
/// the original behaviour.
pub fn assemble_graph(
    analyzed: &BTreeMap<String, AnalyzedFile>,
    repo_name: &str,
    opts: &ResolveOptions,
) -> Graph {
    let empty = BTreeMap::new();
    assemble_graph_with_scip(analyzed, repo_name, opts, None, &empty)
}

/// Graph construction with optional **precise** (SCIP) resolution.
///
/// With `scip = Some(resolver)`, each call/import-name site is first resolved
/// via SCIP: a hit becomes a single `RESOLVED` edge (confidence
/// [`RESOLVED_CONFIDENCE`]) to the mapped target, *superseding* the heuristic
/// edge(s) for that site (spec A1); a miss falls back to the exact slice-1
/// heuristic (spec R4 — never dropped). With `scip = None` every site takes the
/// heuristic path, so the graph is byte-identical to slice 1.
///
/// `sources` (path → text) is only read on the SCIP path, to convert
/// Tree-sitter byte columns to the UTF-16 positions SCIP expects; an empty map
/// is fine when `scip = None`.
pub fn assemble_graph_with_scip(
    analyzed: &BTreeMap<String, AnalyzedFile>,
    repo_name: &str,
    opts: &ResolveOptions,
    scip: Option<&ScipResolver>,
    sources: &BTreeMap<String, String>,
) -> Graph {
    assemble_with_coverage(analyzed, repo_name, opts, scip, sources).0
}

/// The shared implementation behind [`assemble_graph`] and
/// [`assemble_graph_with_scip`], also returning [`ResolutionCoverage`] (spec A5)
/// for `index_repo`'s stats and for hermetic coverage tests.
pub fn assemble_with_coverage(
    analyzed: &BTreeMap<String, AnalyzedFile>,
    repo_name: &str,
    opts: &ResolveOptions,
    scip: Option<&ScipResolver>,
    sources: &BTreeMap<String, String>,
) -> (Graph, ResolutionCoverage) {
    let mut g = Graph::new();
    let mut cov = ResolutionCoverage::default();

    // The file keyset is the existence oracle for the resolver, keeping this
    // function pure.
    let keyset: BTreeSet<String> = analyzed.keys().cloned().collect();
    let fs = BTreeMapModuleFs::new(&keyset);

    // The precise-resolution overlay (built once); `None` ⇒ pure heuristic.
    let site_resolver =
        scip.map(|resolver| SiteResolver::new(resolver, sources, repo_name, analyzed));

    // ── Repo node ──
    let repo_uid = uid_repo(repo_name);
    g.add_node(extracted_node(
        repo_uid.clone(),
        NodeKind::Repo,
        repo_name,
        repo_name,
        "",
        Span::default(),
    ));

    // ── Phase 1: Module + symbol nodes, structural edges ──
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

        // Module —MEMBER_OF→ Repo, Repo —DEFINES→ Module.
        add_structural_pair(&mut g, &module_uid, &repo_uid);

        for sym in &file.symbols {
            let sym_uid = uid_symbol(repo_name, path, &sym.fqn);
            g.add_node(extracted_node(
                sym_uid.clone(),
                sym.kind,
                &sym.name,
                &sym.fqn,
                path,
                sym.span,
            ));
        }

        // Structural edges for symbols (done after all symbol nodes in the file
        // exist so a method can attach to its class defined later in the file).
        for sym in &file.symbols {
            let sym_uid = uid_symbol(repo_name, path, &sym.fqn);
            let container_uid = match &sym.container_fqn {
                // Method attaches to its class in the same file when present.
                Some(class_fqn) => {
                    let class_uid = uid_symbol(repo_name, path, class_fqn);
                    if g.get_node(&class_uid).is_some() {
                        class_uid
                    } else {
                        module_uid.clone()
                    }
                }
                // Top-level symbol attaches to its module.
                None => module_uid.clone(),
            };
            add_structural_pair(&mut g, &sym_uid, &container_uid);
        }
    }

    // ── Phase 2: import edges + per-file imported-name → resolved-module map ──
    // `imported_targets[path]` maps an imported local name to the module path it
    // resolved to (for `Resolved` imports only). Used by call rule 1.
    let mut imported_targets: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();

    for (path, file) in analyzed {
        let module_uid = uid_module(repo_name, path);
        let mut per_file: BTreeMap<String, String> = BTreeMap::new();

        for import in &file.imports {
            resolve_import(
                &mut g,
                repo_name,
                path,
                &module_uid,
                import,
                opts,
                &fs,
                &mut per_file,
            );
        }

        imported_targets.insert(path.clone(), per_file);
    }

    // ── Index for call resolution ──
    let index = CallIndex::build(repo_name, analyzed);
    debug_assert_eq!(
        imported_targets,
        imported_targets_only(analyzed, opts, &fs),
        "graph-free imported_targets must equal the builder's"
    );

    // ── Phase 3: call edges (recall-biased, SCIP-superseded) ──
    for (path, file) in analyzed {
        let empty = BTreeMap::new();
        let per_file_imports = imported_targets.get(path).unwrap_or(&empty);
        for call in &file.calls {
            let outcome = resolve_call(
                &mut g,
                repo_name,
                path,
                file,
                call,
                per_file_imports,
                &index,
                site_resolver.as_ref(),
            );
            cov.tally(outcome);
        }
    }

    (g, cov)
}

impl ResolutionCoverage {
    fn tally(&mut self, outcome: SiteOutcome) {
        self.sites_total += 1;
        match outcome {
            SiteOutcome::Resolved => self.sites_resolved += 1,
            SiteOutcome::Heuristic => self.sites_heuristic += 1,
            SiteOutcome::Ambiguous => self.sites_ambiguous += 1,
            SiteOutcome::None => {}
        }
    }
}

/// Convenience wrapper: analyze every source file then call [`assemble_graph`].
///
/// `files`: normalized repo-relative path → source text. The result is
/// identical to analyzing each file individually and passing the resulting map
/// to [`assemble_graph`] — the M4 tests rely on this equivalence.
pub fn build_graph(
    files: &BTreeMap<String, String>,
    repo_name: &str,
    opts: &ResolveOptions,
) -> Graph {
    let analyzed: BTreeMap<String, AnalyzedFile> = files
        .iter()
        .map(|(path, src)| (path.clone(), analyze(path, src)))
        .collect();
    assemble_graph(&analyzed, repo_name, opts)
}

/// Resolve one import statement to its `IMPORTS` edge and update `per_file`.
///
/// This is the exact slice-1 heuristic import resolution (module-granular). The
/// recall gaps precise resolution closes are all on the *call* side; import
/// edges already resolve to the correct target module/package, so they are left
/// to the heuristic and are byte-identical with or without SCIP. (Upgrading
/// import-edge provenance to `Resolved` carries no recall value and is deferred
/// to keep this path unchanged.)
#[allow(clippy::too_many_arguments)]
fn resolve_import(
    g: &mut Graph,
    repo_name: &str,
    path: &str,
    module_uid: &Uid,
    import: &ImportRef,
    opts: &ResolveOptions,
    fs: &BTreeMapModuleFs,
    per_file: &mut BTreeMap<String, String>,
) {
    match resolve(&import.specifier, path, opts, fs) {
        ResolveResult::Resolved(target) => {
            let target_uid = uid_module(repo_name, &target);
            g.add_edge(import_edge(module_uid, &target_uid));
            for name in &import.imported_names {
                per_file.insert(name.clone(), target.clone());
            }
        }
        ResolveResult::External(pkg) => {
            let pkg_uid = uid_package(&pkg);
            // The Package node may be referenced by several modules;
            // add_node is idempotent by uid.
            g.add_node(extracted_node(
                pkg_uid.clone(),
                NodeKind::Package,
                &pkg,
                &pkg,
                "",
                Span::default(),
            ));
            g.add_edge(import_edge(module_uid, &pkg_uid));
        }
        ResolveResult::Unresolved => {
            // No edge, no invented target.
        }
    }
}

/// The graph-free per-file imported-name → resolved-module map (call rule 1's
/// input), built without adding any edges.
///
/// This mirrors the `Resolved` branch of [`resolve_import`] exactly — and only
/// that branch, since `External`/`Unresolved` imports never seed a call-rule-1
/// candidate. The differential harness uses it to reproduce the builder's
/// candidate set; a `debug_assert` in [`assemble_with_coverage`] pins the two
/// equal.
pub(crate) fn imported_targets_only(
    analyzed: &BTreeMap<String, AnalyzedFile>,
    opts: &ResolveOptions,
    fs: &BTreeMapModuleFs,
) -> BTreeMap<String, BTreeMap<String, String>> {
    let mut out: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    for (path, file) in analyzed {
        let mut per_file: BTreeMap<String, String> = BTreeMap::new();
        for import in &file.imports {
            if let ResolveResult::Resolved(target) = resolve(&import.specifier, path, opts, fs) {
                for name in &import.imported_names {
                    per_file.insert(name.clone(), target.clone());
                }
            }
        }
        out.insert(path.clone(), per_file);
    }
    out
}

/// Resolve one call site to its `CALLS` edge(s), returning the [`SiteOutcome`].
///
/// Precise first: if `site_resolver` resolves the callee identifier via SCIP, a
/// single `RESOLVED` edge to the mapped target supersedes any heuristic edge
/// (spec A1) and the function returns early. Otherwise it runs the exact slice-1
/// heuristic (spec R4) — identical behaviour to slice 1 when `site_resolver` is
/// `None`.
///
/// The *decision* of what to resolve to — the SCIP target and the heuristic
/// candidate set + [`HeuristicClass`] — is computed by the shared, graph-free
/// [`resolve_site_targets`]; this function only turns that decision into edges
/// (materializing an external `Package` node on the SCIP path). The differential
/// harness ([`crate::resolve_differential`]) consumes the *same* helper, so the
/// two can never drift (Definition of Done #2).
#[allow(clippy::too_many_arguments)]
fn resolve_call(
    g: &mut Graph,
    repo_name: &str,
    path: &str,
    file: &AnalyzedFile,
    call: &CallRef,
    per_file_imports: &BTreeMap<String, String>,
    index: &CallIndex,
    site_resolver: Option<&SiteResolver>,
) -> SiteOutcome {
    let caller_uid = caller_uid(repo_name, path, call);
    let resolution = resolve_site_targets(
        repo_name,
        path,
        file,
        call,
        per_file_imports,
        index,
        site_resolver,
    );

    // ── Precise path (supersede): a single RESOLVED edge to the mapped target. ──
    if let Some(target) = resolution.scip_target {
        let target_uid = materialize_target(g, target);
        // A self-edge (a recursive call resolving to its own enclosing node)
        // adds no impact information; skip it but still count as resolved.
        if target_uid != caller_uid {
            g.add_edge(call_edge(
                &caller_uid,
                &target_uid,
                Provenance::Resolved,
                RESOLVED_CONFIDENCE,
            ));
        }
        return SiteOutcome::Resolved;
    }

    // ── Heuristic fallback (exact slice-1 behaviour). ──
    if resolution.heuristic_targets.is_empty() {
        return SiteOutcome::None; // cannot point at an unknown target
    }

    // Confidence depends on receiver kind and candidate count.
    let (prov, conf) = call_confidence(resolution.class, resolution.heuristic_targets.len());
    for target in &resolution.heuristic_targets {
        g.add_edge(call_edge(&caller_uid, target, prov, conf));
    }
    match prov {
        Provenance::Ambiguous => SiteOutcome::Ambiguous,
        _ => SiteOutcome::Heuristic,
    }
}

/// The graph node that *makes* a call: the enclosing symbol, or the Module at
/// top level. Shared by the builder and the differential harness so a call site
/// is always attributed to the same caller.
pub(crate) fn caller_uid(repo_name: &str, path: &str, call: &CallRef) -> Uid {
    uid_enclosing(repo_name, path, &call.enclosing_fqn)
}

/// The node a site enclosed by `enclosing_fqn` in `path` is attributed to: the
/// enclosing function/method symbol, or the file's Module node at top level
/// (empty `enclosing_fqn`). Shared by call attribution ([`caller_uid`]) and
/// contract-plane producer attribution.
pub(crate) fn uid_enclosing(repo_name: &str, path: &str, enclosing_fqn: &str) -> Uid {
    if enclosing_fqn.is_empty() {
        uid_module(repo_name, path)
    } else {
        uid_symbol(repo_name, path, enclosing_fqn)
    }
}

/// The slice-1 call-resolution branch a bare/`this.`/`other.` call takes. This
/// is the unit the accuracy metric is computed *per* (spec A5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeuristicClass {
    /// Bare call `foo()` with exactly one heuristic candidate.
    BareSingle,
    /// Bare call `foo()` with two or more heuristic candidates (over-included).
    BareMulti,
    /// `this.method()` resolved within the enclosing class.
    ThisMethod,
    /// `other.method()` with no type info — all same-named methods repo-wide.
    UnknownReceiver,
}

/// The shared per-site resolution *decision*, free of any graph mutation: the
/// SCIP target (when SCIP covers the site) and the heuristic candidate set with
/// the [`HeuristicClass`] the site falls into.
///
/// Both [`resolve_call`] (which turns this into edges) and the differential
/// harness consume this, so the SCIP target and heuristic targets recorded for a
/// site are *by construction* the same in both paths.
pub(crate) struct SiteResolution {
    /// The SCIP-mapped target for the callee identifier, if SCIP covered it.
    pub scip_target: Option<MappedTarget>,
    /// The heuristic candidate target nodes (deduped, deterministic order).
    pub heuristic_targets: Vec<Uid>,
    /// Which slice-1 branch this call falls into (for the per-class metric). The
    /// `Bare*` split reflects the heuristic candidate count.
    pub class: HeuristicClass,
}

/// Compute the SCIP target and the heuristic candidate set + class for one call
/// site, **without touching the graph**.
///
/// This is the single source of truth the builder and the differential harness
/// share. The heuristic candidate logic is byte-identical to the slice-1
/// branches; the SCIP target is the same `MappedTarget` the builder would
/// materialize.
#[allow(clippy::too_many_arguments)]
pub(crate) fn resolve_site_targets(
    repo_name: &str,
    path: &str,
    file: &AnalyzedFile,
    call: &CallRef,
    per_file_imports: &BTreeMap<String, String>,
    index: &CallIndex,
    site_resolver: Option<&SiteResolver>,
) -> SiteResolution {
    // ── SCIP target (target the callee IDENTIFIER position). ──
    let scip_target = site_resolver.and_then(|resolver| {
        // `callee_span` is 1-based line / 0-based byte col; SCIP wants 0-based.
        let line0 = call.callee_span.start_line.saturating_sub(1);
        let byte_col = call.callee_span.start_col as usize;
        resolver.resolve_site(path, line0, byte_col)
    });

    // ── Heuristic candidates (exact slice-1 behaviour), deduped + ordered. ──
    let mut candidates: BTreeSet<Uid> = BTreeSet::new();
    match call.receiver.as_deref() {
        // Rule 1: bare call `foo()`.
        None => {
            // Local Function/Method symbols named callee_name in this file.
            for sym in &file.symbols {
                if matches!(sym.kind, NodeKind::Function | NodeKind::Method)
                    && sym.name == call.callee_name
                {
                    candidates.insert(uid_symbol(repo_name, path, &sym.fqn));
                }
            }
            // Imported name resolving to a module: symbols named callee_name there.
            if let Some(target_module) = per_file_imports.get(&call.callee_name) {
                for uid in index.symbols_named_in_module(target_module, &call.callee_name) {
                    candidates.insert(uid);
                }
            }
        }
        // Rule 2: `this.method()`.
        Some("this") => {
            if let Some(class_fqn) = enclosing_class(&call.enclosing_fqn) {
                for sym in &file.symbols {
                    if sym.kind == NodeKind::Method
                        && sym.name == call.callee_name
                        && sym.container_fqn.as_deref() == Some(class_fqn)
                    {
                        candidates.insert(uid_symbol(repo_name, path, &sym.fqn));
                    }
                }
            }
        }
        // Rule 3: `other.method()` — no type info, all methods repo-wide.
        Some(_) => {
            for uid in index.methods_named(&call.callee_name) {
                candidates.insert(uid);
            }
        }
    }

    let class = classify_site(call.receiver.as_deref(), candidates.len());
    SiteResolution {
        scip_target,
        heuristic_targets: candidates.into_iter().collect(),
        class,
    }
}

/// The [`HeuristicClass`] for a call given its receiver and heuristic candidate
/// count. `Bare*` splits on count; the receiver kind decides the rest.
fn classify_site(receiver: Option<&str>, candidate_count: usize) -> HeuristicClass {
    match receiver {
        None => {
            if candidate_count <= 1 {
                HeuristicClass::BareSingle
            } else {
                HeuristicClass::BareMulti
            }
        }
        Some("this") => HeuristicClass::ThisMethod,
        Some(_) => HeuristicClass::UnknownReceiver,
    }
}

/// Ensure a SCIP-mapped target exists as a node and return its `Uid`. First-party
/// nodes already exist (created in phase 1); an external `Package` node is added
/// idempotently here because a *call* (unlike an import) may be the first thing
/// to reference it.
fn materialize_target(g: &mut Graph, target: MappedTarget) -> Uid {
    if let MappedTarget::External(pkg_uid) = &target {
        if g.get_node(pkg_uid).is_none() {
            // Recover the package's display name from the uid's fqn field.
            let name = pkg_uid
                .as_str()
                .rsplit('|')
                .nth(1)
                .unwrap_or("")
                .to_string();
            g.add_node(extracted_node(
                pkg_uid.clone(),
                NodeKind::Package,
                &name,
                &name,
                "",
                Span::default(),
            ));
        }
    }
    target.uid().clone()
}

/// The `(Provenance, confidence)` for a heuristic call edge of a given
/// [`HeuristicClass`].
///
/// Confidence is the **calibrated** per-class number (see the `CONF_*` constants
/// and `docs/accuracy/ts-resolution.md`). Provenance still distinguishes a
/// single confident heuristic guess (`Inferred`) from an over-included set
/// (`Ambiguous`): `BareSingle`/`ThisMethod` are `Inferred`; `BareMulti` and
/// `UnknownReceiver` are `Ambiguous`.
pub(crate) fn call_confidence(class: HeuristicClass, _count: usize) -> (Provenance, f32) {
    match class {
        HeuristicClass::BareSingle => (Provenance::Inferred, CONF_BARE_SINGLE),
        HeuristicClass::BareMulti => (Provenance::Ambiguous, CONF_BARE_MULTI),
        HeuristicClass::ThisMethod => (Provenance::Inferred, CONF_THIS_METHOD),
        HeuristicClass::UnknownReceiver => (Provenance::Ambiguous, CONF_UNKNOWN_RECEIVER),
    }
}

/// Derive the enclosing class fqn from an enclosing method fqn:
/// `"C.m"` → `Some("C")`, `"A.B.m"` → `Some("A.B")`, `"f"` → `None`.
fn enclosing_class(enclosing_fqn: &str) -> Option<&str> {
    enclosing_fqn.rfind('.').map(|idx| &enclosing_fqn[..idx])
}

// ── Repo-wide call index ───────────────────────────────────────────────────

/// Lookup tables for call resolution, built once per `assemble_graph`.
pub(crate) struct CallIndex {
    /// (module path, symbol name) -> uids of Function/Method symbols there.
    by_module_name: BTreeMap<(String, String), Vec<Uid>>,
    /// symbol name -> uids of all Method symbols repo-wide with that name.
    methods_by_name: BTreeMap<String, Vec<Uid>>,
}

impl CallIndex {
    pub(crate) fn build(repo_name: &str, analyzed: &BTreeMap<String, AnalyzedFile>) -> CallIndex {
        let mut by_module_name: BTreeMap<(String, String), Vec<Uid>> = BTreeMap::new();
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
                if sym.kind == NodeKind::Method {
                    methods_by_name
                        .entry(sym.name.clone())
                        .or_default()
                        .push(uid);
                }
            }
        }
        CallIndex {
            by_module_name,
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

    /// All Method symbols repo-wide named `name`.
    fn methods_named(&self, name: &str) -> Vec<Uid> {
        self.methods_by_name.get(name).cloned().unwrap_or_default()
    }
}

// ── Uid helpers ────────────────────────────────────────────────────────────

fn uid_repo(repo_name: &str) -> Uid {
    Uid::new(LANG, repo_name, "", repo_name, "")
}

pub(crate) fn uid_module(repo_name: &str, path: &str) -> Uid {
    Uid::new(LANG, repo_name, path, "<module>", "")
}

pub(crate) fn uid_symbol(repo_name: &str, path: &str, fqn: &str) -> Uid {
    Uid::new(LANG, repo_name, path, fqn, "")
}

pub(crate) fn uid_package(pkg: &str) -> Uid {
    Uid::new(LANG, EXTERNAL_PKG, "", pkg, "")
}

// ── Node / edge helpers ────────────────────────────────────────────────────

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

/// Add the reciprocal MEMBER_OF / DEFINES pair: `member` —MEMBER_OF→ `container`
/// and `container` —DEFINES→ `member`. Both Extracted, confidence 1.0.
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

fn call_edge(src: &Uid, dst: &Uid, prov: Provenance, conf: f32) -> Edge {
    Edge {
        src: src.clone(),
        dst: dst.clone(),
        kind: EdgeKind::Calls,
        provenance: prov,
        confidence: Confidence::new(conf),
    }
}

// ── misc ───────────────────────────────────────────────────────────────────

/// The base file name (text after the last `/`), or the whole path if none.
fn base_name(path: &str) -> &str {
    match path.rfind('/') {
        Some(idx) => &path[idx + 1..],
        None => path,
    }
}
