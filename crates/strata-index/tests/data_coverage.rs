//! Data-plane link-coverage consistency + floor gate (Slice 16, D3).
//!
//! Indexes the committed `crossrepo_data` fixture estate end-to-end through
//! `index_estate` (the SAME path `strata index --workspace` uses), aggregates each
//! repo's `DataLinkCoverage`, and pins it equal to the numbers published in
//! `docs/accuracy/data-linking.md`. This is the data-plane analogue of
//! `infra_coverage.rs`: the report cannot silently drift from the code, and a
//! regression that dropped a real FK link (inflating `fks_unresolved`) fails the
//! build. It also proves the indexer activates the data plane on `.sql` (repo-a)
//! and is silent when absent (repo-b contributes zero).

use std::path::{Path, PathBuf};

use strata_index::{index_estate, DataLinkCoverage, ResolveMode, WorkspaceManifest};

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
        let ty = entry.file_type()?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_all(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

fn add(a: DataLinkCoverage, b: DataLinkCoverage) -> DataLinkCoverage {
    DataLinkCoverage {
        schemas_detected: a.schemas_detected + b.schemas_detected,
        schemas_failed: a.schemas_failed + b.schemas_failed,
        statements_skipped: a.statements_skipped + b.statements_skipped,
        tables_total: a.tables_total + b.tables_total,
        columns_total: a.columns_total + b.columns_total,
        fks_total: a.fks_total + b.fks_total,
        fks_linked: a.fks_linked + b.fks_linked,
        fks_unresolved: a.fks_unresolved + b.fks_unresolved,
        reads_linked: a.reads_linked + b.reads_linked,
        reads_unresolved: a.reads_unresolved + b.reads_unresolved,
        writes_linked: a.writes_linked + b.writes_linked,
        writes_unresolved: a.writes_unresolved + b.writes_unresolved,
        orm_models_total: a.orm_models_total + b.orm_models_total,
        orm_models_linked: a.orm_models_linked + b.orm_models_linked,
        orm_models_unresolved: a.orm_models_unresolved + b.orm_models_unresolved,
    }
}

fn coverage_of(name: &str) -> DataLinkCoverage {
    let tmp = tempfile::tempdir().expect("tempdir");
    copy_dir_all(&fixture_dir(name), tmp.path()).expect("copy fixture");
    let manifest_path = tmp.path().join("strata.workspace.toml");
    let manifest = WorkspaceManifest::parse_file(&manifest_path).expect("manifest parses");
    let stats = index_estate(&manifest, &manifest_path, ResolveMode::Off);
    stats
        .repos
        .iter()
        .filter_map(|r| r.stats.as_ref())
        .fold(DataLinkCoverage::default(), |acc, s| add(acc, s.data_link))
}

/// The numbers published in `docs/accuracy/data-linking.md` for the
/// `crossrepo_data` fixture estate. The consistency test pins these equal to the
/// live computation; the doc tabulates the same values.
const DOC_COVERAGE: DataLinkCoverage = DataLinkCoverage {
    // repo-a: one `.sql` schema; repo-b: none.
    schemas_detected: 1,
    // The one schema parses cleanly — no failures.
    schemas_failed: 0,
    // The clean schema has no PL/pgSQL `DO`/`CREATE FUNCTION` bodies, so the
    // per-statement splitter skips nothing (the robustness signal is exercised by
    // the strata-data unit tests, not this estate's headline numbers).
    statements_skipped: 0,
    // orgs, users, memberships — 3 tables.
    tables_total: 3,
    // orgs(2) + users(4, after the ALTER ADD) + memberships(3) = 9 columns.
    columns_total: 9,
    // users.org_id→orgs.id, memberships.user_id→users.id, memberships.org_id→orgs.id.
    fks_total: 3,
    // All three FK targets are declared in the same schema → 3 linked.
    fks_linked: 3,
    // None unresolved (every FK target exists).
    fks_unresolved: 0,
    // repo-a code: users.ts `getUserEmail` reads users (1) + `listUsersWithOrg`
    // reads users & orgs via a JOIN (2) = 3 Reads, all to declared tables.
    reads_linked: 3,
    // No code reads an undeclared table.
    reads_unresolved: 0,
    // repo-a code: writer.py `touch_last_login` writes users (UPDATE) +
    // `add_membership` writes memberships (INSERT) = 2 Writes. The f-string
    // `delete_by_table` is dynamic SQL → NOT captured (honest absence, no edge).
    writes_linked: 2,
    // No code writes an undeclared table.
    writes_unresolved: 0,
    // repo-a ORM models (M2b): models.py `User` (SQLAlchemy `__tablename__`) → users,
    // and src/org.entity.ts `Org` (TypeORM `@Entity`) → orgs. Both name declared
    // tables, so 2 total, 2 linked, 0 unresolved.
    orm_models_total: 2,
    orm_models_linked: 2,
    // No model names an undeclared table (every explicit name is declared).
    orm_models_unresolved: 0,
};

#[test]
fn data_report_matches_committed_numbers() {
    let live = coverage_of("crossrepo_data");
    assert_eq!(
        live, DOC_COVERAGE,
        "live data link coverage must equal the numbers in \
         docs/accuracy/data-linking.md (update the doc + this constant together if \
         the fixture changes)"
    );
}

#[test]
fn data_coverage_meets_documented_floors() {
    let cov = coverage_of("crossrepo_data");
    assert!(
        cov.schemas_detected >= 1,
        "schema-detection floor: expected ≥ 1, got {}",
        cov.schemas_detected
    );
    assert!(
        cov.tables_total >= 3,
        "table floor: expected ≥ 3, got {}",
        cov.tables_total
    );
    assert!(
        cov.fks_linked >= 3,
        "FK-link floor: expected ≥ 3, got {}",
        cov.fks_linked
    );
    assert!(
        cov.reads_linked >= 3,
        "code→table read floor: expected ≥ 3, got {}",
        cov.reads_linked
    );
    assert!(
        cov.writes_linked >= 2,
        "code→table write floor: expected ≥ 2, got {}",
        cov.writes_linked
    );
    assert!(
        cov.orm_models_linked >= 2,
        "ORM model→table link floor: expected ≥ 2, got {}",
        cov.orm_models_linked
    );
    // Honesty pins: every FK and every code-referenced table in this estate
    // resolves, so the unresolved counters are exactly 0 — a regression that
    // silently dropped a real link (inflating one) fails here.
    assert_eq!(
        cov.fks_unresolved, 0,
        "no FK should be unresolved in this estate, got {}",
        cov.fks_unresolved
    );
    assert_eq!(
        cov.reads_unresolved, 0,
        "no code read should be unresolved in this estate, got {}",
        cov.reads_unresolved
    );
    assert_eq!(
        cov.writes_unresolved, 0,
        "no code write should be unresolved in this estate, got {}",
        cov.writes_unresolved
    );
    assert_eq!(
        cov.orm_models_unresolved, 0,
        "no ORM model should be unresolved in this clean estate, got {}",
        cov.orm_models_unresolved
    );
    // schemas_failed is the malformed-schema honesty counter; this estate is clean.
    assert_eq!(
        cov.schemas_failed, 0,
        "no schema should fail to parse in this estate, got {}",
        cov.schemas_failed
    );
    // statements_skipped is the per-statement robustness signal; this fixture has no
    // PL/pgSQL `DO`/`CREATE FUNCTION` bodies, so nothing is skipped. A regression
    // that started dropping statements from this clean estate fails here. (On a real
    // repo a non-zero count is an honest informational signal, not a failure.)
    assert_eq!(
        cov.statements_skipped, 0,
        "no statement should be skipped in this clean estate, got {}",
        cov.statements_skipped
    );
}

#[test]
#[ignore = "run with --ignored --nocapture to print the live data coverage"]
fn print_data_coverage() {
    let cov = coverage_of("crossrepo_data");
    println!("crossrepo_data coverage = {cov:#?}");
}
