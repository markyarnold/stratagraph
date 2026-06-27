//! End-to-end estate integration test (Task 10 regression anchor).
//!
//! Exercises the full estate-aware flow in sequence on the `crossrepo` fixture,
//! confirming that the Task 1-9 pieces compose correctly:
//!
//! 1. `cmd_index_workspace` writes `estate.toml` markers in both member repos.
//! 2. Auto-resolved `cmd_blast` on the producer file surfaces the cross-repo
//!    consumer; explicit `--db` blast stays single-repo.
//! 3. After a git edit to the producer, auto-resolved `cmd_detect_changes`
//!    surfaces the cross-repo consumer; explicit `--db` does not.
//! 4. A plain `cmd_index` reindex of the producer keeps estate-qualified UIDs
//!    and leaves the `estate.toml` marker intact.
//!
//! Each step feeds the next (the marker written in step 1 enables auto-resolve
//! in steps 2-3; the same DB written by step 1 is reindexed in step 4), so the
//! composed sequence is the regression value — not copies of the per-task tests.

use std::path::{Path, PathBuf};
use std::process::Command;

use strata_cli::{cmd_blast, cmd_detect_changes, cmd_index, cmd_index_workspace, BlastFormat};
use strata_index::{estate_marker, ResolveMode};
use strata_store::{DuckGraphStore, GraphStore};

// ── Fixture helpers ────────────────────────────────────────────────────────────

/// Path to the crossrepo fixture committed inside strata-index.
fn crossrepo_fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../crates/strata-index/tests/fixtures/crossrepo")
}

/// Copy a directory tree to `dst`, skipping `.strata/` and `.git/` subdirs.
fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == ".strata" || name == ".git" {
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

/// Copy the crossrepo fixture to a fresh tempdir; return the dir.
fn copy_crossrepo_to_tempdir() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    copy_dir_all(&crossrepo_fixture_dir(), tmp.path()).expect("copy crossrepo fixture");
    tmp
}

/// Run `git <args>` at `dir`, asserting success.
fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Initialise a git repo at `dir` with a deterministic, identity-free config.
fn git_init_and_commit(dir: &Path, msg: &str) {
    git(dir, &["init", "-q"]);
    git(dir, &["config", "user.email", "t@t"]);
    git(dir, &["config", "user.name", "t"]);
    git(dir, &["config", "commit.gpgsign", "false"]);
    git(dir, &["add", "-A"]);
    git(
        dir,
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            msg,
        ],
    );
}

/// Load node UIDs from a `graph.duckdb`.
fn load_node_uids(db_path: &Path) -> Vec<String> {
    let store = DuckGraphStore::open(db_path).expect("open store");
    let graph = store.load_graph().expect("load graph");
    graph.nodes().map(|n| n.uid.0.clone()).collect()
}

// ── End-to-end test ───────────────────────────────────────────────────────────

/// Full estate-aware flow: index → markers → auto blast → auto detect_changes
/// → plain reindex preserves estate qualification.
///
/// Each assertion uses the output of the previous step, making this a true
/// composition regression (not independent per-task checks).
#[test]
fn estate_e2e_full_flow() {
    let tmp = copy_crossrepo_to_tempdir();
    let manifest = tmp.path().join("strata.workspace.toml");
    let producer_dir = tmp.path().join("repo-producer");
    let consumer_dir = tmp.path().join("repo-consumer");
    let producer_db = producer_dir.join(".strata/graph.duckdb");
    let producer_strata = producer_dir.join(".strata");
    let consumer_strata = consumer_dir.join(".strata");
    let producer_file = producer_dir.join("src/handlers.ts");

    // ── Step 1: estate index writes markers in BOTH members ───────────────────
    //
    // git-init the producer so detect_changes (step 3) has a HEAD to diff against.
    // Do this BEFORE indexing so the git metadata is present when `strata index
    // --workspace` runs.
    git_init_and_commit(&producer_dir, "baseline");

    cmd_index_workspace(&manifest, ResolveMode::Auto, false)
        .expect("step 1: estate indexing should succeed");

    // Verify markers exist in both repos and carry the correct fields.
    let producer_marker = estate_marker::read_marker(&producer_strata)
        .expect("step 1: estate.toml must be written in repo-producer");
    assert_eq!(
        producer_marker.repo, "repo-producer",
        "step 1: producer marker.repo must be 'repo-producer'"
    );
    assert_eq!(
        producer_marker.estate, "shop-estate",
        "step 1: producer marker.estate must be 'shop-estate'"
    );

    let consumer_marker = estate_marker::read_marker(&consumer_strata)
        .expect("step 1: estate.toml must be written in repo-consumer");
    assert_eq!(
        consumer_marker.repo, "repo-consumer",
        "step 1: consumer marker.repo must be 'repo-consumer'"
    );
    assert_eq!(
        consumer_marker.estate, "shop-estate",
        "step 1: consumer marker.estate must be 'shop-estate'"
    );

    // ── Step 2a: auto-resolve blast surfaces the cross-repo consumer ──────────
    //
    // Pass the absolute producer file path; no db, no workspace. The marker
    // written in step 1 drives auto-resolution to estate mode.
    let producer_file_str = producer_file.to_str().expect("valid UTF-8 path");
    let blast_auto = cmd_blast(None, None, None, producer_file_str, BlastFormat::Agent)
        .expect("step 2a: auto-resolved estate blast must succeed");

    assert!(
        blast_auto.contains("repo-consumer")
            || blast_auto.contains("fetchUserProfile")
            || blast_auto.contains("getUserViaClient")
            || blast_auto.contains("api.ts"),
        "step 2a: auto blast must surface the consumer in the other repo; got:\n{blast_auto}"
    );

    // ── Step 2b: explicit --db blast stays single-repo (discriminating control) ─
    let blast_explicit_db = cmd_blast(
        Some(producer_db.as_path()),
        None,
        None,
        producer_file_str,
        BlastFormat::Agent,
    )
    .expect("step 2b: explicit-db blast must succeed");

    assert!(
        !blast_explicit_db.contains("repo-consumer")
            && !blast_explicit_db.contains("fetchUserProfile")
            && !blast_explicit_db.contains("getUserViaClient")
            && !blast_explicit_db.contains("api.ts"),
        "step 2b: explicit-db blast must NOT surface cross-repo consumers; got:\n{blast_explicit_db}"
    );

    // ── Step 3: modify the producer handler and run detect_changes ────────────
    //
    // Rewrite the getUser function body so diff_code_symbols reports a Modified
    // symbol (a comment-only change would not alter the body slice).
    std::fs::write(
        &producer_file,
        "import express from \"express\";\n\
         \nconst app = express();\n\
         \nexport function getUser(req: any, res: any) {\n\
         res.json({ id: req.params.id, modified: true });\n\
         }\n\
         app.get(\"/users/:id\", getUser);\n\
         \nexport { app };\n",
    )
    .expect("step 3: write modified handlers.ts");

    // ── Step 3a: auto-resolve detect_changes surfaces the cross-repo consumer ──
    let dc_auto = cmd_detect_changes(
        None,                // no --db (auto-resolve)
        Some(&producer_dir), // --repo = producer root
        None,                // no --workspace
        false,               // not staged
    )
    .expect("step 3a: auto-resolved estate detect_changes must succeed");

    assert!(
        dc_auto.contains("fetchUserProfile") || dc_auto.contains("getUserViaClient"),
        "step 3a: auto detect_changes must surface the cross-repo consumer; got:\n{dc_auto}"
    );

    // ── Step 3b: explicit --db detect_changes stays single-repo ──────────────
    let dc_explicit = cmd_detect_changes(
        Some(&producer_db),  // --db (forces single-repo)
        Some(&producer_dir), // --repo
        None,                // no --workspace
        false,
    )
    .expect("step 3b: explicit-db detect_changes must succeed");

    // The changed symbol must still appear (it's in the single-repo graph).
    assert!(
        dc_explicit.contains("getUser"),
        "step 3b: single-repo detect_changes must still surface the changed symbol; got:\n{dc_explicit}"
    );
    // But the cross-repo consumer must NOT appear.
    assert!(
        !dc_explicit.contains("fetchUserProfile") && !dc_explicit.contains("getUserViaClient"),
        "step 3b: explicit --db must NOT surface cross-repo consumers; got:\n{dc_explicit}"
    );

    // ── Step 4: plain cmd_index keeps estate-qualified UIDs + marker ──────────
    //
    // A plain single-repo reindex must: (a) honour the estate.toml marker left
    // by step 1 so UID package = "repo-producer" (not the dir basename which
    // happens to be the same, but the estate-qualified value), and (b) leave the
    // marker intact.
    cmd_index(&producer_dir, &producer_db, false).expect("step 4: plain reindex must succeed");

    let uids = load_node_uids(&producer_db);
    assert!(
        !uids.is_empty(),
        "step 4: graph must still have nodes after plain reindex"
    );
    // At least one UID must be qualified with the manifest name "repo-producer"
    // (confirming estate qualification is active, not falling back to basename).
    // We cannot assert ALL UIDs, because external/synthetic nodes (e.g. `express`)
    // use `<external>` as the package segment by design.
    assert!(
        uids.iter().any(|uid| uid.contains("|repo-producer|")),
        "step 4: at least one UID must be estate-qualified with 'repo-producer'; got: {uids:?}"
    );
    // Verify that estate-qualified package format is preserved on reindex: every
    // non-external UID must carry the "repo-producer" package segment, confirming
    // that estate qualification (not a basename fallback) is active. The
    // basename-vs-manifest-name distinction is proven separately in
    // `estate_member_reindex.rs`, where those two values differ.
    let non_external_uids: Vec<_> = uids
        .iter()
        .filter(|uid| !uid.contains("|<external>|"))
        .collect();
    assert!(
        non_external_uids
            .iter()
            .all(|uid| uid.contains("|repo-producer|")),
        "step 4: every non-external UID must be qualified with 'repo-producer'; got: {non_external_uids:?}"
    );

    // Marker must still be present and correct after the plain reindex.
    let marker_after_reindex = estate_marker::read_marker(&producer_strata)
        .expect("step 4: estate.toml marker must still exist after plain reindex");
    assert_eq!(
        marker_after_reindex.repo, "repo-producer",
        "step 4: marker.repo must still be 'repo-producer' after plain reindex"
    );
    assert_eq!(
        marker_after_reindex.estate, "shop-estate",
        "step 4: marker.estate must still be 'shop-estate' after plain reindex"
    );
}
