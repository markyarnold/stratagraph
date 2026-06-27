//! GraphQL link-coverage report tests (Slice 4, M2 — Definition of Done test
//! 12). `link_estate` returns an [`EstateLinkCoverage`] for the
//! `crossrepo_graphql` fixture estate; the committed
//! `docs/accuracy/graphql-linking.md` publishes those numbers. Two tests keep the
//! report honest (the same discipline as the OpenAPI report):
//! - `graphql_report_matches_committed_numbers`: live == committed (no drift).
//! - `graphql_coverage_meets_documented_floors`: the CI gate.

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

/// The numbers published in `docs/accuracy/graphql-linking.md` for the
/// `crossrepo_graphql` fixture estate. The consistency test below pins these equal
/// to the live computation; the doc tabulates the same values.
const DOC_COVERAGE: EstateLinkCoverage = EstateLinkCoverage {
    // repo-schema: 3 resolver entries (getUser/listUsers/createUser) → 3 PRODUCES.
    producers_total: 3,
    // repo-app: the gql query (Query.getUser) + orders.graphql (Query.listUsers)
    // + the UNTAGGED CREATE_USER constant (Mutation.createUser) → 3 cross-repo
    // CONSUMES, all Extracted. nonExistentField + the interpolated template + the
    // empty doc + the broken UNTAGGED constant produce NO link.
    consumers_total: 3,
    // Unique-key estate: no consumer matches several apis, so no fan-out (B6).
    consumers_ambiguous: 0,
    by_tier: TierCounts {
        // GraphQL unique matches are Extracted (the document names the contract);
        // an untagged constant that parses is evidence-identical to a tagged doc.
        extracted: 3,
        inferred: 0,
        ambiguous: 0,
    },
    // No outgoing HTTP calls in this estate.
    unmatched_consumers: 0,
    // The interpolated `gql` template (broken.ts) + the comment-only empty.graphql
    // are counted unparsed, never guessed into a link. The broken UNTAGGED constant
    // (BROKEN_UNTAGGED in mutations.ts) is NOT counted: it never claimed to be
    // GraphQL, so its parse failure is silently skipped (the honesty rule).
    unparsed_documents: 2,
    // No parsed document in this corpus has a root-level fragment spread (the only
    // spread lives in the interpolated — hence unparsed — composed query).
    unresolved_root_spreads: 0,
};

// ── Test 12a: the live coverage matches the committed report. ────────────────

#[test]
fn graphql_report_matches_committed_numbers() {
    let live = coverage_of("crossrepo_graphql");
    assert_eq!(
        live, DOC_COVERAGE,
        "live GraphQL link coverage must equal the numbers in \
         docs/accuracy/graphql-linking.md (update the doc + this constant together \
         if the fixture changes)"
    );
}

// ── Test 12b: the CI floor gate. ─────────────────────────────────────────────

#[test]
fn graphql_coverage_meets_documented_floors() {
    let cov = coverage_of("crossrepo_graphql");
    assert!(
        cov.producers_total >= 3,
        "GraphQL producer-link floor: expected ≥ 3, got {}",
        cov.producers_total
    );
    assert!(
        cov.consumers_total >= 3,
        "GraphQL consumer-link floor: expected ≥ 3 cross-repo CONSUMES (incl. the \
         untagged CREATE_USER constant), got {}",
        cov.consumers_total
    );
    assert!(
        cov.by_tier.extracted >= 3,
        "Extracted GraphQL consumer-tier floor: expected ≥ 3 (untagged constants \
         that parse link identically to tagged docs), got {}",
        cov.by_tier.extracted
    );
    // Honesty floor (two-sided): the TAGGED interpolated template + empty doc stay
    // counted unparsed (never invented into edges), AND the UNTAGGED broken
    // constant must NOT inflate the count (it never claimed to be GraphQL). The
    // fixture is deterministic, so `unparsed_documents` is pinned at exactly 2.
    assert_eq!(
        cov.unparsed_documents, 2,
        "unparsed-document count must be exactly 2 (tagged broken.ts + empty.graphql); \
         the untagged broken constant must be silently skipped, not counted: got {}",
        cov.unparsed_documents
    );
}

// ── Helper to (re)derive the published numbers when the fixture changes. ─────

#[test]
#[ignore = "run with --ignored --nocapture to print the live GraphQL coverage"]
fn print_graphql_coverage() {
    let cov = coverage_of("crossrepo_graphql");
    println!("crossrepo_graphql coverage = {cov:#?}");
}
