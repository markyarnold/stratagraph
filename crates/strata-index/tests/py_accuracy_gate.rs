//! The committed-corpus **Python** resolution-accuracy gate (Track C1), the
//! Python analogue of `accuracy_gate.rs` (TS).
//!
//! Hermetic: each corpus project ships its sources + a committed `index.scip`
//! produced by `scip-python` (no Node at test time). The harness assembles the
//! Python plane with `assemble_python`, runs the **language-parametric**
//! `resolve_differential_graph` over it against the committed SCIP ground truth,
//! and asserts the documented per-band floors. The same computed report is
//! checked against `docs/accuracy/py-resolution.json` so the published numbers
//! cannot silently drift. The differential core (`accuracy_report` / `by_band`)
//! and the per-band floor/consistency harness are shared UNCHANGED with the TS
//! gate (see `support/accuracy.rs`).
//!
//! Regenerate the committed indexes (needs Node, network) with:
//! ```text
//! STRATA_SCIP_LIVE=1 cargo test -p strata-index --test gen_scip -- --ignored generate_py
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use strata_core::{AnalyzedFile, Graph};
use strata_index::{
    accuracy_report, assemble_python, resolve_differential_graph, AccuracyReport, Band,
};
use strata_lang_py::analyze;
use strata_scip::ScipResolver;

#[path = "support/accuracy.rs"]
mod support;
use support::{assert_band_floors, assert_band_nonvacuous, AccuracyDoc, BandFloor};

/// One Python corpus project: its directory under `py-corpus/` and the repo name
/// `scip-python` assigns it (the `--project-name`, also the package component of
/// every `py`-tagged uid).
struct PyProject {
    dir: &'static str,
    repo: &'static str,
}

/// The committed Python resolution corpus. Three self-contained packages, each
/// scip-python-indexable on bare sources (no venv/deps), together exercising the
/// bands non-vacuously:
///   * `shop` — import-matched cross-module calls (Inferred), same-module calls
///     (Extracted), typed-receiver `.total()`/`.tax()` fan-outs SCIP narrows
///     (Ambiguous), and a `getattr(...)()` dynamic call never guessed.
///   * `geometry` — `self.area()`/`self.resize()` own-class methods (Inferred),
///     constructor calls SCIP resolves but the heuristic cannot (recall misses),
///     typed- and untyped-receiver `.area()`/`.scale()` fan-outs (Ambiguous, the
///     untyped ones an honest SCIP gap), and a dynamic getattr call.
///   * `pipeline` — same-module stage calls (Extracted), a unique-repo-wide bare
///     name (Inferred), an unknown bare name resolving to nothing (unresolved),
///     and `str.split()` on a builtin (unadjudicable).
const PY_CORPUS: &[PyProject] = &[
    PyProject {
        dir: "shop",
        repo: "shop",
    },
    PyProject {
        dir: "geometry",
        repo: "geometry",
    },
    PyProject {
        dir: "pipeline",
        repo: "pipeline",
    },
];

fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("accuracy")
        .join("py-corpus")
}

/// Load every `.py` under a project, keyed by path **relative to the project
/// root** (e.g. `shop/models.py`) — the same key `scip-python` uses for
/// `relative_path`, so a SCIP position lookup aligns. `__pycache__` is excluded
/// (it is never committed; this is belt-and-braces).
fn load_sources(proj: &PyProject) -> BTreeMap<String, String> {
    let base = corpus_root().join(proj.dir);
    let mut out = BTreeMap::new();
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
        for entry in std::fs::read_dir(dir).expect("read corpus dir") {
            let p = entry.expect("entry").path();
            if p.is_dir() {
                if p.file_name().and_then(|n| n.to_str()) == Some("__pycache__") {
                    continue;
                }
                walk(base, &p, out);
            } else if p.extension().and_then(|e| e.to_str()) == Some("py") {
                let rel = p
                    .strip_prefix(base)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.insert(rel, std::fs::read_to_string(&p).expect("read source"));
            }
        }
    }
    walk(&base, &base, &mut out);
    out
}

/// Per-site outcomes for one Python project: analyze, assemble the `py` plane,
/// then run the generic graph-based differential against the committed SCIP.
fn outcomes_for(proj: &PyProject) -> Vec<strata_index::SiteOutcome> {
    let sources = load_sources(proj);
    let analyzed: BTreeMap<String, AnalyzedFile> = sources
        .iter()
        .map(|(path, text)| (path.clone(), analyze(path, text)))
        .collect();
    let mut graph = Graph::new();
    assemble_python(&mut graph, proj.repo, &analyzed);
    let bytes = std::fs::read(corpus_root().join(proj.dir).join("index.scip"))
        .expect("committed index.scip");
    let scip = ScipResolver::from_bytes(&bytes).expect("index.scip parses");
    resolve_differential_graph(&analyzed, &sources, &graph, proj.repo, "py", &scip)
}

/// The accuracy report over the WHOLE committed Python corpus.
fn corpus_report() -> AccuracyReport {
    let mut all = Vec::new();
    for proj in PY_CORPUS {
        all.extend(outcomes_for(proj));
    }
    accuracy_report(&all)
}

const REPORT_PATH: &str = "../../docs/accuracy/py-resolution.json";

// ── Inspection: print the report (run with --nocapture) ──────────────────────

#[test]
fn print_py_corpus_report() {
    let report = corpus_report();
    eprintln!("\n=== Python resolution accuracy over the committed corpus ===\n");
    eprint!("{}", support::render_report(&report));
    match report.check_band_monotonicity() {
        Ok(()) => eprintln!("monotonicity: OK (EXTRACTED ≥ INFERRED ≥ AMBIGUOUS)"),
        Err(e) => eprintln!("monotonicity: VIOLATED — {e}"),
    }
}

// ── Consistency: the committed doc must match the live metrics ────────────────

#[test]
fn report_matches_committed_doc() {
    let report = corpus_report();
    AccuracyDoc::load(REPORT_PATH).assert_matches(&report);
}

// ── The CI gate: documented per-band floors + non-vacuity ─────────────────────
//
// Measured 2026-06-14 over the committed corpus (scip-python 0.6.6):
//   EXTRACTED  9 adjudicable sites, precision 1.00 (same-module calls)
//   INFERRED  15 adjudicable sites, precision 1.00 (import-matched / self / unique)
//   AMBIGUOUS 10 adjudicable sites, precision 0.56 (typed-receiver fan-outs)
// Floors sit at the MEASURED precision minus a documented honesty margin (the
// corpus is modest, so the margin absorbs a single-site swing without masking a
// real regression). Unlike the TS heuristic, the Python linker DOES emit
// Extracted-band edges (a same-module bare call to a local `def` is a
// deterministic fact), so EXTRACTED is a gated, populated band here.

#[test]
fn corpus_meets_documented_floors() {
    let report = corpus_report();
    eprint!("{}", support::render_report(&report)); // surfaced on failure

    assert_band_floors(
        &report,
        &[
            // measured 1.00, margin 0.05 → 0.95 (a same-module fact must stay a fact).
            BandFloor {
                band: Band::Extracted,
                floor: 0.95,
            },
            // measured 1.00, margin 0.15 → 0.85.
            BandFloor {
                band: Band::Inferred,
                floor: 0.85,
            },
            // measured 0.56, margin 0.16 → 0.40 (the will-break bar; an
            // over-included fan-out must never claim more).
            BandFloor {
                band: Band::Ambiguous,
                floor: 0.40,
            },
        ],
    );

    // All three gated bands must carry enough adjudicable sites to mean something.
    assert_band_nonvacuous(&report, &[Band::Extracted, Band::Inferred, Band::Ambiguous]);
}
