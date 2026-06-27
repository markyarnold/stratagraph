//! Task 7: plain `strata index <member>` on an estate member reindexes with
//! the manifest-declared name (estate-qualified UIDs), not the directory basename.
//!
//! # Discriminating scenario
//!
//! The estate manifest names the member repo `"my-pkg"`, but its directory on
//! disk is `member-dir/`.  Without the fix, a plain `cmd_index` would derive
//! the repo name from the basename (`"member-dir"`) and produce UIDs whose
//! `package` segment is `"member-dir"` — inconsistent with the estate.  With
//! the fix, `resolve_context` reads the `.strata/estate.toml` marker, finds
//! `repo = "my-pkg"`, and `index_repo_named` qualifies all UIDs with `"my-pkg"`.
//!
//! # Test flow
//!
//! 1. Build a minimal estate in a tempdir:
//!    - `<tmp>/strata.workspace.toml`  — manifest with `name = "my-pkg"` for the member.
//!    - `<tmp>/member-dir/src/lib.ts`  — a TypeScript source file.
//! 2. Index the whole estate via `cmd_index_workspace` — this writes the
//!    `.strata/estate.toml` marker in `<tmp>/member-dir/.strata/`.
//! 3. Load `member-dir`'s `graph.duckdb` and confirm at least one node has
//!    `package == "my-pkg"` (estate-qualified, not the basename).
//! 4. Run plain `cmd_index(<tmp>/member-dir, <db-path>, false)` — the plain
//!    member reindex.
//! 5. Reload the graph and assert:
//!    - All node UIDs still have `package == "my-pkg"` (NOT `"member-dir"`).
//!    - The `.strata/estate.toml` marker is still present.

use std::path::PathBuf;

use strata_cli::{cmd_index, cmd_index_workspace};
use strata_index::estate_marker;
use strata_index::ResolveMode;
use strata_store::{DuckGraphStore, GraphStore};

// ── Fixture helpers ────────────────────────────────────────────────────────────

/// Build a minimal estate in a fresh tempdir and return it.
///
/// Layout:
/// ```text
/// <tmp>/
///   strata.workspace.toml     ← manifest: member-dir as "my-pkg"
///   member-dir/
///     src/
///       lib.ts                ← one TS source so the graph has nodes
/// ```
fn make_estate_tempdir() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");

    // The workspace manifest: directory basename is `member-dir`, manifest name
    // is `my-pkg` — deliberately different so a basename-reindex is detectable.
    std::fs::write(
        tmp.path().join("strata.workspace.toml"),
        "[workspace]\nname = \"test-estate\"\n\n[[repos]]\nname = \"my-pkg\"\npath = \"member-dir\"\n",
    )
    .expect("write manifest");

    // The member repo source.
    let src_dir = tmp.path().join("member-dir/src");
    std::fs::create_dir_all(&src_dir).expect("create src dir");
    std::fs::write(
        src_dir.join("lib.ts"),
        "// member repo: lib.ts\nexport function greet(name: string): string {\n  return \"hello \" + name;\n}\n",
    )
    .expect("write lib.ts");

    tmp
}

/// Open `graph.duckdb` at `db_path` and return all node UID strings.
fn load_node_uids(db_path: &std::path::Path) -> Vec<String> {
    let store = DuckGraphStore::open(db_path).expect("open store");
    let graph = store.load_graph().expect("load graph");
    graph.nodes().map(|n| n.uid.0.clone()).collect()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// A plain `cmd_index` on a member that has an `.strata/estate.toml` marker
/// must reindex with the manifest-declared name (`"my-pkg"`), not the directory
/// basename (`"member-dir"`).  The marker must survive the reindex unchanged.
#[test]
fn plain_index_of_estate_member_keeps_manifest_name_and_marker() {
    let tmp = make_estate_tempdir();
    let manifest = tmp.path().join("strata.workspace.toml");
    let member_dir = tmp.path().join("member-dir");
    let db_path: PathBuf = member_dir.join(".strata/graph.duckdb");
    let strata_dir = member_dir.join(".strata");

    // ── Step 1: index the whole estate. ────────────────────────────────────────
    // This writes the marker and produces graph.duckdb with "my-pkg"-qualified UIDs.
    cmd_index_workspace(&manifest, ResolveMode::Auto, false)
        .expect("estate indexing should succeed");

    // Verify the marker was written.
    assert!(
        strata_dir.join("estate.toml").exists(),
        "estate.toml marker must exist after estate index"
    );

    // Verify the initial index used "my-pkg", not "member-dir".
    let uids_after_estate_index = load_node_uids(&db_path);
    assert!(
        !uids_after_estate_index.is_empty(),
        "graph must have nodes after estate index"
    );
    assert!(
        uids_after_estate_index
            .iter()
            .all(|uid| !uid.contains("|member-dir|")),
        "after estate index, NO uid should be basename-qualified (member-dir); got: {uids_after_estate_index:?}"
    );
    assert!(
        uids_after_estate_index
            .iter()
            .any(|uid| uid.contains("|my-pkg|")),
        "after estate index, at least one uid must be manifest-qualified (my-pkg); got: {uids_after_estate_index:?}"
    );

    // ── Step 2: plain reindex of the member in isolation. ──────────────────────
    // This is the regression: without the fix, this would use "member-dir" as the
    // repo name, corrupting the estate-qualified UIDs.
    cmd_index(&member_dir, &db_path, false).expect("plain member reindex must succeed");

    // ── Step 3: assert UIDs are still "my-pkg"-qualified. ──────────────────────
    let uids_after_reindex = load_node_uids(&db_path);
    assert!(
        !uids_after_reindex.is_empty(),
        "graph must still have nodes after plain reindex"
    );
    assert!(
        uids_after_reindex
            .iter()
            .all(|uid| !uid.contains("|member-dir|")),
        "after plain reindex, NO uid must use the basename package (member-dir); \
         if any do, the estate-qualification was lost. UIDs: {uids_after_reindex:?}"
    );
    assert!(
        uids_after_reindex
            .iter()
            .any(|uid| uid.contains("|my-pkg|")),
        "after plain reindex, at least one uid must still use the manifest name (my-pkg); \
         UIDs: {uids_after_reindex:?}"
    );

    // ── Step 4: assert the marker is still present. ────────────────────────────
    let marker = estate_marker::read_marker(&strata_dir);
    assert!(
        marker.is_some(),
        "estate.toml marker must still exist after plain reindex; \
         it must not be deleted or corrupted"
    );
    let marker = marker.unwrap();
    assert_eq!(
        marker.repo, "my-pkg",
        "marker.repo must still be 'my-pkg' after plain reindex"
    );
}
