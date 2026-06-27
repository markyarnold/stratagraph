//! Infra link-coverage report tests (Slice 5, M2 — Definition of Done test 8).
//! `index_estate` indexes each repo and records a per-repo
//! [`InfraLinkCoverage`](strata_index::InfraLinkCoverage) in its `IndexStats`; the
//! committed `docs/accuracy/infra-linking.md` publishes the estate-aggregated
//! numbers. Two tests keep the report honest, the same discipline as the OpenAPI
//! and GraphQL reports:
//! - `infra_report_matches_committed_numbers`: live == committed (no drift).
//! - `infra_coverage_meets_documented_floors`: the CI gate.

use std::path::{Path, PathBuf};

use strata_index::{index_estate, InfraLinkCoverage, ResolveMode, WorkspaceManifest};

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

/// Sum two coverages field-wise (the estate aggregate over its repos).
fn add(a: InfraLinkCoverage, b: InfraLinkCoverage) -> InfraLinkCoverage {
    InfraLinkCoverage {
        templates_detected: a.templates_detected + b.templates_detected,
        templates_failed: a.templates_failed + b.templates_failed,
        resources_total: a.resources_total + b.resources_total,
        resolvers_total: a.resolvers_total + b.resolvers_total,
        resolvers_linked: a.resolvers_linked + b.resolvers_linked,
        resolvers_unlinked: a.resolvers_unlinked + b.resolvers_unlinked,
        lambdas_runs_linked: a.lambdas_runs_linked + b.lambdas_runs_linked,
        lambdas_handler_unresolved: a.lambdas_handler_unresolved + b.lambdas_handler_unresolved,
        iam_policy_grants_unattributed: a.iam_policy_grants_unattributed
            + b.iam_policy_grants_unattributed,
    }
}

/// Index the named fixture estate and return the estate-aggregated infra coverage
/// (the field-wise sum of every repo's per-repo `InfraLinkCoverage`).
fn coverage_of(name: &str) -> InfraLinkCoverage {
    let tmp = tempfile::tempdir().expect("tempdir");
    copy_dir_all(&fixture_dir(name), tmp.path()).expect("copy fixture");
    let manifest_path = tmp.path().join("strata.workspace.toml");
    let manifest = WorkspaceManifest::parse_file(&manifest_path).expect("manifest parses");
    let stats = index_estate(&manifest, &manifest_path, ResolveMode::Off);
    stats
        .repos
        .iter()
        .filter_map(|r| r.stats.as_ref())
        .fold(InfraLinkCoverage::default(), |acc, s| {
            add(acc, s.infra_link)
        })
}

/// The numbers published in `docs/accuracy/infra-linking.md` for the
/// `crossrepo_infra` fixture estate. The consistency test pins these equal to the
/// live computation; the doc tabulates the same values.
const DOC_COVERAGE: InfraLinkCoverage = InfraLinkCoverage {
    // repo-a: one template; repo-b: none.
    templates_detected: 1,
    // Both templates in this estate parse cleanly — no failures.
    templates_failed: 0,
    // repo-a's template: UserFunction, UserRole, Api, UserDS, GetUserResolver,
    // CreateUserResolver — 6 resources.
    resources_total: 6,
    // Two root resolvers (Query.getUser, Mutation.createUser).
    resolvers_total: 2,
    // Both name a field repo-a's schema declares → 2 linked.
    resolvers_linked: 2,
    // None unlinked (no ghost field in this estate).
    resolvers_unlinked: 0,
    // UserFunction's `user.handler` resolves to src/handlers/user.ts → 1 Runs link.
    lambdas_runs_linked: 1,
    // No unresolved handlers in this estate (the one Lambda resolves).
    lambdas_handler_unresolved: 0,
    // No standalone policy with an unresolved role target in this estate.
    iam_policy_grants_unattributed: 0,
};

// ── Test 8a: the live coverage matches the committed report. ─────────────────

#[test]
fn infra_report_matches_committed_numbers() {
    let live = coverage_of("crossrepo_infra");
    assert_eq!(
        live, DOC_COVERAGE,
        "live infra link coverage must equal the numbers in \
         docs/accuracy/infra-linking.md (update the doc + this constant together \
         if the fixture changes)"
    );
}

// ── Test 8b: the CI floor gate. ──────────────────────────────────────────────

#[test]
fn infra_coverage_meets_documented_floors() {
    let cov = coverage_of("crossrepo_infra");
    assert!(
        cov.templates_detected >= 1,
        "template-detection floor: expected ≥ 1, got {}",
        cov.templates_detected
    );
    assert!(
        cov.resolvers_linked >= 2,
        "resolver-link floor: expected ≥ 2 (getUser + createUser), got {}",
        cov.resolvers_linked
    );
    assert!(
        cov.lambdas_runs_linked >= 1,
        "Runs-link floor: expected ≥ 1 (UserFunction → its handler module), got {}",
        cov.lambdas_runs_linked
    );
    // Honesty pin (two-sided): this estate has no ghost field and its one Lambda
    // resolves, so both honesty counters are exactly 0 — a regression that
    // silently dropped a real link (inflating these) fails here.
    assert_eq!(
        cov.resolvers_unlinked, 0,
        "no resolver should be unlinked in this estate, got {}",
        cov.resolvers_unlinked
    );
    assert_eq!(
        cov.lambdas_handler_unresolved, 0,
        "no handler should be unresolved in this estate, got {}",
        cov.lambdas_handler_unresolved
    );
}

// ── Helper to (re)derive the published numbers when the fixture changes. ─────

#[test]
#[ignore = "run with --ignored --nocapture to print the live infra coverage"]
fn print_infra_coverage() {
    let cov = coverage_of("crossrepo_infra");
    println!("crossrepo_infra coverage = {cov:#?}");
}
