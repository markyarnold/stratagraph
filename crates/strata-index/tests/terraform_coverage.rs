//! Terraform/Terragrunt link-coverage report tests (Track D1, Slice 14, M2).
//!
//! `index_repo` over the committed `terraform_appsync` fixture records both an
//! [`InfraLinkCoverage`](strata_index::InfraLinkCoverage) (the TF `.tf` resources
//! flow through the SAME infra plane as CFN) and a
//! [`TerragruntCoverage`](strata_index::TerragruntCoverage); the committed
//! `docs/accuracy/terraform-linking.md` publishes these numbers. Two tests keep
//! the report honest, the same discipline as `infra-linking.md`:
//! - `terraform_report_matches_committed_numbers`: live == committed (no drift).
//! - `terraform_coverage_meets_documented_floors`: the CI gate.

use std::path::{Path, PathBuf};

use strata_index::{index_repo, InfraLinkCoverage, TerragruntCoverage};
use strata_store::DuckGraphStore;

fn fixture_dir(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

/// Index the committed `terraform_appsync` fixture (hermetic — no Node/SCIP, no
/// network) and return its infra + Terragrunt coverage.
fn coverage() -> (InfraLinkCoverage, TerragruntCoverage) {
    let mut store = DuckGraphStore::open_in_memory().expect("store");
    let stats = index_repo(&fixture_dir("terraform_appsync"), &mut store).expect("index");
    (stats.infra_link, stats.terragrunt)
}

/// The infra numbers published in `docs/accuracy/terraform-linking.md`.
const DOC_INFRA: InfraLinkCoverage = InfraLinkCoverage {
    templates_detected: 1,
    templates_failed: 0,
    resources_total: 9,
    resolvers_total: 3,
    resolvers_linked: 2,
    resolvers_unlinked: 1,
    lambdas_runs_linked: 0,
    lambdas_handler_unresolved: 2,
    // No standalone policy with an unresolved role target in this fixture.
    iam_policy_grants_unattributed: 0,
};

/// The Terragrunt numbers published in `docs/accuracy/terraform-linking.md`.
const DOC_TERRAGRUNT: TerragruntCoverage = TerragruntCoverage {
    units_detected: 2,
    deps_total: 1,
    deps_linked: 1,
    deps_unresolved: 0,
};

#[test]
fn terraform_report_matches_committed_numbers() {
    let (infra, tg) = coverage();
    assert_eq!(
        infra, DOC_INFRA,
        "live infra coverage must equal docs/accuracy/terraform-linking.md \
         (update the doc + this constant together if the fixture changes)"
    );
    assert_eq!(
        tg, DOC_TERRAGRUNT,
        "live Terragrunt coverage must equal docs/accuracy/terraform-linking.md"
    );
}

#[test]
fn terraform_coverage_meets_documented_floors() {
    let (infra, tg) = coverage();
    // Detection + linking floors (TF `.tf` resources flow through the infra plane).
    assert!(
        infra.templates_detected >= 1,
        "TF config-detection floor: expected ≥ 1, got {}",
        infra.templates_detected
    );
    assert!(
        infra.resources_total >= 6,
        "TF resource floor: expected ≥ 6, got {}",
        infra.resources_total
    );
    assert!(
        infra.resolvers_linked >= 2,
        "resolver-link floor: expected ≥ 2 (getUser + createUser), got {}",
        infra.resolvers_linked
    );
    // Honesty pins (two-sided): exactly one ghost resolver stays unlinked, and the
    // zip-packaged Lambdas stay handler-unresolved (Runs deferred). A regression
    // that silently invented either link fails here.
    assert_eq!(
        infra.resolvers_unlinked, 1,
        "exactly one unlinked resolver (the ghost field), got {}",
        infra.resolvers_unlinked
    );
    assert_eq!(
        infra.lambdas_runs_linked, 0,
        "no TF Runs links (zip-packaged Lambdas), got {}",
        infra.lambdas_runs_linked
    );
    // Terragrunt structural floors.
    assert!(
        tg.units_detected >= 2,
        "Terragrunt unit floor: expected ≥ 2, got {}",
        tg.units_detected
    );
    assert_eq!(
        tg.deps_unresolved, 0,
        "no Terragrunt dependency should be unresolved in this fixture, got {}",
        tg.deps_unresolved
    );
}

/// Print the live coverage for (re)deriving the committed numbers when the fixture
/// changes. Run with `--ignored --nocapture`.
#[test]
#[ignore = "run with --ignored --nocapture to print the live terraform coverage"]
fn print_terraform_coverage() {
    let (infra, tg) = coverage();
    println!("terraform_appsync infra = {infra:#?}");
    println!("terraform_appsync terragrunt = {tg:#?}");
}
