//! Link-coverage report tests (Slice 3, M3 — Definition of Done tests 7, 8).
//!
//! `link_estate` returns an [`EstateLinkCoverage`] for the fixture estate; the
//! committed `docs/accuracy/openapi-linking.md` publishes those numbers. Two
//! tests keep the report honest (same discipline as the slice-2 accuracy report):
//! - `report_matches_committed_numbers` asserts the live coverage equals the
//!   numbers tabulated in the committed report (it cannot silently drift).
//! - `coverage_meets_documented_floors` is the CI gate: documented floors fail
//!   the build if a tier regresses.

use std::path::{Path, PathBuf};

use strata_index::{
    index_estate, link_estate, EstateLinkCoverage, ResolveMode, TierCounts, WorkspaceManifest,
};

fn fixture_dir(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == ".strata" {
            continue;
        }
        let src_path = entry.path();
        let dst_path = dst.join(&name);
        if entry.file_type()?.is_dir() {
            copy_dir_all(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

/// Index + link the named fixture estate; return its coverage.
fn coverage_of(name: &str) -> EstateLinkCoverage {
    let tmp = tempfile::tempdir().expect("tempdir");
    copy_dir_all(&fixture_dir(name), tmp.path()).expect("copy fixture");
    let manifest_path = tmp.path().join("strata.workspace.toml");
    let manifest = WorkspaceManifest::parse_file(&manifest_path).expect("manifest parses");
    index_estate(&manifest, &manifest_path, ResolveMode::Off);
    let (_g, coverage, _results) = link_estate(&manifest, tmp.path());
    coverage
}

/// The numbers published in `docs/accuracy/openapi-linking.md` for the
/// `crossrepo` fixture estate. The consistency test below pins these equal to the
/// live computation; the doc tabulates the same values.
const DOC_COVERAGE: EstateLinkCoverage = EstateLinkCoverage {
    // repo-producer: 1 route (GET /users/{id}) → 1 PRODUCES.
    producers_total: 1,
    // repo-consumer: a literal-URL fetch + an operationId-name call → 2 CONSUMES
    // (both Inferred). The route file's `app.get(...)` is NOT a consumer call.
    consumers_total: 2,
    // Unique-key estate: no consumer matches several apis, so no fan-out (B6).
    consumers_ambiguous: 0,
    by_tier: TierCounts {
        extracted: 0,
        inferred: 2,
        ambiguous: 0,
    },
    // repo-consumer's fetch("/widgets/9") matches no operation → 1 unmatched.
    unmatched_consumers: 1,
    // An OpenAPI-only estate has no GraphQL documents (additive fields = 0).
    unparsed_documents: 0,
    unresolved_root_spreads: 0,
};

// ── Test 8a: the live coverage matches the committed report. ─────────────────

#[test]
fn report_matches_committed_numbers() {
    let live = coverage_of("crossrepo");
    assert_eq!(
        live, DOC_COVERAGE,
        "live link coverage must equal the numbers in docs/accuracy/openapi-linking.md \
         (update the doc + this constant together if the fixture changes)"
    );
}

// ── Test 8b: the CI floor gate. ──────────────────────────────────────────────

#[test]
fn coverage_meets_documented_floors() {
    let cov = coverage_of("crossrepo");
    // Floors sit at the measured values (the fixture is deterministic); they are
    // re-derived from the report whenever the fixture changes. A regression
    // (a producer or consumer link lost) fails the build here.
    assert!(
        cov.producers_total >= 1,
        "producer-link floor: expected ≥ 1, got {}",
        cov.producers_total
    );
    assert!(
        cov.consumers_total >= 2,
        "consumer-link floor: expected ≥ 2 cross-repo CONSUMES, got {}",
        cov.consumers_total
    );
    assert!(
        cov.by_tier.inferred >= 2,
        "Inferred consumer-tier floor: expected ≥ 2, got {}",
        cov.by_tier.inferred
    );
    // Honesty floor: the undeclared-endpoint call is reported as unmatched, never
    // invented into an edge.
    assert!(
        cov.unmatched_consumers >= 1,
        "unmatched-consumer floor: the /widgets call must be counted unmatched, got {}",
        cov.unmatched_consumers
    );
}

// ── Helper to (re)derive the published numbers when the fixture changes. ─────

#[test]
#[ignore = "run with --ignored --nocapture to print the live coverage numbers"]
fn print_coverage() {
    let cov = coverage_of("crossrepo");
    println!("crossrepo coverage = {cov:#?}");
}
