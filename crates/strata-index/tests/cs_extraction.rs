//! C# extraction/linking accuracy tests (Slice 11). The committed corpus
//! `tests/fixtures/accuracy/cs/` is analyzed and linked with the C# plane; the
//! resulting [`CsLinkCoverage`] is pinned to the numbers published in
//! `docs/accuracy/cs-extraction.md`, the same honesty discipline as the TS,
//! Python, OpenAPI, GraphQL, and infra reports:
//! - `cs_coverage_matches_committed_numbers`: live == committed (no drift).
//! - `cs_coverage_meets_documented_floors`: the CI gate.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use strata_core::{AnalyzedFile, Graph};
use strata_index::{assemble_csharp, CsLinkCoverage};
use strata_lang_cs::analyze as analyze_cs;

const REPO: &str = "cs-accuracy";

fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("accuracy")
        .join("cs")
}

/// Recursively collect every `.cs` file under `dir` into a `repo-relative path →
/// source` map (deterministic order via `BTreeMap`).
fn collect_cs(dir: &Path) -> BTreeMap<String, String> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
        for entry in std::fs::read_dir(dir).expect("read corpus dir") {
            let entry = entry.expect("dir entry");
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, out);
            } else if path.extension().and_then(|e| e.to_str()) == Some("cs") {
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

/// Build the C# plane over the corpus and return its [`CsLinkCoverage`].
fn corpus_coverage() -> CsLinkCoverage {
    let sources = collect_cs(&corpus_dir());
    let analyzed: BTreeMap<String, AnalyzedFile> = sources
        .iter()
        .map(|(p, s)| (p.clone(), analyze_cs(p, s)))
        .collect();
    let mut g = Graph::new();
    assemble_csharp(&mut g, REPO, &analyzed)
}

/// The numbers published in `docs/accuracy/cs-extraction.md`. The consistency
/// test pins these equal to the live computation; the doc tabulates the same
/// values. Update the doc and this constant together if the corpus changes.
const DOC_COVERAGE: CsLinkCoverage = CsLinkCoverage {
    calls_total: 7,
    calls_same_file: 1,
    calls_inferred: 2,
    calls_ambiguous: 1,
    calls_unresolved: 3,
};

#[test]
fn cs_coverage_matches_committed_numbers() {
    let live = corpus_coverage();
    assert_eq!(
        live, DOC_COVERAGE,
        "live C# link coverage must equal the numbers in \
         docs/accuracy/cs-extraction.md (update the doc + this constant together \
         if the corpus changes)"
    );
}

#[test]
fn cs_coverage_meets_documented_floors() {
    let cov = corpus_coverage();
    // The corpus is deterministic, so the floors sit at the measured values.
    assert!(
        cov.calls_total >= 7,
        "call-site floor: expected ≥ 7, got {}",
        cov.calls_total
    );
    assert!(
        cov.calls_same_file >= 1,
        "same-file Extracted floor: expected ≥ 1, got {}",
        cov.calls_same_file
    );
    assert!(
        cov.calls_inferred >= 2,
        "Inferred floor (this-method + unique cross-file): expected ≥ 2, got {}",
        cov.calls_inferred
    );
    assert!(
        cov.calls_ambiguous >= 1,
        "Ambiguous floor (unknown-receiver fan-out): expected ≥ 1, got {}",
        cov.calls_ambiguous
    );
    // Honesty pin: the reflection misses + the unknown bare name are surfaced,
    // never inflated away. A regression that invented a confident edge for the
    // `mi.Invoke(...)` reflective call would DROP `calls_unresolved` and fail here.
    assert!(
        cov.calls_unresolved >= 3,
        "unresolved floor (Ghost name + GetMethod + Invoke reflection): expected ≥ 3, got {}",
        cov.calls_unresolved
    );
}

#[test]
#[ignore = "run with --ignored --nocapture to print the live C# coverage"]
fn print_cs_coverage() {
    println!("cs accuracy corpus coverage = {:#?}", corpus_coverage());
}
