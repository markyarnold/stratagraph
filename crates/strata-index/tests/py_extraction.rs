//! Python extraction/linking accuracy tests (Slice 9). The committed corpus
//! `tests/fixtures/accuracy/py/` is analyzed and linked with the Python plane;
//! the resulting [`PyLinkCoverage`] is pinned to the numbers published in
//! `docs/accuracy/py-extraction.md`, the same honesty discipline as the TS,
//! OpenAPI, GraphQL, and infra reports:
//! - `py_coverage_matches_committed_numbers`: live == committed (no drift).
//! - `py_coverage_meets_documented_floors`: the CI gate.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use strata_core::{AnalyzedFile, Graph};
use strata_index::{assemble_python, PyLinkCoverage};
use strata_lang_py::analyze as analyze_py;

const REPO: &str = "py-accuracy";

fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("accuracy")
        .join("py")
}

/// Recursively collect every `.py` file under `dir` into a `repo-relative path →
/// source` map (deterministic order via `BTreeMap`).
fn collect_py(dir: &Path) -> BTreeMap<String, String> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
        for entry in std::fs::read_dir(dir).expect("read corpus dir") {
            let entry = entry.expect("dir entry");
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, out);
            } else if path.extension().and_then(|e| e.to_str()) == Some("py") {
                let rel = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .replace('\\', "/");
                out.insert(rel, std::fs::read_to_string(&path).expect("read source"));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

/// Build the Python plane over the corpus and return its [`PyLinkCoverage`].
fn corpus_coverage() -> PyLinkCoverage {
    let sources = collect_py(&corpus_dir());
    let analyzed: BTreeMap<String, AnalyzedFile> = sources
        .iter()
        .map(|(p, s)| (p.clone(), analyze_py(p, s)))
        .collect();
    let mut g = Graph::new();
    assemble_python(&mut g, REPO, &analyzed)
}

/// The numbers published in `docs/accuracy/py-extraction.md`. The consistency
/// test pins these equal to the live computation; the doc tabulates the same
/// values. Update the doc and this constant together if the corpus changes.
const DOC_COVERAGE: PyLinkCoverage = PyLinkCoverage {
    calls_total: 6,
    calls_same_module: 1,
    calls_inferred: 2,
    calls_ambiguous: 1,
    calls_unresolved: 2,
};

#[test]
fn py_coverage_matches_committed_numbers() {
    let live = corpus_coverage();
    assert_eq!(
        live, DOC_COVERAGE,
        "live Python link coverage must equal the numbers in \
         docs/accuracy/py-extraction.md (update the doc + this constant together \
         if the corpus changes)"
    );
}

#[test]
fn py_coverage_meets_documented_floors() {
    let cov = corpus_coverage();
    // The corpus is deterministic, so the floors sit at the measured values.
    assert!(
        cov.calls_total >= 6,
        "call-site floor: expected ≥ 6, got {}",
        cov.calls_total
    );
    assert!(
        cov.calls_same_module >= 1,
        "same-module Extracted floor: expected ≥ 1, got {}",
        cov.calls_same_module
    );
    assert!(
        cov.calls_inferred >= 2,
        "Inferred floor (self-method + import-matched): expected ≥ 2, got {}",
        cov.calls_inferred
    );
    // Honesty pin: the ambiguous fan-out and the dynamic/unknown misses are
    // surfaced, never inflated away. A regression that invented a confident edge
    // for the `getattr(...)()` dynamic call would drop `calls_unresolved` here.
    assert!(
        cov.calls_ambiguous >= 1,
        "Ambiguous floor (unknown-receiver fan-out): expected ≥ 1, got {}",
        cov.calls_ambiguous
    );
    assert!(
        cov.calls_unresolved >= 2,
        "unresolved floor (dynamic getattr + unknown name): expected ≥ 2, got {}",
        cov.calls_unresolved
    );
}

#[test]
#[ignore = "run with --ignored --nocapture to print the live Python coverage"]
fn print_py_coverage() {
    println!("py accuracy corpus coverage = {:#?}", corpus_coverage());
}
