//! Estate-blast integration tests (Task 5: `blast --workspace` + auto-resolution).
//!
//! Copies the `crossrepo` fixture from strata-index to a tempdir, indexes it as
//! an estate (which writes the `.strata/estate.toml` markers in each member repo),
//! then calls `cmd_blast` in:
//!   1. Auto-resolve mode (no `--db`, no `--workspace`): the producer file should
//!      surface the consumer from the other repo.
//!   2. Explicit-`--db` mode: single-repo only; consumer must NOT appear.

use std::path::{Path, PathBuf};

use strata_cli::{cmd_blast, cmd_index_workspace, BlastFormat};
use strata_index::ResolveMode;

// ── Fixture helpers ────────────────────────────────────────────────────────────

/// Path to the crossrepo fixture committed inside strata-index.
fn crossrepo_fixture_dir() -> PathBuf {
    // CARGO_MANIFEST_DIR = crates/strata-cli; crossrepo is in crates/strata-index/tests/fixtures/
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../crates/strata-index/tests/fixtures/crossrepo")
}

/// Copy a directory tree to `dst`, skipping any `.strata/` subdirs.
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

/// Copy the crossrepo fixture to a fresh tempdir; return the dir.
fn copy_crossrepo_to_tempdir() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    copy_dir_all(&crossrepo_fixture_dir(), tmp.path()).expect("copy crossrepo fixture");
    tmp
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// Auto-resolve: no `--db`, no `--workspace` — blast on the producer file must
/// surface the consumer in the other repo via estate linking.
#[test]
fn blast_on_producer_surfaces_cross_repo_consumer() {
    let tmp = copy_crossrepo_to_tempdir();
    let manifest = tmp.path().join("strata.workspace.toml");

    // Index the estate (writes estate.toml markers in each member repo).
    cmd_index_workspace(&manifest, ResolveMode::Auto, false)
        .expect("estate indexing should succeed");

    // Verify markers exist (both member repos indexed).
    assert!(
        tmp.path()
            .join("repo-producer/.strata/estate.toml")
            .exists(),
        "estate.toml marker must be written in repo-producer"
    );
    assert!(
        tmp.path()
            .join("repo-consumer/.strata/estate.toml")
            .exists(),
        "estate.toml marker must be written in repo-consumer"
    );

    // Auto-resolve: pass the absolute path to the producer file; no db, no workspace.
    let producer_file = tmp.path().join("repo-producer/src/handlers.ts");
    let producer_file_str = producer_file.to_str().expect("valid UTF-8 path");

    let out = cmd_blast(None, None, None, producer_file_str, BlastFormat::Agent)
        .expect("auto-resolved estate blast must succeed");

    assert!(
        out.contains("repo-consumer") || out.contains("api.ts"),
        "estate blast must surface the consumer in the other repo; got:\n{out}"
    );
}

/// Fix A: never-confident-wrong — a file that is NOT under any estate member
/// (an "orphan" file outside all manifest-listed repos) blasted via the
/// explicit `--workspace` path must NOT surface symbols from other repos.
///
/// Before the fix, `blast_estate` fell through to
/// `blast_for_file_in_repo(&graph, &rel, None)` — an unscoped blast over the
/// full estate graph — which could surface any repo's symbols. After the fix it
/// falls back to a single-repo blast or an honest empty/note, never unscoped.
///
/// ## Discrimination design
///
/// The crossrepo fixture's `repo-consumer` repo has a node at path `src/api.ts`.
/// `node_in_file` uses a component-boundary suffix match: `"src/api.ts"` matches
/// file `"api.ts"` because `"src/api.ts".ends_with("api.ts")` and the preceding
/// byte is `'/'`.  So an orphan placed at `<tmp>/api.ts` (outside all estate
/// member directories) will, with the OLD unscoped blast
/// (`blast_for_file_in_repo(&graph, "api.ts", None)`), hit the
/// `repo-consumer/src/api.ts` node and surface `repo-consumer` in the output.
/// The FIXED code never reaches that call — it returns the honest "not under any
/// indexed estate member" note instead.
///
/// This makes the test genuinely discriminating: it fails against the old bug and
/// passes against the fix.
#[test]
fn blast_estate_file_not_in_any_member_does_not_blast_unscoped() {
    let tmp = copy_crossrepo_to_tempdir();
    let manifest = tmp.path().join("strata.workspace.toml");

    cmd_index_workspace(&manifest, ResolveMode::Auto, false)
        .expect("estate indexing should succeed");

    // Create a file that lives OUTSIDE all estate member directories — directly
    // in the tmp root, which is only the estate root, not a listed repo.
    //
    // Critically: the basename is `api.ts`, which suffix-matches the
    // `repo-consumer/src/api.ts` node already in the estate graph (via
    // component-boundary suffix: `"src/api.ts".ends_with("api.ts")` and the byte
    // before the match is `'/'`).  The OLD unscoped blast would surface
    // `repo-consumer` here; the fixed code must not.
    let orphan_file = tmp.path().join("api.ts");
    std::fs::write(&orphan_file, "export function orphan() {}").expect("write orphan file");
    let orphan_str = orphan_file.to_str().expect("valid UTF-8 path");

    // Blast via explicit --workspace. Before Fix A this would call
    // blast_for_file_in_repo with repo=None (unscoped), surfacing repo-consumer
    // via the `api.ts` suffix match. After Fix A it must not.
    let out = cmd_blast(
        None,
        None,
        Some(manifest.as_path()),
        orphan_str,
        BlastFormat::Agent,
    )
    .expect("estate blast on orphan file must not error");

    // 1. The output must NOT contain symbols from either estate member repo.
    assert!(
        !out.contains("repo-consumer") && !out.contains("repo-producer"),
        "Fix A: unmatched file must NOT surface other repos' symbols; got:\n{out}"
    );

    // 2. Positive assertion: the honest fallback note must be present.
    //    The production code in blast_estate emits this note verbatim when no
    //    estate member matches and no local .strata/graph.duckdb exists.
    assert!(
        out.contains("not under any indexed estate member"),
        "Fix A: output must contain the honest fallback note; got:\n{out}"
    );
}

/// Explicit `--db`: single-repo blast on the producer's own db must NOT show
/// the consumer (back-compat: the estate is invisible when db is forced).
#[test]
fn blast_with_explicit_db_stays_single_repo() {
    let tmp = copy_crossrepo_to_tempdir();
    let manifest = tmp.path().join("strata.workspace.toml");

    cmd_index_workspace(&manifest, ResolveMode::Auto, false)
        .expect("estate indexing should succeed");

    let producer_db = tmp.path().join("repo-producer/.strata/graph.duckdb");
    let producer_file = tmp.path().join("repo-producer/src/handlers.ts");
    let producer_file_str = producer_file.to_str().expect("valid UTF-8 path");

    let out = cmd_blast(
        Some(producer_db.as_path()),
        None,
        None,
        producer_file_str,
        BlastFormat::Agent,
    )
    .expect("explicit-db blast must succeed");

    // Consumer lives in a different repo's graph; single-repo blast must not see it.
    assert!(
        !out.contains("repo-consumer"),
        "explicit-db blast must NOT surface the consumer from another repo; got:\n{out}"
    );
}
