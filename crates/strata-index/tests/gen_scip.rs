//! One-off SCIP index generators for the committed corpus fixtures.
//!
//! These are `#[ignore]`d (they need Node/rust-analyzer + network) and exist only
//! to (re)write the committed `index.scip` for each corpus project. Run
//! explicitly:
//!
//! ```text
//! STRATA_SCIP_LIVE=1 cargo test -p strata-index --test gen_scip -- --ignored
//! # or, per language:
//! STRATA_SCIP_LIVE=1 cargo test -p strata-index --test gen_scip -- --ignored generate_py
//! ```
//!
//! They are NOT part of the normal hermetic suite; the gate tests consume the
//! committed indexes with no indexer.

use std::path::{Path, PathBuf};
use std::process::Command;

use strata_scip::{run_scip, RunOptions};

fn project(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("accuracy")
        .join(name)
}

/// Generate `index.scip` for a Rust corpus project via `rust-analyzer scip .`.
/// Each corpus crate is an isolated workspace (empty `[workspace]`), so
/// `cargo metadata` (which rust-analyzer needs) succeeds. Mirrors the dev-time
/// command in `rust-resolution.md`. The committed `.scip` is the hermetic
/// artifact the gate consumes; this regenerator is `#[ignore]`d. The `target/`
/// rust-analyzer leaves behind is gitignored (see the corpus `.gitignore`).
fn generate_rust(dir_name: &str) {
    let dir = project("rust-corpus").join(dir_name);
    assert!(dir.is_dir(), "rust corpus project must exist at {dir:?}");
    let status = Command::new("rust-analyzer")
        .arg("scip")
        .arg(".")
        .current_dir(&dir)
        .status()
        .expect("rust-analyzer must run (rustup component add rust-analyzer)");
    assert!(status.success(), "rust-analyzer scip failed for {dir_name}");
    assert!(
        dir.join("index.scip").is_file(),
        "rust-analyzer did not write index.scip for {dir_name}"
    );
    eprintln!("wrote {}", dir.join("index.scip").display());
}

/// Generate `index.scip` for a Python corpus project via `scip-python`, run on
/// **bare sources** (no venv/dependencies). Mirrors the dev-time command in
/// `py-resolution.md`. The committed `.scip` is the hermetic artifact the gate
/// consumes; this regenerator is `#[ignore]`d.
fn generate_py(dir_name: &str, project_name: &str) {
    let dir = project("py-corpus").join(dir_name);
    assert!(dir.is_dir(), "py corpus project must exist at {dir:?}");
    let status = Command::new("npx")
        .arg("--yes")
        .arg("@sourcegraph/scip-python")
        .arg("index")
        .arg(".")
        .arg("--project-name")
        .arg(project_name)
        .arg("--project-version")
        .arg("0.0.0")
        .arg("--output")
        .arg("index.scip")
        .current_dir(&dir)
        .status()
        .expect("npx scip-python must run (PATH must include node/npx)");
    assert!(status.success(), "scip-python failed for {dir_name}");
    assert!(
        dir.join("index.scip").is_file(),
        "scip-python did not write index.scip for {dir_name}"
    );
    eprintln!("wrote {}", dir.join("index.scip").display());
}

fn generate(name: &str) {
    let dir = project(name);
    assert!(dir.is_dir(), "fixture project {name} must exist at {dir:?}");
    let opts = RunOptions {
        run_npm_install: true,
        ..RunOptions::default()
    };
    let index = run_scip(&dir, &opts).expect("scip-typescript produced an index");
    // run_scip writes <dir>/index.scip; assert it landed where the gate expects.
    assert_eq!(index, dir.join("index.scip"));
    eprintln!("wrote {}", index.display());
}

#[test]
#[ignore = "needs Node; regenerates committed index.scip"]
fn generate_methods_index() {
    generate("methods");
}

// ── Slice 13 corpus expansion (Track C1) ─────────────────────────────────────
// Four projects covering REAL patterns the small corpus missed: re-export /
// barrel chains + default imports, class inheritance + override + super, async /
// Promise chains + higher-order callbacks, and overloads + namespace imports +
// `any`-typed dynamic access (the AMBIGUOUS / unadjudicable populator).

#[test]
#[ignore = "needs Node; regenerates committed index.scip"]
fn generate_reexports_index() {
    generate("reexports");
}

#[test]
#[ignore = "needs Node; regenerates committed index.scip"]
fn generate_inheritance_index() {
    generate("inheritance");
}

#[test]
#[ignore = "needs Node; regenerates committed index.scip"]
fn generate_async_hof_index() {
    generate("async_hof");
}

#[test]
#[ignore = "needs Node; regenerates committed index.scip"]
fn generate_dynamic_index() {
    generate("dynamic");
}

// ── Track C1: Python resolution corpus (scip-python) ─────────────────────────
// Three self-contained packages indexed on bare sources. The committed `.scip`
// is the hermetic ground truth consumed by `py_accuracy_gate.rs`.

#[test]
#[ignore = "needs Node; regenerates committed index.scip via scip-python"]
fn generate_py_shop_index() {
    generate_py("shop", "shop");
}

#[test]
#[ignore = "needs Node; regenerates committed index.scip via scip-python"]
fn generate_py_geometry_index() {
    generate_py("geometry", "geometry");
}

#[test]
#[ignore = "needs Node; regenerates committed index.scip via scip-python"]
fn generate_py_pipeline_index() {
    generate_py("pipeline", "pipeline");
}

// ── Track C1: Rust resolution corpus (rust-analyzer scip) ────────────────────
// Two isolated cargo workspaces indexed with rust-analyzer. The committed
// `.scip` is the hermetic ground truth consumed by `rust_accuracy_gate.rs`.

#[test]
#[ignore = "needs rust-analyzer; regenerates committed index.scip via rust-analyzer scip"]
fn generate_rust_shapes_index() {
    generate_rust("shapes");
}

#[test]
#[ignore = "needs rust-analyzer; regenerates committed index.scip via rust-analyzer scip"]
fn generate_rust_registry_index() {
    generate_rust("registry");
}
