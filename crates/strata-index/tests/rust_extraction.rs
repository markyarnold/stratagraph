//! Rust extraction/linking accuracy tests (Slice 21). The committed corpus
//! `tests/fixtures/accuracy/rust/` is analyzed and linked with the Rust plane; the
//! resulting [`RustLinkCoverage`] is pinned to the numbers published in
//! `docs/accuracy/rust-extraction.md`, the same honesty discipline as the TS,
//! Python, C#, OpenAPI, GraphQL, and infra reports:
//! - `rust_coverage_matches_committed_numbers`: live == committed (no drift).
//! - `rust_coverage_meets_documented_floors`: the CI gate.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use strata_core::{AnalyzedFile, Graph};
use strata_index::{assemble_rust, RustLinkCoverage};
use strata_lang_rust::analyze as analyze_rust;

const REPO: &str = "rust-accuracy";

fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("accuracy")
        .join("rust")
}

/// Recursively collect every `.rs` file under `dir` into a `repo-relative path →
/// source` map (deterministic order via `BTreeMap`).
fn collect_rust(dir: &Path) -> BTreeMap<String, String> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
        for entry in std::fs::read_dir(dir).expect("read corpus dir") {
            let entry = entry.expect("dir entry");
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, out);
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
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

/// Build the Rust plane over the corpus and return its [`RustLinkCoverage`].
fn corpus_coverage() -> RustLinkCoverage {
    let sources = collect_rust(&corpus_dir());
    let analyzed: BTreeMap<String, AnalyzedFile> = sources
        .iter()
        .map(|(p, s)| (p.clone(), analyze_rust(p, s)))
        .collect();
    let mut g = Graph::new();
    assemble_rust(&mut g, REPO, &analyzed)
}

/// The numbers published in `docs/accuracy/rust-extraction.md`. The consistency
/// test pins these equal to the live computation; the doc tabulates the same
/// values. Update the doc and this constant together if the corpus changes.
const DOC_COVERAGE: RustLinkCoverage = RustLinkCoverage {
    calls_total: 8,
    calls_same_file: 2,
    calls_inferred: 3,
    calls_ambiguous: 1,
    calls_unresolved: 2,
};

#[test]
fn rust_coverage_matches_committed_numbers() {
    let live = corpus_coverage();
    assert_eq!(
        live, DOC_COVERAGE,
        "live Rust link coverage must equal the numbers in \
         docs/accuracy/rust-extraction.md (update the doc + this constant together \
         if the corpus changes)"
    );
}

#[test]
fn rust_coverage_meets_documented_floors() {
    let cov = corpus_coverage();
    // The corpus is deterministic, so the floors sit at the measured values.
    assert!(
        cov.calls_total >= 8,
        "call-site floor: expected ≥ 8, got {}",
        cov.calls_total
    );
    assert!(
        cov.calls_same_file >= 2,
        "same-file Extracted floor: expected ≥ 2, got {}",
        cov.calls_same_file
    );
    assert!(
        cov.calls_inferred >= 3,
        "Inferred floor (self-method + unique cross-module + type-qualified): expected ≥ 3, got {}",
        cov.calls_inferred
    );
    assert!(
        cov.calls_ambiguous >= 1,
        "Ambiguous floor (unknown-receiver fan-out): expected ≥ 1, got {}",
        cov.calls_ambiguous
    );
    // Honesty pin: the unknown bare name + the no-candidate trait dispatch are
    // surfaced, never inflated away. (Macros never enter the tally at all — they
    // are not calls — which is the strongest form of this honesty.) A regression
    // that invented a confident edge for `ghost()` or `acct.absent()` would DROP
    // `calls_unresolved` and fail here.
    assert!(
        cov.calls_unresolved >= 2,
        "unresolved floor (ghost() + acct.absent() trait dispatch): expected ≥ 2, got {}",
        cov.calls_unresolved
    );
}

#[test]
#[ignore = "run with --ignored --nocapture to print the live Rust coverage"]
fn print_rust_coverage() {
    println!("rust accuracy corpus coverage = {:#?}", corpus_coverage());
}
