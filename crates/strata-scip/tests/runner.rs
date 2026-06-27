//! Runner tests.
//!
//! - The robustness tests (test 7) run everywhere, with no Node: they prove
//!   `run_scip` returns a structured `ScipError` (never panics, never hangs) for
//!   bad inputs.
//! - The live integration test (test 8) is **gated** behind `STRATA_SCIP_LIVE=1`
//!   (it needs Node + npm + network). When enabled it copies the committed
//!   sample project to a tempdir, runs the real `scip-typescript`, and confirms
//!   the freshly produced index resolves the aliased call exactly like the
//!   committed fixture — proving both the runner and that the fixture is
//!   faithful.

use std::path::{Path, PathBuf};

use strata_scip::{run_scip, Position, RunOptions, ScipError, ScipResolver};

// Test 7: robustness — nonexistent directory yields a structured error.
#[test]
fn run_scip_on_missing_dir_errors_without_panicking() {
    let missing = Path::new("/this/path/definitely/does/not/exist/strata-scip");
    let err = run_scip(missing, &RunOptions::default())
        .expect_err("a missing project dir must be an Err");
    // Must be a structured ScipError (Io here, since the dir check fails first).
    match err {
        ScipError::Io(_) | ScipError::ToolUnavailable(_) => {}
        other => panic!("unexpected error variant: {other:?}"),
    }
}

// Test 7 (cont.): a real (temp) directory with no package.json must also yield a
// structured error (npm install / indexing failure), not a panic or a hang.
#[test]
fn run_scip_on_empty_dir_errors_without_panicking() {
    let dir = tempfile::tempdir().expect("create tempdir");
    // run_npm_install = true on a dir with no package.json: npm install fails
    // (or, if npm is absent, ToolUnavailable). Either way: a structured error.
    let opts = RunOptions {
        timeout_secs: 120,
        ..RunOptions::default()
    };
    let result = run_scip(dir.path(), &opts);
    match result {
        Err(ScipError::InstallFailed { .. })
        | Err(ScipError::ToolUnavailable(_))
        | Err(ScipError::IndexFailed { .. })
        | Err(ScipError::Timeout { .. })
        | Err(ScipError::Io(_)) => {}
        Err(ScipError::Parse(_)) => panic!("did not expect a parse error here"),
        Ok(path) => panic!("expected failure on an empty dir, got index at {path:?}"),
    }
}

// Test 7 (cont.): the default options pin the verified scip-typescript version.
#[test]
fn default_options_pin_the_verified_version() {
    assert_eq!(
        RunOptions::default().scip_typescript_version,
        strata_scip::PINNED_SCIP_TYPESCRIPT_VERSION
    );
    assert_eq!(
        strata_scip::PINNED_SCIP_TYPESCRIPT_VERSION,
        "0.4.0",
        "the pinned version must match what the fixtures were generated with"
    );
}

/// Path to the committed `sample` fixture project.
fn sample_fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("sample")
}

/// Recursively copy a directory, skipping `node_modules` and any existing
/// `index.scip` (so the live run produces its own from scratch).
fn copy_project(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).expect("create dst");
    for entry in std::fs::read_dir(src).expect("read_dir") {
        let entry = entry.expect("dir entry");
        let name = entry.file_name();
        if name == "node_modules" || name == "index.scip" || name == "package-lock.json" {
            continue;
        }
        let from = entry.path();
        let to = dst.join(&name);
        if from.is_dir() {
            copy_project(&from, &to);
        } else {
            std::fs::copy(&from, &to).expect("copy file");
        }
    }
}

// Test 8 (gated live integration): actually run scip-typescript end-to-end.
// Enable with `STRATA_SCIP_LIVE=1 cargo test -p strata-scip`.
#[test]
fn live_run_scip_produces_faithful_index() {
    if std::env::var("STRATA_SCIP_LIVE").as_deref() != Ok("1") {
        eprintln!("skipping live scip-typescript test (set STRATA_SCIP_LIVE=1 to run)");
        return;
    }

    let tmp = tempfile::tempdir().expect("create tempdir");
    let project = tmp.path().join("sample");
    copy_project(&sample_fixture_dir(), &project);

    let index_path =
        run_scip(&project, &RunOptions::default()).expect("live scip-typescript run succeeds");
    assert!(index_path.is_file(), "index.scip was written");

    // The freshly produced index must resolve the aliased call exactly like the
    // committed fixture (test 2): bar() -> b.ts's foo.
    let resolver = ScipResolver::from_index_file(&index_path).expect("parse live index");
    let target = resolver
        .resolve_at("src/a.ts", Position::new(1, 25))
        .expect("bar() resolves in live index");
    assert_eq!(target.def_file.as_deref(), Some("src/b.ts"));
    assert!(target.moniker.contains("foo()"), "got {}", target.moniker);
    assert!(!target.is_external);
}
