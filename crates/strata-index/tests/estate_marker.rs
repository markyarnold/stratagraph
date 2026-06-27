use std::path::PathBuf;
use strata_index::estate_marker::{read_marker, write_marker, EstateMarker};
use strata_index::{index_estate, resolve_context, IndexContext, ResolveMode, WorkspaceManifest};
use tempfile::TempDir;

#[test]
fn write_then_read_roundtrips() {
    let tmp = TempDir::new().unwrap();
    let strata = tmp.path().join(".strata");
    std::fs::create_dir_all(&strata).unwrap();
    let m = EstateMarker {
        manifest: PathBuf::from("/abs/estate/strata.workspace.toml"),
        estate: "acme-platform".into(),
        repo: "api".into(),
    };
    write_marker(&strata, &m).unwrap();
    assert!(strata.join("estate.toml").exists());
    assert_eq!(read_marker(&strata), Some(m));
}

#[test]
fn read_missing_is_none() {
    let tmp = TempDir::new().unwrap();
    let strata = tmp.path().join(".strata");
    std::fs::create_dir_all(&strata).unwrap();
    assert_eq!(read_marker(&strata), None);
}

#[test]
fn read_garbage_is_none_not_panic() {
    let tmp = TempDir::new().unwrap();
    let strata = tmp.path().join(".strata");
    std::fs::create_dir_all(&strata).unwrap();
    std::fs::write(strata.join("estate.toml"), "this is not = valid [ toml").unwrap();
    assert_eq!(read_marker(&strata), None);
}

#[test]
fn estate_index_writes_a_marker_per_member() {
    // Reuse the existing estate fixture (repo-a / repo-b).
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/estate");
    let tmp = TempDir::new().unwrap();
    // Copy the fixture so we write .strata into a throwaway dir.
    copy_dir(&fixture, tmp.path());
    let manifest_path = tmp.path().join("strata.workspace.toml");
    let manifest = WorkspaceManifest::parse_file(&manifest_path).unwrap();

    let _stats = index_estate(&manifest, &manifest_path, ResolveMode::Auto);

    for (repo_dir, repo_name) in [("repo-a", "repo-a"), ("repo-b", "repo-b")] {
        let strata = tmp.path().join(repo_dir).join(".strata");
        let marker = read_marker(&strata).expect("member should have an estate marker");
        assert_eq!(marker.repo, repo_name);
        assert_eq!(marker.estate, "test-estate");
        assert_eq!(marker.manifest, manifest_path.canonicalize().unwrap());
    }
}

// ── Task 3: resolve_context tests ────────────────────────────────────────────

#[test]
fn resolve_returns_single_without_marker() {
    let tmp = TempDir::new().unwrap();
    std::fs::create_dir_all(tmp.path().join(".strata")).unwrap();
    match resolve_context(tmp.path()) {
        IndexContext::Single { db } => {
            assert_eq!(db, tmp.path().join(".strata/graph.duckdb"));
        }
        _ => panic!("expected Single"),
    }
}

#[test]
fn resolve_returns_estate_with_valid_marker() {
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/estate");
    let tmp = TempDir::new().unwrap();
    copy_dir(&fixture, tmp.path());
    let manifest_path = tmp.path().join("strata.workspace.toml");
    let manifest = WorkspaceManifest::parse_file(&manifest_path).unwrap();
    index_estate(&manifest, &manifest_path, ResolveMode::Auto);

    let repo_a = tmp.path().join("repo-a");
    // Fix C: resolve_context now stores the canonicalized repo_root, so compare
    // against the canonical form (resolves macOS /var → /private/var symlinks).
    let repo_a_canon = repo_a.canonicalize().unwrap_or_else(|_| repo_a.clone());
    match resolve_context(&repo_a) {
        IndexContext::Estate {
            repo, repo_root, ..
        } => {
            assert_eq!(repo, "repo-a");
            assert_eq!(repo_root, repo_a_canon);
        }
        _ => panic!("expected Estate"),
    }
}

#[test]
fn resolve_falls_back_to_single_when_manifest_deleted() {
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/estate");
    let tmp = TempDir::new().unwrap();
    copy_dir(&fixture, tmp.path());
    let manifest_path = tmp.path().join("strata.workspace.toml");
    let manifest = WorkspaceManifest::parse_file(&manifest_path).unwrap();
    index_estate(&manifest, &manifest_path, ResolveMode::Auto);
    std::fs::remove_file(&manifest_path).unwrap(); // manifest gone

    assert!(
        matches!(
            resolve_context(&tmp.path().join("repo-a")),
            IndexContext::Single { .. }
        ),
        "stale marker (manifest deleted) must degrade to Single"
    );
}

/// Fix D: marker is present and syntactically valid (manifest parses, repo name
/// is listed in the manifest), BUT the marker lives in a directory whose
/// canonical path does NOT match what the manifest records for that repo.
///
/// Concretely: index `repo-a` so it gets a valid marker, then copy the marker
/// into a second scratch directory (`repo-impostor`) whose path is NOT listed in
/// the manifest. `resolve_context` must reject the mismatch and return `Single`.
#[test]
fn resolve_falls_back_to_single_when_repo_path_differs() {
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/estate");
    let tmp = TempDir::new().unwrap();
    copy_dir(&fixture, tmp.path());
    let manifest_path = tmp.path().join("strata.workspace.toml");
    let manifest = WorkspaceManifest::parse_file(&manifest_path).unwrap();
    // Index the estate so repo-a gets a real marker pointing at `manifest_path`.
    index_estate(&manifest, &manifest_path, ResolveMode::Auto);

    // Create a completely different directory ("impostor") with its own .strata/.
    let impostor = tmp.path().join("repo-impostor");
    let impostor_strata = impostor.join(".strata");
    std::fs::create_dir_all(&impostor_strata).unwrap();

    // Copy repo-a's marker verbatim into the impostor's .strata/.
    // The marker still says repo = "repo-a" and manifest = <manifest_path>,
    // but the impostor directory resolves to a path the manifest doesn't list.
    let repo_a_marker_src = tmp.path().join("repo-a/.strata/estate.toml");
    std::fs::copy(&repo_a_marker_src, impostor_strata.join("estate.toml")).unwrap();

    // resolve_context must detect the path mismatch and degrade to Single.
    assert!(
        matches!(resolve_context(&impostor), IndexContext::Single { .. }),
        "marker with mismatched repo path must degrade to Single, not Estate"
    );
}

// Minimal recursive copy helper (fixtures are tiny).
fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let dst = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            std::fs::create_dir_all(&dst).unwrap();
            copy_dir(&entry.path(), &dst);
        } else {
            std::fs::copy(entry.path(), &dst).unwrap();
        }
    }
}
