//! Differential testing of the heuristic resolution against SCIP-as-ground-truth
//! (spec A4/A5, §6).
//!
//! [`resolve_differential`] records, per call site, what the slice-1 heuristic
//! *would* resolve to vs what SCIP *did* resolve to — **without building a
//! graph**. It shares the exact per-site decision with the real builder
//! (`assemble_graph_with_scip`) through [`crate::build::resolve_site_targets`],
//! so the two cannot drift (a property the drift test pins).
//!
//! [`accuracy_report`] is a pure function of those [`SiteOutcome`]s: it computes
//! per-class precision/recall over the sites SCIP covers, the SCIP-uncovered
//! count, and the overall figures — exactly per the metric defined in the plan.
//! Because it is pure, it is unit-testable with hand-built outcomes, no SCIP
//! involved.

use std::collections::{BTreeMap, HashMap};

use strata_core::{AnalyzedFile, Direction, EdgeKind, Graph, Provenance, Uid};
use strata_lang_ts::ResolveOptions;
use strata_scip::ScipResolver;

use crate::build::{
    call_confidence, caller_uid, imported_targets_only, resolve_site_targets, CallIndex,
    HeuristicClass,
};
use crate::fs::BTreeMapModuleFs;
use crate::scip_merge::{scip_position, symbol_name_from_moniker};

/// One call site's heuristic outcome vs SCIP ground truth.
///
/// `heuristic_targets` is the set of nodes the heuristic would emit edges to (0,
/// 1, or many); `scip_target` is the node SCIP resolves the callee to, or `None`
/// when SCIP does not cover the site (then it is *excluded* from precision/recall
/// and counted as `uncovered`).
#[derive(Debug, Clone)]
pub struct SiteOutcome {
    /// The file the call site is in (repo-relative, `/`-normalized).
    pub file: String,
    /// Which slice-1 branch the heuristic took for this site.
    pub class: HeuristicClass,
    /// The heuristic candidate target nodes (deduped, deterministic order).
    pub heuristic_targets: Vec<Uid>,
    /// The SCIP-resolved target, or `None` when SCIP did not cover the site.
    pub scip_target: Option<Uid>,
    /// The provenance of the confidence the **heuristic** edge for this site
    /// would carry (`call_confidence`): `Inferred` for `BareSingle`/`ThisMethod`,
    /// `Ambiguous` for `BareMulti`/`UnknownReceiver`. This is what determines the
    /// site's confidence [`Band`] for the calibration view — the band the
    /// heuristic *claims*, which SCIP then grades. (The shipped graph edge for a
    /// SCIP-covered site is a `Resolved` supersede; that is the RESOLVED band's
    /// province, not the heuristic's — see [`Band`].)
    pub heuristic_provenance: Provenance,
}

/// Run BOTH resolutions for every call site and record the outcomes — no graph
/// is built (spec §6).
///
/// `analyzed`/`sources`/`repo_name`/`opts` are exactly the builder's inputs;
/// `scip` is the parsed ground-truth index. For each call site this records the
/// heuristic candidate set + class and the SCIP target via the shared
/// [`resolve_site_targets`], so the recorded SCIP target equals the `RESOLVED`
/// edge target a builder run would produce (drift test).
pub fn resolve_differential(
    analyzed: &BTreeMap<String, AnalyzedFile>,
    sources: &BTreeMap<String, String>,
    repo_name: &str,
    opts: &ResolveOptions,
    scip: &ScipResolver,
) -> Vec<SiteOutcome> {
    // Reproduce the builder's resolution inputs (graph-free).
    let keyset: std::collections::BTreeSet<String> = analyzed.keys().cloned().collect();
    let fs = BTreeMapModuleFs::new(&keyset);
    let imported = imported_targets_only(analyzed, opts, &fs);
    let index = CallIndex::build(repo_name, analyzed);
    let site_resolver = crate::scip_merge::SiteResolver::new(scip, sources, repo_name, analyzed);

    let mut outcomes = Vec::new();
    for (path, file) in analyzed {
        let empty = BTreeMap::new();
        let per_file_imports = imported.get(path).unwrap_or(&empty);
        for call in &file.calls {
            let caller = caller_uid(repo_name, path, call);
            let resolution = resolve_site_targets(
                repo_name,
                path,
                file,
                call,
                per_file_imports,
                &index,
                Some(&site_resolver),
            );
            // Mirror the builder's self-edge suppression: a SCIP target equal to
            // the caller (a recursive call) produces no edge, so it is not a
            // resolved target for accuracy purposes either.
            let scip_target = resolution
                .scip_target
                .map(|t| t.uid().clone())
                .filter(|uid| uid != &caller);
            // The provenance the heuristic edge for this site would carry — the
            // same `call_confidence` the builder uses (single source of truth),
            // so the calibration band reflects exactly the heuristic's claim.
            let (heuristic_provenance, _conf) =
                call_confidence(resolution.class, resolution.heuristic_targets.len());
            outcomes.push(SiteOutcome {
                file: path.clone(),
                class: resolution.class,
                heuristic_targets: resolution.heuristic_targets,
                scip_target,
                heuristic_provenance,
            });
        }
    }
    outcomes
}

/// Run the differential for a **non-TypeScript** language plane and record the
/// per-site outcomes — the language-parametric generalization of
/// [`resolve_differential`].
///
/// The TypeScript path ([`resolve_differential`]) reproduces the *TS builder's*
/// per-site decision graph-free, because the TS builder is the one that resolves
/// calls. Python/Rust/C# each have their **own** linker (`assemble_python` /
/// `assemble_rust` / `assemble_csharp`) that owns their resolution rules and
/// emits the heuristic `CALLS` edges. So for those planes the faithful, no-drift
/// source of the heuristic decision is the **assembled language graph itself**:
/// the caller passes the already-built `graph` (from that language's assembler)
/// and we read back, per call site, the exact heuristic edges the linker emitted
/// and the band they claim. The SCIP ground truth is computed here against the
/// language-tagged symbol nodes, reusing the same [`scip_position`] /
/// [`symbol_name_from_moniker`] / `(file, line, name)` alignment the TS
/// `scip_merge` path uses (those helpers are language-agnostic).
///
/// The resulting [`SiteOutcome`]s feed the **identical** [`accuracy_report`] /
/// [`by_band`](AccuracyReport::by_band) substrate, so the per-band calibration
/// view, the floors, and the monotonicity invariant are computed by the very
/// same code for every language.
///
/// Inputs:
/// - `analyzed` / `sources` — the language's `path → AnalyzedFile` / `path →
///   text` maps (the same the assembler consumed). `sources` keys MUST match the
///   `relative_path`s in `scip` (so a SCIP position can be looked up).
/// - `graph` — the graph the language's assembler produced from `analyzed`.
/// - `repo_name` — the package component of every uid for this corpus project.
/// - `lang` — the language tag the assembler stamps on its uids (`"py"`,
///   `"rust"`, …), so the SCIP→node mapping built here keys nodes the same way.
/// - `scip` — the committed ground-truth index.
pub fn resolve_differential_graph(
    analyzed: &BTreeMap<String, AnalyzedFile>,
    sources: &BTreeMap<String, String>,
    graph: &Graph,
    repo_name: &str,
    lang: &str,
    scip: &ScipResolver,
) -> Vec<SiteOutcome> {
    let scip_index = GraphScipIndex::build(lang, repo_name, analyzed);

    let mut outcomes = Vec::new();
    for (path, file) in analyzed {
        for call in &file.calls {
            let caller = uid_enclosing_lang(lang, repo_name, path, &call.enclosing_fqn);

            // ── Heuristic side: the exact edges the language linker emitted for
            //    this call site, read back from the assembled graph. A site is
            //    identified by (caller, callee simple-name): every heuristic
            //    `CALLS` edge out of `caller` whose target node's name equals the
            //    callee name is an edge the linker produced for this callee at
            //    this caller. They all carry one provenance (the linker stamps a
            //    single band per site), so the band is unambiguous. (A self-edge
            //    the linker suppresses, or a site that resolved to nothing, yields
            //    no such edge — handled below.) ──
            let mut heuristic_targets: Vec<Uid> = Vec::new();
            let mut edge_provenance: Option<Provenance> = None;
            for (edge, target) in graph.neighbors(&caller, Direction::Outgoing, &[EdgeKind::Calls])
            {
                if target.name == call.callee_name {
                    heuristic_targets.push(edge.dst.clone());
                    edge_provenance = Some(edge.provenance);
                }
            }
            heuristic_targets.sort_by(|a, b| a.as_str().cmp(b.as_str()));
            heuristic_targets.dedup();

            // The band the heuristic edge for this site claims. When the linker
            // emitted edges, it is their stamped provenance. When it emitted none
            // (an unresolved site, or a suppressed self-edge), the site still
            // *attempted* a band by its receiver shape — mirroring the TS metric,
            // where even a 0-candidate bare call is binned by its class. A bare /
            // `self` / `Self` call attempts the INFERRED tier; a method call on an
            // unknown receiver attempts AMBIGUOUS. This only affects the band's
            // `sites`/recall bookkeeping (a 0-edge site contributes nothing to
            // precision either way), so an unresolved site is never silently
            // counted as a confirmation or denial.
            let heuristic_provenance =
                edge_provenance.unwrap_or_else(|| attempted_band_provenance(call));

            // ── SCIP side: resolve the callee identifier position to a
            //    language-tagged node uid (or external/None). ──
            let scip_target = scip_index
                .resolve_call_site(scip, sources, path, call)
                .filter(|uid| uid != &caller);

            outcomes.push(SiteOutcome {
                file: path.clone(),
                // The per-class table is a TS-only artefact (its `HeuristicClass`
                // is the TS branch taxonomy). For a non-TS plane we bin every site
                // into a single neutral class; the meaningful calibration is the
                // per-band view, which uses `heuristic_provenance` above.
                class: HeuristicClass::BareSingle,
                heuristic_targets,
                scip_target,
                heuristic_provenance,
            });
        }
    }
    outcomes
}

/// The provenance band a call site *attempts* by its receiver shape, used only
/// when the language linker emitted no edge for the site (so it has no stamped
/// provenance) — see [`resolve_differential_graph`]. Mirrors the TS metric's
/// rule that a site is binned by its receiver class even with zero candidates: a
/// bare / `self` / `Self` call attempts INFERRED; a method call on an unknown
/// receiver attempts AMBIGUOUS.
fn attempted_band_provenance(call: &strata_core::CallRef) -> Provenance {
    match call.receiver.as_deref() {
        None => Provenance::Inferred,
        Some("self") | Some("&self") => Provenance::Inferred,
        // A `::`-scoped `Self::` qualifier also names the enclosing type.
        Some(r) if call.receiver_is_path && last_path_segment(r) == "Self" => Provenance::Inferred,
        Some(_) => Provenance::Ambiguous,
    }
}

/// The last `::`-separated segment of a path (`a::b::C` → `C`). Mirrors the Rust
/// linker's `last_segment`; used only by [`attempted_band_provenance`].
fn last_path_segment(path: &str) -> &str {
    match path.rfind("::") {
        Some(idx) => &path[idx + 2..],
        None => path,
    }
}

/// The node a site enclosed by `enclosing_fqn` is attributed to, for an
/// arbitrary language tag — the language-parametric twin of
/// [`crate::build::uid_enclosing`] (which is hard-tagged `"ts"`). Every language
/// linker attributes a call the same way (enclosing symbol, or the module node
/// at top level), so this reproduces the caller node for the graph read-back.
fn uid_enclosing_lang(lang: &str, repo_name: &str, path: &str, enclosing_fqn: &str) -> Uid {
    if enclosing_fqn.is_empty() {
        Uid::new(lang, repo_name, path, "<module>", "")
    } else {
        Uid::new(lang, repo_name, path, enclosing_fqn, "")
    }
}

/// A language-tagged SCIP→node mapper for the generic ([graph-based])
/// differential. It mirrors the `scip_merge::SiteResolver` alignment — resolve
/// the callee identifier position, read the symbol name off the moniker, and
/// match a `(def_file, def_line, name)` symbol node, with an overload-tolerant
/// unique `(def_file, name)` fallback — but keys its node index on an arbitrary
/// `lang` tag (not the hard-coded `"ts"` of `scip_merge`). The alignment logic
/// is reused unchanged via the public [`scip_position`] /
/// [`symbol_name_from_moniker`] helpers.
///
/// [graph-based]: resolve_differential_graph
struct GraphScipIndex {
    /// Exact `(file, 1-based decl line, simple-name)` → symbol node uid.
    nodes_by_loc: HashMap<(String, u32, String), Uid>,
    /// Overload fallback `(file, simple-name)` → node, only when the name is
    /// unique in the file (`None` = ambiguous; never guessed between).
    by_file_name: HashMap<(String, String), Option<Uid>>,
    /// `file` → module node uid (for a namespace/module-granular SCIP target).
    module_by_file: HashMap<String, Uid>,
}

impl GraphScipIndex {
    fn build(lang: &str, repo_name: &str, analyzed: &BTreeMap<String, AnalyzedFile>) -> Self {
        let mut nodes_by_loc: HashMap<(String, u32, String), Uid> = HashMap::new();
        let mut by_file_name: HashMap<(String, String), Option<Uid>> = HashMap::new();
        let mut module_by_file: HashMap<String, Uid> = HashMap::new();
        for (path, file) in analyzed {
            module_by_file.insert(
                path.clone(),
                Uid::new(lang, repo_name, path, "<module>", ""),
            );
            for sym in &file.symbols {
                let uid = Uid::new(lang, repo_name, path, &sym.fqn, "");
                nodes_by_loc
                    .entry((path.clone(), sym.span.start_line, sym.name.clone()))
                    .or_insert_with(|| uid.clone());
                by_file_name
                    .entry((path.clone(), sym.name.clone()))
                    .and_modify(|slot| {
                        if slot.as_ref() != Some(&uid) {
                            *slot = None;
                        }
                    })
                    .or_insert_with(|| Some(uid.clone()));
            }
        }
        GraphScipIndex {
            nodes_by_loc,
            by_file_name,
            module_by_file,
        }
    }

    /// Resolve the callee identifier of `call` (in `path`) against `scip` and map
    /// the SCIP definition onto a language-tagged node uid. `None` when SCIP does
    /// not cover the site, the moniker is external (no first-party def), or the
    /// definition does not match a known symbol/module node.
    fn resolve_call_site(
        &self,
        scip: &ScipResolver,
        sources: &BTreeMap<String, String>,
        path: &str,
        call: &strata_core::CallRef,
    ) -> Option<Uid> {
        let line0 = call.callee_span.start_line.saturating_sub(1);
        let byte_col = call.callee_span.start_col as usize;
        let line_text = nth_line(sources.get(path)?, line0)?;
        let pos = scip_position(line_text, line0, byte_col);
        let target = scip.resolve_at(path, pos)?;
        // External symbol (no first-party definition): the heuristic targets are
        // all first-party, so an external SCIP target can never confirm one. We
        // record it as uncovered (None) — exactly as the TS path treats a
        // `Package` target it cannot map to a first-party node here.
        if target.is_external {
            return None;
        }
        let def_file = target.def_file.as_deref()?;
        let def_line_1based = target.def_position?.line + 1;
        match symbol_name_from_moniker(&target.moniker) {
            Some(name) => self.match_named_def(def_file, def_line_1based, &name),
            // No trailing name → a module-granular target (e.g. a namespace).
            None => self.module_by_file.get(def_file).cloned(),
        }
    }

    /// Exact `(file, line, name)` first (always correct, keeps shadows apart),
    /// then a UNIQUE `(file, name)` fallback for the overload/decl-line-skew case;
    /// declines an ambiguous name. Identical policy to
    /// `scip_merge::match_named_def`.
    fn match_named_def(&self, def_file: &str, def_line_1based: u32, name: &str) -> Option<Uid> {
        if let Some(uid) =
            self.nodes_by_loc
                .get(&(def_file.to_string(), def_line_1based, name.to_string()))
        {
            return Some(uid.clone());
        }
        self.by_file_name
            .get(&(def_file.to_string(), name.to_string()))
            .cloned()
            .flatten()
    }
}

/// The `line0`-th (0-based) line of `text`, newline-stripped. Mirrors
/// `scip_merge::nth_line` (kept local to avoid widening that module's surface).
fn nth_line(text: &str, line0: u32) -> Option<&str> {
    text.split('\n')
        .nth(line0 as usize)
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
}

/// A design §4.1 confidence **band**: the calibration unit that answers "does a
/// 0.9 edge resolve correctly ≥ 90% of the time?".
///
/// The bands are the §4.1 provenance tiers, by their confidence ranges:
/// `RESOLVED` 0.90–1.0, `EXTRACTED` 0.95–1.0, `INFERRED` 0.40–0.80, `AMBIGUOUS`
/// < 0.40. The variant order is the **monotonicity order** the calibration
/// asserts: measured precision must be non-increasing down the list
/// (`Resolved` ≥ `Extracted` ≥ `Inferred` ≥ `Ambiguous`) — a higher-confidence
/// band that resolves *less* reliably than a lower one is a calibration bug.
///
/// A call site is binned by the band the **heuristic** edge for that site claims
/// (its [`SiteOutcome::heuristic_provenance`]). The heuristic only ever emits
/// `Inferred` or `Ambiguous` edges, so over the heuristic corpus the `Resolved`
/// and `Extracted` bands carry **no** sites and report `precision: None` — those
/// tiers are the compiler's (SCIP supersede) and the deterministic AST's
/// province, never a heuristic guess's ("an inference can never masquerade as a
/// fact", §4.1). SCIP is the **oracle** that grades each heuristic-band edge,
/// not the producer of the edge being graded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Band {
    /// §4.1 `RESOLVED` (0.90–1.0): compiler/SCIP-grade. No heuristic edge.
    Resolved,
    /// §4.1 `EXTRACTED` (0.95–1.0): deterministic AST read. No heuristic call edge.
    Extracted,
    /// §4.1 `INFERRED` (0.40–0.80): a single confident heuristic guess.
    Inferred,
    /// §4.1 `AMBIGUOUS` (< 0.40): an over-included candidate set.
    Ambiguous,
}

/// The bands in canonical (monotonicity) order — the order they appear in the
/// report and docs, highest-confidence first.
pub const ALL_BANDS: [Band; 4] = [
    Band::Resolved,
    Band::Extracted,
    Band::Inferred,
    Band::Ambiguous,
];

impl Band {
    /// The band a heuristic edge of the given provenance falls into (§4.1).
    fn from_provenance(prov: Provenance) -> Band {
        match prov {
            Provenance::Resolved | Provenance::Observed => Band::Resolved,
            Provenance::Extracted => Band::Extracted,
            Provenance::Inferred => Band::Inferred,
            // Ambiguous and (defensively) Model — the latter never gates impact
            // and never appears on a call edge — fall to the lowest band.
            Provenance::Ambiguous | Provenance::Model => Band::Ambiguous,
        }
    }

    /// Stable index for a band (its position in [`ALL_BANDS`]); keys the tally map.
    fn index(self) -> usize {
        match self {
            Band::Resolved => 0,
            Band::Extracted => 1,
            Band::Inferred => 2,
            Band::Ambiguous => 3,
        }
    }

    /// The human-readable §4.1 band name (used in the report table and docs).
    pub fn name(self) -> &'static str {
        match self {
            Band::Resolved => "RESOLVED",
            Band::Extracted => "EXTRACTED",
            Band::Inferred => "INFERRED",
            Band::Ambiguous => "AMBIGUOUS",
        }
    }
}

/// Calibration metrics for one confidence [`Band`]: the measured precision of
/// the heuristic edges that claim that band, scored against SCIP ground truth.
///
/// `precision` is **edge-level** (`confirmed / (confirmed + denied)`), identical
/// in spirit to [`ClassMetrics::precision`] — an over-included edge that SCIP
/// rejects is a denial, so a band that over-includes scores low. It is the
/// honest answer to "does an edge in this band resolve correctly?", and it is
/// `None` (undefined — *not* 1.0 or 0.0) when the band has **zero adjudicable
/// edges** (no sites, or only SCIP-unadjudicable ones).
#[derive(Debug, Clone, PartialEq)]
pub struct BandMetrics {
    /// The band these metrics describe.
    pub band: Band,
    /// SCIP-adjudicable call sites whose heuristic edge claims this band (the
    /// sites that contribute to precision — SCIP covers them).
    pub sites: usize,
    /// Heuristic edges in this band confirmed by SCIP (dst == SCIP's dst).
    pub confirmed: usize,
    /// Heuristic edges in this band SCIP denies (dst ≠ SCIP's dst — e.g. an
    /// over-included candidate the receiver's type rules out).
    pub denied: usize,
    /// Measured edge-level precision `confirmed / (confirmed + denied)`, or
    /// `None` when the band has no adjudicable edges (undefined, stated — never
    /// a vacuous 1.0/0.0).
    pub precision: Option<f64>,
    /// Sites whose heuristic edge claims this band but which SCIP **cannot
    /// adjudicate** (no ground-truth edge). Excluded from `precision`; surfaced
    /// so an unadjudicable site is never silently treated as confirmed or denied.
    pub unadjudicable: usize,
}

/// Precision/recall for one [`HeuristicClass`] over the SCIP-covered sites in
/// that class.
#[derive(Debug, Clone, PartialEq)]
pub struct ClassMetrics {
    /// The class these metrics describe.
    pub class: HeuristicClass,
    /// Number of SCIP-covered sites in this class (the denominator of recall).
    pub sites: usize,
    /// `true_positive_edges / (true_positive_edges + false_positive_edges)`.
    pub precision: f64,
    /// `recall_hits / sites`.
    pub recall: f64,
}

/// The full accuracy report: per-class metrics plus overall figures and the
/// SCIP coverage split.
#[derive(Debug, Clone)]
pub struct AccuracyReport {
    /// Per-class metrics, in a stable class order (see [`ALL_CLASSES`]).
    pub per_class: Vec<ClassMetrics>,
    /// Per-confidence-band calibration metrics, in canonical band order (see
    /// [`ALL_BANDS`]). The **calibration view** (§4.1, the "does 0.9 mean 90%?"
    /// measurement) — a regrouping of the very same per-site SCIP confirmations
    /// the `per_class` numbers use, keyed by the band the heuristic edge claims.
    pub by_band: Vec<BandMetrics>,
    /// Overall precision across all covered sites' edges.
    pub overall_precision: f64,
    /// Overall recall across all covered sites.
    pub overall_recall: f64,
    /// Sites SCIP covered (the metric's universe).
    pub covered_sites: usize,
    /// Sites SCIP did not cover (excluded from precision/recall).
    pub uncovered_sites: usize,
}

impl AccuracyReport {
    /// The metrics for a specific class, if present.
    pub fn class(&self, class: HeuristicClass) -> Option<&ClassMetrics> {
        self.per_class.iter().find(|m| m.class == class)
    }

    /// The calibration metrics for a specific [`Band`], if present.
    pub fn band(&self, band: Band) -> Option<&BandMetrics> {
        self.by_band.iter().find(|m| m.band == band)
    }

    /// Check the §4.1 **monotonicity invariant**: measured precision must be
    /// non-increasing down the band order (`RESOLVED` ≥ `EXTRACTED` ≥
    /// `INFERRED` ≥ `AMBIGUOUS`). A higher-confidence band that resolves *less*
    /// reliably than a lower one is a calibration bug — the confidence ordering
    /// would be lying.
    ///
    /// Bands with no measured precision (`None` — zero adjudicable edges) carry
    /// no claim and are skipped; the invariant is asserted only between bands
    /// that *do* have a measured number. Returns `Err(description)` on the first
    /// inversion, `Ok(())` when the populated bands are ordered.
    pub fn check_band_monotonicity(&self) -> Result<(), String> {
        let mut prev: Option<(Band, f64)> = None;
        for band in ALL_BANDS {
            let Some(p) = self.band(band).and_then(|m| m.precision) else {
                continue;
            };
            if let Some((hi_band, hi_p)) = prev {
                if p > hi_p + 1e-9 {
                    return Err(format!(
                        "band calibration inverted: {} precision {:.4} > higher band {} precision {:.4} \
                         — a higher-confidence band must not resolve less reliably (§4.1)",
                        band.name(),
                        p,
                        hi_band.name(),
                        hi_p,
                    ));
                }
            }
            prev = Some((band, p));
        }
        Ok(())
    }
}

/// The classes, in the canonical order used throughout the report and docs.
pub const ALL_CLASSES: [HeuristicClass; 4] = [
    HeuristicClass::BareSingle,
    HeuristicClass::BareMulti,
    HeuristicClass::ThisMethod,
    HeuristicClass::UnknownReceiver,
];

/// A running tally for one class while folding the outcomes.
#[derive(Default, Clone, Copy)]
struct Tally {
    sites: usize,
    recall_hits: usize,
    true_positive_edges: usize,
    false_positive_edges: usize,
}

impl Tally {
    fn precision(&self) -> f64 {
        ratio(
            self.true_positive_edges,
            self.true_positive_edges + self.false_positive_edges,
        )
    }
    fn recall(&self) -> f64 {
        ratio(self.recall_hits, self.sites)
    }
}

/// A running tally for one confidence band while folding the outcomes.
///
/// Unlike [`Tally`], the band view's precision is **honestly undefined** when
/// there are no adjudicable edges — `precision()` returns `Option<f64>`, never a
/// vacuous 1.0. `sites` counts SCIP-adjudicable sites in the band; `confirmed` /
/// `denied` are the edge tallies; `unadjudicable` is the SCIP-uncovered sites in
/// the band, surfaced but kept out of the precision denominator.
#[derive(Default, Clone, Copy)]
struct BandTally {
    sites: usize,
    confirmed_edges: usize,
    denied_edges: usize,
    unadjudicable: usize,
}

impl BandTally {
    /// Edge-level precision `confirmed / (confirmed + denied)`, or `None` when
    /// the band has no adjudicable edges (undefined — *not* a vacuous 1.0/0.0).
    fn precision(&self) -> Option<f64> {
        let denom = self.confirmed_edges + self.denied_edges;
        if denom == 0 {
            None
        } else {
            Some(self.confirmed_edges as f64 / denom as f64)
        }
    }
}

/// `numer / denom`, defined as **1.0 when `denom == 0`** (an empty class makes no
/// mistakes and misses nothing — the vacuous-truth convention). This keeps the
/// metric total and division-by-zero-free; the `sites` count makes the
/// emptiness visible so a 1.0 from no data is never mistaken for real accuracy.
fn ratio(numer: usize, denom: usize) -> f64 {
    if denom == 0 {
        1.0
    } else {
        numer as f64 / denom as f64
    }
}

/// Compute the [`AccuracyReport`] from a set of [`SiteOutcome`]s (pure).
///
/// Over the sites SCIP covers (`scip_target.is_some()`), per the plan's metric:
///   * a **recall hit** is a site whose `scip_target ∈ heuristic_targets`;
///   * each heuristic target equal to `scip_target` is a **true-positive edge**,
///     each one that differs is a **false-positive edge**;
///   * `precision = tp / (tp + fp)`, `recall = recall_hits / sites_in_class`,
///     both per class and overall (empty denominators ⇒ 1.0, see [`ratio`]).
///
/// SCIP-uncovered sites are excluded from the universe and only counted in
/// [`AccuracyReport::uncovered_sites`].
pub fn accuracy_report(outcomes: &[SiteOutcome]) -> AccuracyReport {
    let mut per_class: BTreeMap<usize, Tally> = BTreeMap::new();
    let mut by_band: BTreeMap<usize, BandTally> = BTreeMap::new();
    let mut overall = Tally::default();
    let mut covered_sites = 0usize;
    let mut uncovered_sites = 0usize;

    for outcome in outcomes {
        // ── Band view: bin EVERY site (covered or not) by the band the
        //    heuristic edge claims; an uncovered site is `unadjudicable` in its
        //    band — surfaced, but kept out of the precision denominator. This is
        //    a pure regrouping of the same per-site confirmation the per-class
        //    metric uses; it never recomputes resolution. ──
        let band = Band::from_provenance(outcome.heuristic_provenance);
        let band_tally = by_band.entry(band.index()).or_default();

        let Some(scip_target) = &outcome.scip_target else {
            uncovered_sites += 1;
            band_tally.unadjudicable += 1;
            continue;
        };
        covered_sites += 1;
        band_tally.sites += 1;

        let tally = per_class.entry(class_index(outcome.class)).or_default();
        tally.sites += 1;
        overall.sites += 1;

        let hit = outcome.heuristic_targets.iter().any(|t| t == scip_target);
        if hit {
            tally.recall_hits += 1;
            overall.recall_hits += 1;
        }
        for target in &outcome.heuristic_targets {
            if target == scip_target {
                tally.true_positive_edges += 1;
                overall.true_positive_edges += 1;
                band_tally.confirmed_edges += 1;
            } else {
                tally.false_positive_edges += 1;
                overall.false_positive_edges += 1;
                band_tally.denied_edges += 1;
            }
        }
    }

    let per_class = ALL_CLASSES
        .iter()
        .map(|&class| {
            let tally = per_class
                .get(&class_index(class))
                .copied()
                .unwrap_or_default();
            ClassMetrics {
                class,
                sites: tally.sites,
                precision: tally.precision(),
                recall: tally.recall(),
            }
        })
        .collect();

    let by_band = ALL_BANDS
        .iter()
        .map(|&band| {
            let tally = by_band.get(&band.index()).copied().unwrap_or_default();
            BandMetrics {
                band,
                sites: tally.sites,
                confirmed: tally.confirmed_edges,
                denied: tally.denied_edges,
                precision: tally.precision(),
                unadjudicable: tally.unadjudicable,
            }
        })
        .collect();

    AccuracyReport {
        per_class,
        by_band,
        overall_precision: overall.precision(),
        overall_recall: overall.recall(),
        covered_sites,
        uncovered_sites,
    }
}

/// Stable index for a class (the order in [`ALL_CLASSES`]); keys the tally map.
fn class_index(class: HeuristicClass) -> usize {
    match class {
        HeuristicClass::BareSingle => 0,
        HeuristicClass::BareMulti => 1,
        HeuristicClass::ThisMethod => 2,
        HeuristicClass::UnknownReceiver => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uid(name: &str) -> Uid {
        Uid::new("ts", "t", "src/x.ts", name, "")
    }

    /// Build a `SiteOutcome` from a class, heuristic target names, and an
    /// optional SCIP target name. The `heuristic_provenance` is derived from the
    /// class via the real [`call_confidence`] mapping — exactly as
    /// `resolve_differential` records it — so the band view is exercised
    /// faithfully (`BareSingle`/`ThisMethod` ⇒ Inferred; the rest ⇒ Ambiguous).
    fn outcome(class: HeuristicClass, heuristic: &[&str], scip: Option<&str>) -> SiteOutcome {
        let (heuristic_provenance, _conf) = call_confidence(class, heuristic.len());
        SiteOutcome {
            file: "src/x.ts".to_string(),
            class,
            heuristic_targets: heuristic.iter().map(|n| uid(n)).collect(),
            scip_target: scip.map(uid),
            heuristic_provenance,
        }
    }

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    // A known mix → exact expected precision/recall per class and overall.
    #[test]
    fn metric_matches_hand_computed_mix() {
        let outcomes = vec![
            // BareSingle: heuristic hits exactly (tp=1).  precision 1, recall 1.
            outcome(HeuristicClass::BareSingle, &["foo"], Some("foo")),
            // BareSingle: heuristic points elsewhere (fp=1, miss). precision 0, recall 0.
            outcome(HeuristicClass::BareSingle, &["wrong"], Some("foo")),
            // BareMulti: over-includes — one right, one wrong (tp=1, fp=1, hit).
            outcome(HeuristicClass::BareMulti, &["foo", "bar"], Some("foo")),
            // UnknownReceiver: two candidates, neither is the SCIP target (fp=2, miss).
            outcome(HeuristicClass::UnknownReceiver, &["a", "b"], Some("c")),
            // ThisMethod uncovered by SCIP — excluded from precision/recall.
            outcome(HeuristicClass::ThisMethod, &["m"], None),
        ];

        let report = accuracy_report(&outcomes);

        assert_eq!(report.covered_sites, 4);
        assert_eq!(report.uncovered_sites, 1);

        // BareSingle: tp=1, fp=1 → precision 0.5; 1 of 2 sites hit → recall 0.5.
        let bs = report.class(HeuristicClass::BareSingle).unwrap();
        assert_eq!(bs.sites, 2);
        assert!(approx(bs.precision, 0.5), "BareSingle precision {bs:?}");
        assert!(approx(bs.recall, 0.5), "BareSingle recall {bs:?}");

        // BareMulti: tp=1, fp=1 → precision 0.5; 1 of 1 site hit → recall 1.0.
        let bm = report.class(HeuristicClass::BareMulti).unwrap();
        assert_eq!(bm.sites, 1);
        assert!(approx(bm.precision, 0.5), "BareMulti precision {bm:?}");
        assert!(approx(bm.recall, 1.0), "BareMulti recall {bm:?}");

        // UnknownReceiver: tp=0, fp=2 → precision 0.0; 0 of 1 hit → recall 0.0.
        let ur = report.class(HeuristicClass::UnknownReceiver).unwrap();
        assert_eq!(ur.sites, 1);
        assert!(
            approx(ur.precision, 0.0),
            "UnknownReceiver precision {ur:?}"
        );
        assert!(approx(ur.recall, 0.0), "UnknownReceiver recall {ur:?}");

        // ThisMethod has no covered sites → vacuous 1.0 / 1.0, sites 0.
        let tm = report.class(HeuristicClass::ThisMethod).unwrap();
        assert_eq!(tm.sites, 0);
        assert!(approx(tm.precision, 1.0));
        assert!(approx(tm.recall, 1.0));

        // Overall edges: tp = 1+1 = 2, fp = 1+1+2 = 4 → precision 2/6 = 1/3.
        // Overall sites: 4, recall hits: 2 → recall 0.5.
        assert!(
            approx(report.overall_precision, 2.0 / 6.0),
            "overall precision {}",
            report.overall_precision
        );
        assert!(
            approx(report.overall_recall, 0.5),
            "overall recall {}",
            report.overall_recall
        );
    }

    // A site with zero heuristic targets (a recall miss, no edges) lowers recall
    // but does not change precision (no edges to be wrong).
    #[test]
    fn empty_heuristic_targets_is_a_recall_miss_only() {
        let outcomes = vec![
            outcome(HeuristicClass::BareSingle, &[], Some("foo")), // miss, no edges
            outcome(HeuristicClass::BareSingle, &["foo"], Some("foo")), // hit, tp=1
        ];
        let report = accuracy_report(&outcomes);
        let bs = report.class(HeuristicClass::BareSingle).unwrap();
        assert_eq!(bs.sites, 2);
        // tp=1, fp=0 → precision 1.0; 1 of 2 sites hit → recall 0.5.
        assert!(approx(bs.precision, 1.0));
        assert!(approx(bs.recall, 0.5));
    }

    // The empty-input / all-uncovered cases: division-by-zero is guarded to 1.0
    // and the universe is empty.
    #[test]
    fn empty_and_all_uncovered_are_vacuous() {
        let empty = accuracy_report(&[]);
        assert_eq!(empty.covered_sites, 0);
        assert_eq!(empty.uncovered_sites, 0);
        assert!(approx(empty.overall_precision, 1.0));
        assert!(approx(empty.overall_recall, 1.0));
        for m in &empty.per_class {
            assert_eq!(m.sites, 0);
            assert!(approx(m.precision, 1.0));
            assert!(approx(m.recall, 1.0));
        }

        let all_uncovered = accuracy_report(&[
            outcome(HeuristicClass::BareSingle, &["foo"], None),
            outcome(HeuristicClass::UnknownReceiver, &["a", "b"], None),
        ]);
        assert_eq!(all_uncovered.covered_sites, 0);
        assert_eq!(all_uncovered.uncovered_sites, 2);
        assert!(approx(all_uncovered.overall_precision, 1.0));
        assert!(approx(all_uncovered.overall_recall, 1.0));
    }

    // The per_class vector is always present in the canonical order, even when
    // some classes have no sites — so the report shape is stable for the docs.
    #[test]
    fn per_class_is_complete_and_ordered() {
        let report = accuracy_report(&[outcome(HeuristicClass::ThisMethod, &["m"], Some("m"))]);
        let classes: Vec<HeuristicClass> = report.per_class.iter().map(|m| m.class).collect();
        assert_eq!(classes, ALL_CLASSES.to_vec());
    }

    // ── Band calibration view (§4.1) ─────────────────────────────────────────

    // The by_band vector is always present in canonical order with all four
    // bands, so the report shape is stable for the docs.
    #[test]
    fn by_band_is_complete_and_ordered() {
        let report = accuracy_report(&[outcome(HeuristicClass::BareSingle, &["foo"], Some("foo"))]);
        let bands: Vec<Band> = report.by_band.iter().map(|m| m.band).collect();
        assert_eq!(bands, ALL_BANDS.to_vec());
    }

    // The heuristic never emits resolved/extracted-grade edges, so those bands
    // carry no sites and report precision None (undefined — *not* a vacuous 1.0).
    // INFERRED holds the single-candidate guesses (BareSingle/ThisMethod);
    // AMBIGUOUS holds the over-included sets (BareMulti/UnknownReceiver).
    #[test]
    fn bands_partition_by_heuristic_provenance_and_top_bands_are_none() {
        let outcomes = vec![
            outcome(HeuristicClass::BareSingle, &["foo"], Some("foo")), // INFERRED confirm
            outcome(HeuristicClass::ThisMethod, &["m"], Some("m")),     // INFERRED confirm
            outcome(HeuristicClass::UnknownReceiver, &["a", "b"], Some("a")), // AMB 1 conf 1 deny
            outcome(HeuristicClass::BareMulti, &["x", "y"], Some("x")), // AMB 1 conf 1 deny
        ];
        let report = accuracy_report(&outcomes);

        // No heuristic edge can claim RESOLVED/EXTRACTED ⇒ undefined precision.
        let resolved = report.band(Band::Resolved).unwrap();
        assert_eq!(resolved.sites, 0);
        assert_eq!(
            resolved.precision, None,
            "RESOLVED must be None, not vacuous"
        );
        let extracted = report.band(Band::Extracted).unwrap();
        assert_eq!(extracted.sites, 0);
        assert_eq!(extracted.precision, None);

        // INFERRED: BareSingle + ThisMethod, both single-candidate confirms ⇒
        // confirmed=2, denied=0 ⇒ precision 1.00 over 2 sites.
        let inferred = report.band(Band::Inferred).unwrap();
        assert_eq!(inferred.sites, 2);
        assert_eq!(inferred.confirmed, 2);
        assert_eq!(inferred.denied, 0);
        assert!(approx(inferred.precision.unwrap(), 1.0));

        // AMBIGUOUS: two over-included sites, each 1 correct + 1 wrong edge ⇒
        // confirmed=2, denied=2 ⇒ precision 0.50 over 2 sites.
        let amb = report.band(Band::Ambiguous).unwrap();
        assert_eq!(amb.sites, 2);
        assert_eq!(amb.confirmed, 2);
        assert_eq!(amb.denied, 2);
        assert!(approx(amb.precision.unwrap(), 0.5));
    }

    // An empty corpus: every band is present and undefined (None), never 1.0.
    #[test]
    fn empty_corpus_has_all_bands_none() {
        let report = accuracy_report(&[]);
        for m in &report.by_band {
            assert_eq!(m.sites, 0);
            assert_eq!(m.confirmed, 0);
            assert_eq!(m.denied, 0);
            assert_eq!(m.unadjudicable, 0);
            assert_eq!(m.precision, None, "{} must be None on empty", m.band.name());
        }
        // Vacuously monotone (nothing to compare).
        assert!(report.check_band_monotonicity().is_ok());
    }

    // A SCIP-unadjudicable site (SCIP did not cover it) is tallied in its band's
    // `unadjudicable` count, EXCLUDED from the precision denominator, never
    // silently confirmed or denied.
    #[test]
    fn unadjudicable_sites_are_separated_from_precision() {
        let outcomes = vec![
            // Adjudicable INFERRED confirm.
            outcome(HeuristicClass::BareSingle, &["foo"], Some("foo")),
            // INFERRED but SCIP-uncovered ⇒ unadjudicable, not in precision.
            outcome(HeuristicClass::BareSingle, &["bar"], None),
            // AMBIGUOUS but SCIP-uncovered ⇒ unadjudicable.
            outcome(HeuristicClass::UnknownReceiver, &["a", "b"], None),
        ];
        let report = accuracy_report(&outcomes);

        let inferred = report.band(Band::Inferred).unwrap();
        // Only the covered site counts toward precision; the uncovered one is
        // surfaced separately.
        assert_eq!(inferred.sites, 1);
        assert_eq!(inferred.confirmed, 1);
        assert_eq!(inferred.denied, 0);
        assert_eq!(inferred.unadjudicable, 1);
        assert!(approx(inferred.precision.unwrap(), 1.0));

        // The AMBIGUOUS band has ONLY an unadjudicable site ⇒ precision None
        // (no adjudicable edges), unadjudicable=1.
        let amb = report.band(Band::Ambiguous).unwrap();
        assert_eq!(amb.sites, 0);
        assert_eq!(amb.unadjudicable, 1);
        assert_eq!(amb.precision, None, "all-unadjudicable band must be None");

        // Coverage bookkeeping unchanged by the band split.
        assert_eq!(report.covered_sites, 1);
        assert_eq!(report.uncovered_sites, 2);
    }

    // The monotonicity invariant: RESOLVED ≥ EXTRACTED ≥ INFERRED ≥ AMBIGUOUS
    // over the *populated* bands. The natural ordering (Inferred precise,
    // Ambiguous over-included) passes; an inverted corpus is caught.
    #[test]
    fn monotonicity_passes_when_ordered_and_fails_when_inverted() {
        // Ordered: INFERRED 1.0 ≥ AMBIGUOUS 0.5.
        let ordered = accuracy_report(&[
            outcome(HeuristicClass::BareSingle, &["foo"], Some("foo")),
            outcome(HeuristicClass::UnknownReceiver, &["a", "b"], Some("a")),
        ]);
        assert!(
            ordered.check_band_monotonicity().is_ok(),
            "INFERRED 1.0 ≥ AMBIGUOUS 0.5 must pass: {:?}",
            ordered.check_band_monotonicity()
        );

        // Inverted: make INFERRED resolve WORSE than AMBIGUOUS. A BareSingle
        // (Inferred) miss with a wrong edge ⇒ INFERRED precision 0.0; an
        // UnknownReceiver (Ambiguous) perfect single-candidate confirm ⇒ 1.0.
        let inverted = accuracy_report(&[
            outcome(HeuristicClass::BareSingle, &["wrong"], Some("foo")), // INFERRED 0.0
            outcome(HeuristicClass::UnknownReceiver, &["a"], Some("a")),  // AMBIGUOUS 1.0
        ]);
        let err = inverted.check_band_monotonicity().unwrap_err();
        assert!(
            err.contains("inverted") && err.contains("AMBIGUOUS"),
            "expected an inversion error mentioning AMBIGUOUS, got: {err}"
        );
    }

    // The band view is a pure REGROUPING: the band edge tallies summed across
    // all bands equal the overall edge tallies (tp+fp), and the adjudicable band
    // sites sum to covered_sites. No edge is double-counted or dropped.
    #[test]
    fn band_tallies_reconcile_with_overall() {
        let outcomes = vec![
            outcome(HeuristicClass::BareSingle, &["foo"], Some("foo")),
            outcome(HeuristicClass::BareMulti, &["x", "y"], Some("x")),
            outcome(HeuristicClass::ThisMethod, &[], Some("m")), // miss, no edges
            outcome(HeuristicClass::UnknownReceiver, &["a", "b"], Some("c")), // 2 wrong
            outcome(HeuristicClass::BareSingle, &["z"], None),   // unadjudicable
        ];
        let report = accuracy_report(&outcomes);

        let confirmed: usize = report.by_band.iter().map(|b| b.confirmed).sum();
        let denied: usize = report.by_band.iter().map(|b| b.denied).sum();
        let band_sites: usize = report.by_band.iter().map(|b| b.sites).sum();
        let unadj: usize = report.by_band.iter().map(|b| b.unadjudicable).sum();

        // tp total: foo(1) + x(1) = 2. fp total: y(1) + a,b(2) = 3.
        assert_eq!(confirmed, 2, "summed band confirmed must equal overall tp");
        assert_eq!(denied, 3, "summed band denied must equal overall fp");
        assert_eq!(band_sites, report.covered_sites);
        assert_eq!(unadj, report.uncovered_sites);
    }
}
