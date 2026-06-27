//! The committed-corpus **Rust** resolution-accuracy gate (Track C1), the Rust
//! analogue of `accuracy_gate.rs` (TS) and `py_accuracy_gate.rs` (Python).
//!
//! Hermetic: each corpus project ships its sources + a committed `index.scip`
//! produced by `rust-analyzer scip .` (no rust-analyzer at test time). The
//! harness assembles the Rust plane with `assemble_rust`, runs the
//! **language-parametric** `resolve_differential_graph` over it against the
//! committed SCIP ground truth, and asserts the documented per-band floors. The
//! same computed report is checked against `docs/accuracy/rust-resolution.json`
//! so the published numbers cannot silently drift. The differential core
//! (`accuracy_report` / `by_band`) and the per-band floor/consistency harness are
//! shared UNCHANGED with the TS and Python gates (see `support/accuracy.rs`).
//!
//! Resolving rust-analyzer's impl-method monikers (`impl#[Type]method().`,
//! `impl#[Type][Trait]method().`) onto the `rust`-tagged method nodes relies on
//! the Track C1 moniker shim in `scip_merge::symbol_name_from_moniker`.
//!
//! Regenerate the committed indexes (needs rust-analyzer) with:
//! ```text
//! STRATA_SCIP_LIVE=1 cargo test -p strata-index --test gen_scip -- --ignored generate_rust
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use strata_core::{AnalyzedFile, Graph};
use strata_index::{
    accuracy_report, assemble_rust, resolve_differential_graph, AccuracyReport, Band,
};
use strata_lang_rust::analyze;
use strata_scip::ScipResolver;

#[path = "support/accuracy.rs"]
mod support;
use support::{assert_band_floors, assert_band_nonvacuous, AccuracyDoc, BandFloor};

/// One Rust corpus project: its directory under `rust-corpus/` and the crate name
/// `rust-analyzer` assigns it (the `name` in `Cargo.toml`, also the package
/// component of every `rust`-tagged uid).
struct RustProject {
    dir: &'static str,
    repo: &'static str,
}

/// The committed Rust resolution corpus. Two self-contained cargo crates (each
/// an isolated workspace via an empty `[workspace]`, so `rust-analyzer scip .`
/// gets clean `cargo metadata`), together exercising the bands non-vacuously:
///   * `shapes` — struct+impl methods, a `Shape` trait implemented by two types
///     (`describe` trait-dispatch → Ambiguous fan-out across both impls + the
///     trait sig), `self.area()` own-type methods (Inferred), type-qualified
///     `Type::new()` constructors (Inferred, resolved exactly — slice 23),
///     same-file helper calls (Extracted), and two same-named `area`/`scale`
///     methods called on instance receivers (Ambiguous, rust-analyzer narrows).
///   * `registry` — same-file helpers (Extracted), `self.` methods (Inferred),
///     a cross-module type-qualified `Store::new()` (Inferred), a unique
///     cross-module bare `reduce()` (Inferred), and a `.sum()` fan-out across
///     `Store::sum`/`Tally::sum` (Ambiguous, rust-analyzer narrows).
const RUST_CORPUS: &[RustProject] = &[
    RustProject {
        dir: "shapes",
        repo: "shapes",
    },
    RustProject {
        dir: "registry",
        repo: "registry",
    },
];

fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("accuracy")
        .join("rust-corpus")
}

/// Load every `.rs` under a project's `src/`, keyed by path **relative to the
/// project root** (e.g. `src/models.rs`) — the same key `rust-analyzer` uses for
/// `relative_path`, so a SCIP position lookup aligns. A `target/` build dir (if a
/// stray one exists) is excluded; it is never committed.
fn load_sources(proj: &RustProject) -> BTreeMap<String, String> {
    let base = corpus_root().join(proj.dir);
    let mut out = BTreeMap::new();
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
        for entry in std::fs::read_dir(dir).expect("read corpus dir") {
            let p = entry.expect("entry").path();
            if p.is_dir() {
                if p.file_name().and_then(|n| n.to_str()) == Some("target") {
                    continue;
                }
                walk(base, &p, out);
            } else if p.extension().and_then(|e| e.to_str()) == Some("rs") {
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

/// Per-site outcomes for one Rust project: analyze, assemble the `rust` plane,
/// then run the generic graph-based differential against the committed SCIP.
fn outcomes_for(proj: &RustProject) -> Vec<strata_index::SiteOutcome> {
    let sources = load_sources(proj);
    let analyzed: BTreeMap<String, AnalyzedFile> = sources
        .iter()
        .map(|(path, text)| (path.clone(), analyze(path, text)))
        .collect();
    let mut graph = Graph::new();
    assemble_rust(&mut graph, proj.repo, &analyzed);
    let bytes = std::fs::read(corpus_root().join(proj.dir).join("index.scip"))
        .expect("committed index.scip");
    let scip = ScipResolver::from_bytes(&bytes).expect("index.scip parses");
    resolve_differential_graph(&analyzed, &sources, &graph, proj.repo, "rust", &scip)
}

/// The accuracy report over the WHOLE committed Rust corpus.
fn corpus_report() -> AccuracyReport {
    let mut all = Vec::new();
    for proj in RUST_CORPUS {
        all.extend(outcomes_for(proj));
    }
    accuracy_report(&all)
}

const REPORT_PATH: &str = "../../docs/accuracy/rust-resolution.json";

// ── Inspection: print the report (run with --nocapture) ──────────────────────

#[test]
fn print_rust_corpus_report() {
    let report = corpus_report();
    eprintln!("\n=== Rust resolution accuracy over the committed corpus ===\n");
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
// Measured 2026-06-14 over the committed corpus (rust-analyzer 1.96.0):
//   EXTRACTED 11 adjudicable sites, precision 1.00 (same-file calls)
//   INFERRED  10 adjudicable sites, precision 1.00 (Type::new / self / unique name)
//   AMBIGUOUS  9 adjudicable sites, precision 0.47 (method fan-outs incl trait
//              dispatch over several impls + the trait signature)
// Floors sit at the MEASURED precision minus a documented honesty margin. Like
// Python (and unlike the TS heuristic), the Rust linker emits Extracted-band
// edges for a same-file call, so EXTRACTED is a gated, populated band here.

#[test]
fn corpus_meets_documented_floors() {
    let report = corpus_report();
    eprint!("{}", support::render_report(&report)); // surfaced on failure

    assert_band_floors(
        &report,
        &[
            // measured 1.00, margin 0.05 → 0.95 (a same-file fact must stay a fact).
            BandFloor {
                band: Band::Extracted,
                floor: 0.95,
            },
            // measured 1.00, margin 0.15 → 0.85.
            BandFloor {
                band: Band::Inferred,
                floor: 0.85,
            },
            // measured 0.47, margin 0.07 → 0.40 (the will-break bar; trait-dispatch
            // and same-name fan-outs must never claim more than Ambiguous).
            BandFloor {
                band: Band::Ambiguous,
                floor: 0.40,
            },
        ],
    );

    // All three gated bands must carry enough adjudicable sites to mean something.
    assert_band_nonvacuous(&report, &[Band::Extracted, Band::Inferred, Band::Ambiguous]);
}
