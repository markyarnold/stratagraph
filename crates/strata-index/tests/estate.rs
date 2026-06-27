//! Estate plumbing tests (Slice 3, Milestone 1 — Definition of Done §72–83).
//!
//! All estate integration tests copy the committed fixture estate to a tempdir
//! before indexing so `.strata/` dirs are never created inside the source tree.
//! Fixture: `tests/fixtures/estate/{repo-a,repo-b,strata.workspace.toml}`.
//!
//! Tests 1–3 are pure manifest unit tests (no IO).
//! Tests 4–7 are integration tests (copy fixture to tempdir, index with ResolveMode::Off).
//! Test 8 is a CLI smoke test via the binary.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use strata_core::{query, Uid};
use strata_index::{index_estate, load_estate, EstateError, ResolveMode, WorkspaceManifest};

// ── Fixture helpers ────────────────────────────────────────────────────────────

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("estate")
}

/// Copy the fixture estate directory tree to a fresh tempdir and return it.
fn copy_fixture_to_tempdir() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    copy_dir_all(&fixture_dir(), tmp.path()).expect("copy fixture");
    tmp
}

fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        // Skip .strata/ dirs if any exist in the fixture (shouldn't be committed).
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

// ══ Tests 1–3: pure manifest unit tests ════════════════════════════════════════

/// Test 1: parse a valid 2-repo manifest → expected struct.
#[test]
fn manifest_parse_valid_two_repos() {
    let toml = r#"
[workspace]
name = "my-estate"

[[repos]]
name = "frontend"
path = "../frontend"

[[repos]]
name = "backend"
path = "../backend"
"#;
    let manifest = WorkspaceManifest::parse_str(toml).expect("valid manifest parses ok");
    assert_eq!(manifest.workspace.name, "my-estate");
    assert_eq!(manifest.repos.len(), 2);
    assert_eq!(manifest.repos[0].name, "frontend");
    assert_eq!(manifest.repos[0].path, "../frontend");
    assert_eq!(manifest.repos[1].name, "backend");
    assert_eq!(manifest.repos[1].path, "../backend");
}

/// Test 2a: missing `workspace.name` → `EstateError::Manifest`.
#[test]
fn manifest_empty_workspace_name_is_error() {
    let toml = r#"
[workspace]
name = ""
"#;
    let err = WorkspaceManifest::parse_str(toml).expect_err("empty name must error");
    assert!(
        matches!(err, EstateError::Manifest(_)),
        "expected Manifest error, got: {err:?}"
    );
    assert!(
        err.to_string().contains("manifest"),
        "error message mentions manifest"
    );
}

/// Test 2b: a repo with an empty `name` → `EstateError::Manifest`.
#[test]
fn manifest_empty_repo_name_is_error() {
    let toml = r#"
[workspace]
name = "ok"

[[repos]]
name = ""
path = "some/path"
"#;
    let err = WorkspaceManifest::parse_str(toml).expect_err("empty repo name must error");
    assert!(
        matches!(err, EstateError::Manifest(_)),
        "expected Manifest error, got: {err:?}"
    );
}

/// Test 3: two repos with the same `name` → `EstateError::DuplicateRepo`.
#[test]
fn manifest_duplicate_repo_name_is_error() {
    let toml = r#"
[workspace]
name = "ok"

[[repos]]
name = "svc"
path = "svc-v1"

[[repos]]
name = "svc"
path = "svc-v2"
"#;
    let err = WorkspaceManifest::parse_str(toml).expect_err("duplicate repo names must error");
    assert!(
        matches!(err, EstateError::DuplicateRepo(_)),
        "expected DuplicateRepo error, got: {err:?}"
    );
    assert!(
        err.to_string().contains("svc"),
        "error mentions the duplicate name"
    );
}

// ══ Manifest v2: [[repos.apis]] (api-scoped identity, B6 fix) ════════════════════

/// A v1 manifest (no `[[repos.apis]]`) parses unchanged — `apis` defaults empty.
#[test]
fn manifest_v1_parses_unchanged_with_empty_apis() {
    let toml = r#"
[workspace]
name = "estate"

[[repos]]
name = "frontend"
path = "../frontend"
"#;
    let m = WorkspaceManifest::parse_str(toml).expect("v1 manifest parses");
    assert_eq!(m.repos.len(), 1);
    assert!(
        m.repos[0].apis.is_empty(),
        "a v1 repo entry has no declared apis"
    );
}

/// A v2 manifest with valid `[[repos.apis]]` parses; the declarations are read.
#[test]
fn manifest_v2_parses_declared_apis() {
    let toml = r#"
[workspace]
name = "estate"

[[repos]]
name = "user-svc"
path = "user-svc"

[[repos.apis]]
id = "user"
spec = "openapi.yaml"

[[repos.apis]]
id = "admin"
spec = "admin/openapi.yaml"
"#;
    let m = WorkspaceManifest::parse_str(toml).expect("v2 manifest parses");
    assert_eq!(m.repos[0].apis.len(), 2, "two apis declared on one repo");
    assert_eq!(m.repos[0].apis[0].id, "user");
    assert_eq!(m.repos[0].apis[0].spec, "openapi.yaml");
    assert_eq!(m.repos[0].apis[1].id, "admin");
}

/// A non-slug api id (uppercase / spaces / underscore / empty) → a parse error.
#[test]
fn manifest_v2_rejects_non_slug_api_id() {
    for bad in ["User", "user service", "user_svc", "User-API", ""] {
        let toml = format!(
            r#"
[workspace]
name = "estate"

[[repos]]
name = "svc"
path = "svc"

[[repos.apis]]
id = "{bad}"
spec = "openapi.yaml"
"#
        );
        assert!(
            WorkspaceManifest::parse_str(&toml).is_err(),
            "api id {bad:?} is not slug-safe and must be rejected"
        );
    }
}

/// (Explicit) a bad api id surfaces `EstateError::Manifest`, not a panic.
#[test]
fn manifest_v2_bad_api_id_is_manifest_error() {
    let toml = r#"
[workspace]
name = "estate"

[[repos]]
name = "svc"
path = "svc"

[[repos.apis]]
id = "Billing"
spec = "openapi.yaml"
"#;
    let err = WorkspaceManifest::parse_str(toml).expect_err("uppercase api id must error");
    assert!(
        matches!(err, EstateError::Manifest(_)),
        "expected Manifest error, got: {err:?}"
    );
    assert!(
        err.to_string().contains("slug"),
        "the error explains the slug rule: {err}"
    );
}

/// The same api id declared in two repos is ALLOWED (it is the merge opt-in).
#[test]
fn manifest_v2_same_api_id_across_repos_is_allowed() {
    let toml = r#"
[workspace]
name = "estate"

[[repos]]
name = "repo-x"
path = "repo-x"

[[repos.apis]]
id = "user"
spec = "openapi.yaml"

[[repos]]
name = "repo-y"
path = "repo-y"

[[repos.apis]]
id = "user"
spec = "openapi.yaml"
"#;
    let m = WorkspaceManifest::parse_str(toml)
        .expect("the same api id across repos is the merge opt-in");
    assert_eq!(m.repos[0].apis[0].id, "user");
    assert_eq!(m.repos[1].apis[0].id, "user");
}

// ══ Tests 4–7: estate integration tests (tempdir copy) ═════════════════════════

/// Test 4: `index_estate` on the fixture → both repos ok:true, DBs exist, totals > 0.
#[test]
fn index_estate_indexes_both_repos() {
    let tmp = copy_fixture_to_tempdir();
    let manifest_path = tmp.path().join("strata.workspace.toml");
    let manifest = WorkspaceManifest::parse_file(&manifest_path).expect("manifest parses");

    let stats = index_estate(&manifest, &manifest_path, ResolveMode::Off);

    assert_eq!(stats.estate, "test-estate");
    assert_eq!(stats.repos.len(), 2);

    for result in &stats.repos {
        assert!(
            result.ok,
            "repo {} should index ok, but got error: {:?}",
            result.name, result.error
        );
        assert!(
            result.stats.is_some(),
            "repo {} should have stats",
            result.name
        );
    }

    assert!(
        stats.total_nodes > 0,
        "at least some nodes across both repos"
    );
    assert!(
        stats.total_edges > 0,
        "at least some edges across both repos"
    );

    // The .strata/graph.duckdb files must have been created.
    assert!(
        tmp.path().join("repo-a/.strata/graph.duckdb").exists(),
        "repo-a store must exist after indexing"
    );
    assert!(
        tmp.path().join("repo-b/.strata/graph.duckdb").exists(),
        "repo-b store must exist after indexing"
    );
}

/// Test 5: `load_estate` produces a graph with nodes from BOTH repos;
/// query finds a repo-b symbol; impact of a repo-a symbol finds its repo-a caller.
#[test]
fn load_estate_union_spans_both_repos() {
    let tmp = copy_fixture_to_tempdir();
    let manifest_path = tmp.path().join("strata.workspace.toml");
    let manifest = WorkspaceManifest::parse_file(&manifest_path).expect("manifest parses");

    // First index with ResolveMode::Off (no Node/SCIP needed).
    index_estate(&manifest, &manifest_path, ResolveMode::Off);

    let (estate_graph, results) = load_estate(&manifest, tmp.path());

    // Both repos should load ok.
    assert!(
        results.iter().all(|r| r.ok),
        "all repos should load ok: {:?}",
        results
    );

    // Verify nodes from BOTH repos are present via their repo-qualified UIDs.
    // repo-a exports `greet` — uid: ts::repo-a::src/a.ts::greet
    let greet_uid = Uid::new("ts", "repo-a", "src/a.ts", "greet", "");
    assert!(
        estate_graph.get_node(&greet_uid).is_some(),
        "repo-a greet node must be in the estate graph"
    );

    // repo-b exports `compute` — uid: ts::repo-b::src/b.ts::compute
    let compute_uid = Uid::new("ts", "repo-b", "src/b.ts", "compute", "");
    assert!(
        estate_graph.get_node(&compute_uid).is_some(),
        "repo-b compute node must be in the estate graph"
    );

    // query over the estate graph finds repo-b symbol.
    let hits = query(&estate_graph, "compute");
    assert!(
        !hits.is_empty(),
        "query(\"compute\") over estate graph must find repo-b symbol"
    );
    assert!(
        hits.iter().any(|n| n.name == "compute"),
        "found hits: {:?}",
        hits.iter().map(|n| &n.name).collect::<Vec<_>>()
    );

    // repo-a intra-repo impact: `helper` is called by `greet`.
    let helper_uid = Uid::new("ts", "repo-a", "src/a.ts", "helper", "");
    if estate_graph.get_node(&helper_uid).is_some() {
        // helper exists — verify greet appears in impact.
        let impact_result = strata_core::impact(
            &estate_graph,
            &helper_uid,
            &strata_core::ImpactOptions::default(),
        );
        assert!(
            impact_result.affected.iter().any(|a| a.uid == greet_uid),
            "impact(helper) must include greet (repo-a intra-repo edge), affected: {:?}",
            impact_result
                .affected
                .iter()
                .map(|a| a.uid.as_str())
                .collect::<Vec<_>>()
        );
    }
}

/// Test 6: determinism (R3) — `load_estate` twice → identical graphs.
#[test]
fn load_estate_is_deterministic() {
    let tmp = copy_fixture_to_tempdir();
    let manifest_path = tmp.path().join("strata.workspace.toml");
    let manifest = WorkspaceManifest::parse_file(&manifest_path).expect("manifest parses");

    index_estate(&manifest, &manifest_path, ResolveMode::Off);

    let (g1, _) = load_estate(&manifest, tmp.path());
    let (g2, _) = load_estate(&manifest, tmp.path());

    assert_eq!(
        g1.node_count(),
        g2.node_count(),
        "determinism: node counts must match"
    );
    assert_eq!(
        g1.edge_count(),
        g2.edge_count(),
        "determinism: edge counts must match"
    );

    // Order-independent node UID sets.
    let nodes1: BTreeSet<String> = g1.nodes().map(|n| n.uid.to_string()).collect();
    let nodes2: BTreeSet<String> = g2.nodes().map(|n| n.uid.to_string()).collect();
    assert_eq!(nodes1, nodes2, "determinism: node UID sets must match");
}

/// Test 7: graceful degradation (R2) — one valid repo + one with a missing path.
/// `index_estate` records bad repo ok:false with an error, good repo ok:true, no panic.
/// `load_estate` returns the good repo's nodes.
#[test]
fn index_estate_degrades_gracefully_on_missing_repo() {
    let tmp = copy_fixture_to_tempdir();

    // Build a manifest with repo-a (valid) + a nonexistent repo.
    let toml = r#"
[workspace]
name = "degradation-test"

[[repos]]
name = "repo-a"
path = "repo-a"

[[repos]]
name = "missing-repo"
path = "nonexistent-path"
"#;
    let manifest = WorkspaceManifest::parse_str(toml).expect("manifest parses");
    // Write the manifest to disk so index_estate has a real path to record in the marker.
    let manifest_path = tmp.path().join("strata.workspace.toml");
    std::fs::write(&manifest_path, toml).expect("write manifest");

    let stats = index_estate(&manifest, &manifest_path, ResolveMode::Off);

    // The valid repo should be ok:true.
    let repo_a_result = stats.repos.iter().find(|r| r.name == "repo-a").unwrap();
    assert!(
        repo_a_result.ok,
        "repo-a should index ok, got error: {:?}",
        repo_a_result.error
    );

    // The missing repo should be ok:false with an error message.
    let missing_result = stats
        .repos
        .iter()
        .find(|r| r.name == "missing-repo")
        .unwrap();
    assert!(!missing_result.ok, "missing-repo must be ok:false");
    assert!(
        missing_result.error.is_some(),
        "missing-repo must have an error message"
    );

    // `load_estate` should return the good repo's nodes (repo-a) even though
    // missing-repo's store doesn't exist.
    let (estate_graph, load_results) = load_estate(&manifest, tmp.path());

    let a_load = load_results.iter().find(|r| r.name == "repo-a").unwrap();
    assert!(a_load.ok, "repo-a should load ok");

    let missing_load = load_results
        .iter()
        .find(|r| r.name == "missing-repo")
        .unwrap();
    assert!(!missing_load.ok, "missing-repo load must be ok:false");

    // repo-a nodes are present in the estate graph.
    let greet_uid = Uid::new("ts", "repo-a", "src/a.ts", "greet", "");
    assert!(
        estate_graph.get_node(&greet_uid).is_some(),
        "repo-a greet node must still be in the estate graph despite missing-repo"
    );
}
