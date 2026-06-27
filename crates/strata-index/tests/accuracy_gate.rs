//! The committed-corpus accuracy gate (spec A5, §8.1) and report-consistency
//! check.
//!
//! Hermetic: each corpus project ships its sources + a committed `index.scip`
//! (no Node at test time). The harness runs `resolve_differential` +
//! `accuracy_report` over the *whole* corpus and asserts documented floors — a
//! regression (the heuristic gets worse, or SCIP coverage drops) fails the
//! build. The same computed report is checked against the committed
//! `docs/accuracy/ts-resolution.md` so the published numbers cannot silently
//! drift.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;
use strata_core::Uid;
use strata_index::{accuracy_report, resolve_differential, AccuracyReport, Band, HeuristicClass};
use strata_lang_ts::{analyze, ResolveOptions};
use strata_scip::ScipResolver;

#[path = "support/accuracy.rs"]
mod support;
use support::{assert_band_floors, assert_band_nonvacuous, AccuracyDoc, BandFloor, EPS};

/// One corpus project: its fixture directory name and the repo name the indexer
/// would assign it (the final path component).
struct CorpusProject {
    /// Directory under `tests/fixtures/accuracy/` (the `resolve` fixture lives one
    /// level up; see [`project_dir`]).
    name: &'static str,
    /// The repo name = the `package` component of every uid for this project.
    repo: &'static str,
}

/// The committed corpus. `resolve` is the M2 fixture (reused); `methods` adds
/// this-method/bare-multi/unknown-receiver coverage; the Slice 13 projects
/// (`reexports`, `inheritance`, `async_hof`, `dynamic`) expand it toward
/// statistical meaning across re-export chains, inheritance/override, async +
/// higher-order calls, and overloads + namespace + dynamic access.
const CORPUS: &[CorpusProject] = &[
    CorpusProject {
        name: "resolve",
        repo: "strata-index-resolve",
    },
    CorpusProject {
        name: "methods",
        repo: "strata-accuracy-methods",
    },
    CorpusProject {
        name: "reexports",
        repo: "strata-accuracy-reexports",
    },
    CorpusProject {
        name: "inheritance",
        repo: "strata-accuracy-inheritance",
    },
    CorpusProject {
        name: "async_hof",
        repo: "strata-accuracy-async-hof",
    },
    CorpusProject {
        name: "dynamic",
        repo: "strata-accuracy-dynamic",
    },
];

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}

/// The on-disk directory for a corpus project. `resolve` is at
/// `fixtures/resolve`; everything else is under `fixtures/accuracy/<name>`.
fn project_dir(p: &CorpusProject) -> PathBuf {
    if p.name == "resolve" {
        fixtures_root().join("resolve")
    } else {
        fixtures_root().join("accuracy").join(p.name)
    }
}

/// Load `src/*.ts` for a project into `src/<name>.ts -> text`.
fn load_sources(dir: &Path) -> BTreeMap<String, String> {
    let src = dir.join("src");
    let mut map = BTreeMap::new();
    for entry in std::fs::read_dir(&src).expect("read src dir") {
        let p = entry.expect("entry").path();
        if p.extension().and_then(|e| e.to_str()) == Some("ts") {
            let name = p.file_name().unwrap().to_str().unwrap();
            map.insert(
                format!("src/{name}"),
                std::fs::read_to_string(&p).expect("read source"),
            );
        }
    }
    map
}

/// Run the differential over one project and return its per-site outcomes.
fn outcomes_for(p: &CorpusProject) -> Vec<strata_index::SiteOutcome> {
    let dir = project_dir(p);
    let sources = load_sources(&dir);
    let analyzed: BTreeMap<_, _> = sources
        .iter()
        .map(|(path, text)| (path.clone(), analyze(path, text)))
        .collect();
    let bytes = std::fs::read(dir.join("index.scip")).expect("committed index.scip");
    let resolver = ScipResolver::from_bytes(&bytes).expect("index.scip parses");
    resolve_differential(
        &analyzed,
        &sources,
        p.repo,
        &ResolveOptions::default(),
        &resolver,
    )
}

/// The accuracy report over the WHOLE committed corpus.
fn corpus_report() -> AccuracyReport {
    let mut all = Vec::new();
    for p in CORPUS {
        all.extend(outcomes_for(p));
    }
    accuracy_report(&all)
}

/// A human-readable rendering of the report (printed by the gate for the log,
/// and compared against the committed markdown table).
fn render_report(report: &AccuracyReport) -> String {
    let mut s = String::new();
    s.push_str("class            sites  precision  recall\n");
    for m in &report.per_class {
        s.push_str(&format!(
            "{:<15}  {:>4}     {:>5.2}   {:>5.2}\n",
            class_name(m.class),
            m.sites,
            m.precision,
            m.recall
        ));
    }
    s.push_str(&format!(
        "overall: precision {:.2}, recall {:.2}, covered {}, uncovered {}\n",
        report.overall_precision,
        report.overall_recall,
        report.covered_sites,
        report.uncovered_sites
    ));
    s
}

fn class_name(c: HeuristicClass) -> &'static str {
    match c {
        HeuristicClass::BareSingle => "BareSingle",
        HeuristicClass::BareMulti => "BareMulti",
        HeuristicClass::ThisMethod => "ThisMethod",
        HeuristicClass::UnknownReceiver => "UnknownReceiver",
    }
}

/// The per-band calibration table (§4.1): for each confidence band, the measured
/// edge-level precision of the heuristic edges that claim it, scored against
/// SCIP. `precision` is shown as `--` when undefined (no adjudicable edges) — an
/// honest blank, never a vacuous 1.00.
fn render_band_table(report: &AccuracyReport) -> String {
    let mut s = String::new();
    s.push_str("band       sites  confirmed  denied  precision  unadjudicable\n");
    for m in &report.by_band {
        let prec = match m.precision {
            Some(p) => format!("{p:>5.2}"),
            None => "   --".to_string(),
        };
        s.push_str(&format!(
            "{:<9}  {:>4}   {:>8}  {:>5}     {}      {:>6}\n",
            m.band.name(),
            m.sites,
            m.confirmed,
            m.denied,
            prec,
            m.unadjudicable,
        ));
    }
    s
}

// ── Inspection: print the corpus report (always; run with --nocapture) ───────

#[test]
fn print_corpus_report() {
    let report = corpus_report();
    eprintln!("\n=== TS resolution accuracy over the committed corpus ===\n");
    eprint!("{}", render_report(&report));

    eprintln!("\n=== Per-band calibration (§4.1: does 0.9 mean ≥90%?) ===\n");
    eprint!("{}", render_band_table(&report));
    match report.check_band_monotonicity() {
        Ok(()) => eprintln!("monotonicity: OK (RESOLVED ≥ EXTRACTED ≥ INFERRED ≥ AMBIGUOUS)"),
        Err(e) => eprintln!("monotonicity: VIOLATED — {e}"),
    }

    eprintln!("\nper-class detail:");
    for m in &report.per_class {
        eprintln!(
            "  {:<16} sites={} precision={:.4} recall={:.4}",
            class_name(m.class),
            m.sites,
            m.precision,
            m.recall
        );
    }

    eprintln!("\nper-site outcomes:");
    for p in CORPUS {
        for o in outcomes_for(p) {
            let scip = o
                .scip_target
                .as_ref()
                .map(|u| u.as_str().to_string())
                .unwrap_or_else(|| "<uncovered>".to_string());
            let heur: Vec<&str> = o.heuristic_targets.iter().map(|u| u.as_str()).collect();
            eprintln!(
                "  [{}] {:<16} scip={}  heuristic={:?}",
                p.name,
                class_name(o.class),
                scip,
                heur
            );
        }
    }
}

// ── Overload alignment: SCIP signature line ↔ extractor impl line ────────────
//
// The two `NS.parse(...)` sites in `dynamic/users.ts` are overload calls. SCIP
// resolves both to the single `parse` IMPLEMENTATION in `lib.ts`, but it points
// its definition at the first overload SIGNATURE line (`lib.ts:18`) while the
// Tree-sitter extractor records only the implementation (`lib.ts:20`). The
// SCIP↔extractor `(file, line, name)` merge key therefore mis-aligned and both
// sites were dropped as uncomparable. The overload-tolerant fallback in
// `scip_merge` must make them adjudicate to the impl node.

#[test]
fn dynamic_overload_sites_resolve_to_impl() {
    let dynamic = CORPUS
        .iter()
        .find(|p| p.name == "dynamic")
        .expect("dynamic project is in the corpus");
    let outcomes = outcomes_for(dynamic);

    // The node every `NS.parse(...)` call must resolve to: the implementation in
    // lib.ts (uid keyed on the simple fqn `parse`, as the extractor records it).
    let parse_impl = Uid::new("ts", dynamic.repo, "src/lib.ts", "parse", "");
    let resolved_to_parse = outcomes
        .iter()
        .filter(|o| o.scip_target.as_ref() == Some(&parse_impl))
        .count();

    assert_eq!(
        resolved_to_parse, 2,
        "both NS.parse overload sites must adjudicate to lib.ts's parse impl, got {resolved_to_parse}. \
         Per-site (class, scip_target):\n{:#?}",
        outcomes
            .iter()
            .map(|o| (
                class_name(o.class),
                o.scip_target.as_ref().map(|u| u.as_str().to_string())
            ))
            .collect::<Vec<_>>()
    );
}

// ── The CI gate: documented floors (spec §8.1) ───────────────────────────────
//
// These floors are set at the measured values (the corpus is deterministic, so
// "≥ measured" is the right guard). A regression — the heuristic getting worse,
// or SCIP coverage dropping — pushes a number below its floor and fails the
// build. The floors are intentionally exact-at-measured because the corpus is
// committed and hermetic; as the corpus grows the floors are re-derived from the
// report.

/// Overall recall floor. Measured 0.62 (32/52) after the overload-alignment fix
/// made the two `NS.parse` namespace→free-function sites adjudicable: they are
/// recall MISSES (the method-only rule cannot see a free function), so honestly
/// counting them lowers recall from the prior 0.64 (32/50). Floor sits just
/// below the measured 0.6154 at 0.61 (a real floor, not EPS-reliant).
const FLOOR_OVERALL_RECALL: f64 = 0.61;
/// Overall precision floor (measured 0.68).
const FLOOR_OVERALL_PRECISION: f64 = 0.65;

/// Per-class precision floors, only for the classes with enough sites to gate
/// meaningfully (≥ 5). Under-populated classes are reported but not gated.
/// Re-derived over the expanded corpus (2026-06-12, Slice 13).
fn class_precision_floor(class: HeuristicClass) -> Option<f64> {
    match class {
        HeuristicClass::BareSingle => Some(1.00), // 20 sites, measured 1.00
        HeuristicClass::ThisMethod => Some(1.00), // 8 sites (now ≥ 5), measured 1.00
        HeuristicClass::UnknownReceiver => Some(0.53), // 23 sites, measured 0.53
        // < 5 sites: not gated (see the report's small-corpus caveat).
        HeuristicClass::BareMulti => None,
    }
}

/// Expected SCIP coverage over the corpus — guards against a silent collapse in
/// what `scip-typescript` resolves (e.g. a fixture or index regenerated wrong).
/// Measured 52 after the overload-alignment fix made the two `NS.parse` overload
/// sites adjudicable (was 50 in Slice 13).
const MIN_COVERED_SITES: usize = 52;

#[test]
fn corpus_meets_documented_floors() {
    let report = corpus_report();
    eprint!("{}", render_report(&report)); // surfaced on failure

    assert!(
        report.covered_sites >= MIN_COVERED_SITES,
        "SCIP coverage dropped: {} covered sites, floor {MIN_COVERED_SITES}. Report:\n{}",
        report.covered_sites,
        render_report(&report)
    );

    assert!(
        report.overall_recall + EPS >= FLOOR_OVERALL_RECALL,
        "overall recall {:.4} below floor {FLOOR_OVERALL_RECALL}",
        report.overall_recall
    );
    assert!(
        report.overall_precision + EPS >= FLOOR_OVERALL_PRECISION,
        "overall precision {:.4} below floor {FLOOR_OVERALL_PRECISION}",
        report.overall_precision
    );

    for m in &report.per_class {
        if let Some(floor) = class_precision_floor(m.class) {
            assert!(
                m.precision + EPS >= floor,
                "{} precision {:.4} below floor {floor} ({} sites)",
                class_name(m.class),
                m.precision,
                m.sites
            );
        }
    }

    // ── Per-band calibration floors + the §4.1 monotonicity invariant ────────
    eprint!("{}", render_band_table(&report)); // surfaced on failure

    // The per-band floors + monotonicity are gated by the SHARED, language-
    // parametric harness (`support::assert_band_floors`) — the same gate the
    // Python and Rust accuracy gates use. The floors below are the TS numbers,
    // re-derived over the committed corpus (INFERRED measured 1.00, AMBIGUOUS
    // 0.53). A band with < MIN_GATED_SITES adjudicable sites is reported but not
    // gated (small-corpus caveat), and RESOLVED/EXTRACTED carry no heuristic edge
    // so they are never gated.
    assert_band_floors(
        &report,
        &[
            // measured 1.00, margin 0.15 → 0.85 (well above the will-break bar).
            BandFloor {
                band: Band::Inferred,
                floor: 0.85,
            },
            // measured 0.53, margin 0.13 → 0.40.
            BandFloor {
                band: Band::Ambiguous,
                floor: 0.40,
            },
        ],
    );

    // Non-vacuity: both gated bands must carry enough adjudicable sites to mean
    // something (the TS corpus has INFERRED 28, AMBIGUOUS 24).
    assert_band_nonvacuous(&report, &[Band::Inferred, Band::Ambiguous]);
}

// ── Report consistency: the committed doc must match the live metrics ────────
//
// `docs/accuracy/ts-resolution.md` embeds a machine-readable JSON block (between
// the BEGIN/END markers). This test parses that block and asserts it equals the
// freshly-computed report, so the published numbers can never silently drift
// from the code.

const REPORT_PATH: &str = "../../docs/accuracy/ts-resolution.json";

#[test]
fn report_matches_committed_doc() {
    let report = corpus_report();

    // Overall, coverage, and the §4.1 per-band calibration view are checked by
    // the SHARED, language-parametric harness (`support::AccuracyDoc`) — the same
    // consistency check the Python and Rust gates run. The TS doc additionally
    // carries a `per_class` block (the TS-only branch taxonomy), checked inline
    // below.
    let doc_helper = AccuracyDoc::load(REPORT_PATH);
    doc_helper.assert_matches(&report);

    // Per class (TS-specific): the doc's `per_class` block must equal the live
    // per-class metrics, so the published per-branch numbers cannot drift.
    let json_path = Path::new(env!("CARGO_MANIFEST_DIR")).join(REPORT_PATH);
    let raw = std::fs::read_to_string(&json_path).expect("read accuracy JSON");
    let doc: Value = serde_json::from_str(&raw).expect("accuracy JSON parses");
    let doc_classes = doc["per_class"].as_array().expect("per_class array");
    assert_eq!(
        doc_classes.len(),
        report.per_class.len(),
        "per_class count must match"
    );
    for m in &report.per_class {
        let entry = doc_classes
            .iter()
            .find(|e| e["class"].as_str() == Some(class_name(m.class)))
            .unwrap_or_else(|| panic!("doc missing class {}", class_name(m.class)));
        assert_eq!(
            entry["sites"].as_u64().unwrap() as usize,
            m.sites,
            "{} sites must match the doc",
            class_name(m.class)
        );
        assert_close(
            entry["precision"].as_f64().unwrap(),
            m.precision,
            class_name(m.class),
        );
        assert_close(
            entry["recall"].as_f64().unwrap(),
            m.recall,
            class_name(m.class),
        );
    }
}

/// Assert two 2-dp metric numbers agree within `EPS` (re-exported from the shared
/// harness so the TS per-class check uses the same tolerance).
fn assert_close(doc: f64, live: f64, what: &str) {
    support::assert_close(doc, live, what);
}
