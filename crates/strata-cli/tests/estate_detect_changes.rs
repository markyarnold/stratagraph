//! Estate detect-changes integration tests (Task 6: `detect_changes --workspace`
//! + auto-resolution).
//!
//! Copies the `crossrepo` fixture from strata-index to a tempdir, git-inits the
//! producer repo (so git-diff has a HEAD to diff against), indexes the estate
//! (which writes the `.strata/estate.toml` markers in each member repo), modifies
//! the producer handler, then calls `cmd_detect_changes` in:
//!   1. Auto-resolve mode (no `--db`, no `--workspace`): running in the producer
//!      repo must surface the consumer from the other repo in the affected set.
//!   2. Explicit `--db` mode: single-repo only; consumer must NOT appear
//!      (discriminating negative — back-compat).

use std::path::{Path, PathBuf};
use std::process::Command;

use strata_cli::{cmd_detect_changes, cmd_index_workspace};
use strata_index::ResolveMode;

// ── Fixture helpers ────────────────────────────────────────────────────────────

/// Path to the crossrepo fixture committed inside strata-index.
fn crossrepo_fixture_dir() -> PathBuf {
    // CARGO_MANIFEST_DIR = crates/strata-cli; crossrepo is in crates/strata-index/tests/fixtures/
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../crates/strata-index/tests/fixtures/crossrepo")
}

/// Copy a directory tree to `dst`, skipping any `.strata/` subdirs and `.git/` dirs.
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
fn git_init(dir: &Path) {
    git(dir, &["init", "-q"]);
    git(dir, &["config", "user.email", "t@t"]);
    git(dir, &["config", "user.name", "t"]);
    git(dir, &["config", "commit.gpgsign", "false"]);
}

/// Stage everything and commit.
fn git_commit_all(dir: &Path, msg: &str) {
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

// ── Tests ──────────────────────────────────────────────────────────────────────

/// Auto-resolve mode: run `cmd_detect_changes` with no `--db` and no
/// `--workspace`, in the producer repo working directory. The estate marker
/// causes it to load the full estate graph; modifying a handler in the producer
/// must surface the consumer repo in the affected set.
#[test]
fn detect_changes_on_producer_surfaces_cross_repo_consumer() {
    let tmp = copy_crossrepo_to_tempdir();
    let producer_dir = tmp.path().join("repo-producer");
    let manifest = tmp.path().join("strata.workspace.toml");

    // Git-init the producer repo and commit a baseline so HEAD exists.
    git_init(&producer_dir);
    git_commit_all(&producer_dir, "baseline");

    // Index the estate (writes estate.toml markers in each member repo).
    cmd_index_workspace(&manifest, ResolveMode::Auto, false)
        .expect("estate indexing should succeed");

    // Verify marker exists in producer.
    assert!(
        producer_dir.join(".strata/estate.toml").exists(),
        "estate.toml marker must be written in repo-producer"
    );

    // Modify the producer handler's function body so diff_code_symbols sees a
    // Modified symbol. A comment-only append doesn't change the body slice, so
    // we rewrite the getUser function body itself.
    let handler = producer_dir.join("src/handlers.ts");
    std::fs::write(
        &handler,
        "import express from \"express\";\n\
         \nconst app = express();\n\
         \nexport function getUser(req: any, res: any) {\n\
         res.json({ id: req.params.id, modified: true });\n\
         }\n\
         app.get(\"/users/:id\", getUser);\n\
         \nexport { app };\n",
    )
    .expect("write handlers.ts");

    // Auto-resolve: pass the producer repo as `--repo`, no db, no workspace.
    // The estate marker in producer_dir/.strata/estate.toml triggers estate mode.
    let out = cmd_detect_changes(
        None,                // --db (single-repo forced)
        Some(&producer_dir), // --repo (the member repo root)
        None,                // --workspace (auto-resolve from repo)
        false,               // --staged
    )
    .expect("auto-resolved estate detect_changes must succeed");

    assert!(
        out.contains("fetchUserProfile") || out.contains("getUserViaClient"),
        "estate detect_changes must surface the consumer in the affected set; got:\n{out}"
    );
}

/// Explicit `--db` mode: single-repo detect_changes on the producer's own db
/// must NOT show consumer symbols (back-compat: estate invisible when db forced).
#[test]
fn detect_changes_with_explicit_db_stays_single_repo() {
    let tmp = copy_crossrepo_to_tempdir();
    let producer_dir = tmp.path().join("repo-producer");
    let manifest = tmp.path().join("strata.workspace.toml");

    // Git-init and commit so HEAD exists.
    git_init(&producer_dir);
    git_commit_all(&producer_dir, "baseline");

    // Index the estate.
    cmd_index_workspace(&manifest, ResolveMode::Auto, false)
        .expect("estate indexing should succeed");

    // Modify the producer handler's function body so diff_code_symbols sees a
    // Modified symbol. A comment-only append doesn't change the body slice, so
    // we rewrite the getUser function body itself (same mutation as the positive test).
    let handler = producer_dir.join("src/handlers.ts");
    std::fs::write(
        &handler,
        "import express from \"express\";\n\
         \nconst app = express();\n\
         \nexport function getUser(req: any, res: any) {\n\
         res.json({ id: req.params.id, modified: true });\n\
         }\n\
         app.get(\"/users/:id\", getUser);\n\
         \nexport { app };\n",
    )
    .expect("write handlers.ts");

    // Explicit --db: single-repo; consumer must not appear.
    let producer_db = producer_dir.join(".strata/graph.duckdb");
    let out = cmd_detect_changes(
        Some(&producer_db),  // --db (forces single-repo)
        Some(&producer_dir), // --repo
        None,                // --workspace
        false,               // --staged
    )
    .expect("explicit-db detect_changes must succeed");

    assert!(
        out.contains("getUser"),
        "single-repo --db must still surface the changed symbol; got:\n{out}"
    );
    assert!(
        !out.contains("fetchUserProfile") && !out.contains("getUserViaClient"),
        "explicit --db must NOT surface cross-repo consumers; got:\n{out}"
    );
}
