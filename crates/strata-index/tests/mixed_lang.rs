//! Mixed-language indexing (Slice 9 + Slice 11 + Slice 21, M2): a repo with `.ts`,
//! `.py`, `.cs`, AND `.rs` sources indexes all four planes into ONE graph.
//! Cross-language IMPORTS are NOT expected — TS, Python, C#, and Rust each link
//! within their own resolution world this slice — but all four planes' nodes and
//! intra-language edges coexist.
//!
//! Also pins the skip-dir guards: `.py` files under `__pycache__/`, `venv/`,
//! `.venv/`, `site-packages/`, `node_modules/`, `.cs` files under `bin/`, `obj/`,
//! `packages/`, `.vs/`, and `.rs` files under `target/` (the huge Cargo build dir)
//! are NEVER indexed. The decoy dirs are written into a **tempdir at runtime** (they
//! are gitignored at the repo root, so a committed fixture could not hold them),
//! then the tempdir is indexed — exercising the indexer's own pruning, not
//! gitignore.

use std::fs;
use std::path::Path;

use strata_core::{Direction, EdgeKind, NodeKind, Provenance, Uid};
use strata_index::index_repo;
use strata_store::{DuckGraphStore, GraphStore};

const FIXTURE: &str = "mixed_lang";

fn fixture_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(FIXTURE)
}

/// Copy a directory tree to `dst` (skipping any `.strata/`).
fn copy_dir_all(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        if entry.file_name() == ".strata" {
            continue;
        }
        let sp = entry.path();
        let dp = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir_all(&sp, &dp);
        } else {
            fs::copy(&sp, &dp).unwrap();
        }
    }
}

/// Write the skip-dir decoys into `root`: a `.py` under each dir the indexer must
/// prune, plus a `.pyc` (excluded by extension). Each carries a distinctive
/// symbol name so a leak is unmistakable.
fn write_skip_decoys(root: &Path) {
    let decoys = [
        ("__pycache__/cached.py", "should_not_appear_pycache"),
        (".venv/lib/vendored.py", "should_not_appear_venv"),
        ("venv/installed.py", "should_not_appear_plain_venv"),
        (
            "some_env/site-packages/dep.py",
            "should_not_appear_site_packages",
        ),
        (
            "node_modules/pkg/vendored.py",
            "should_not_appear_node_modules_py",
        ),
    ];
    for (rel, sym) in decoys {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, format!("def {sym}():\n    pass\n")).unwrap();
    }
    // A compiled artifact: excluded by the .py-only extension filter anyway.
    fs::write(
        root.join("pkg/service.pyc"),
        b"\x00compiled-bytecode-not-source\x00",
    )
    .unwrap();
}

/// Write the C# skip-dir decoys into `root`: a `.cs` under each .NET dir the
/// indexer must prune (`bin/`, `obj/`, `packages/`, `.vs/`). Each carries a
/// distinctive type name so a leak is unmistakable (the runtime-decoy guard).
fn write_cs_skip_decoys(root: &Path) {
    let decoys = [
        ("bin/Debug/Generated.cs", "ShouldNotAppearBin"),
        ("obj/Release/AssemblyInfo.cs", "ShouldNotAppearObj"),
        ("packages/Dep/Vendored.cs", "ShouldNotAppearPackages"),
        (".vs/meta/Cached.cs", "ShouldNotAppearVs"),
    ];
    for (rel, ty) in decoys {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(
            &p,
            format!("public class {ty} {{ public void M() {{ }} }}\n"),
        )
        .unwrap();
    }
}

/// Write the Rust skip-dir decoys into `root`: `.rs` files under `target/` (Cargo's
/// build-output dir — the CRITICAL prune, since a real Rust repo's `target/` holds
/// an enormous tree of compiled/dependency `.rs`). Each carries a distinctive type
/// name so a leak is unmistakable (the huge-build-dir runtime-decoy guard).
fn write_rust_skip_decoys(root: &Path) {
    let decoys = [
        (
            "target/debug/build/generated.rs",
            "ShouldNotAppearTargetDebug",
        ),
        (
            "target/release/deps/vendored.rs",
            "ShouldNotAppearTargetRelease",
        ),
    ];
    for (rel, ty) in decoys {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, format!("pub struct {ty} {{ }}\n")).unwrap();
    }
}

fn ts_uid(repo: &str, path: &str, fqn: &str) -> Uid {
    Uid::new("ts", repo, path, fqn, "")
}
fn py_uid(repo: &str, path: &str, fqn: &str) -> Uid {
    Uid::new("py", repo, path, fqn, "")
}
fn cs_uid(repo: &str, path: &str, fqn: &str) -> Uid {
    Uid::new("cs", repo, path, fqn, "")
}
fn rust_uid(repo: &str, path: &str, fqn: &str) -> Uid {
    Uid::new("rust", repo, path, fqn, "")
}

fn calls(g: &strata_core::Graph, src: &Uid) -> Vec<(Uid, Provenance, f32)> {
    g.neighbors(src, Direction::Outgoing, &[EdgeKind::Calls])
        .into_iter()
        .map(|(e, _)| (e.dst.clone(), e.provenance, e.confidence.value()))
        .collect()
}

#[test]
fn mixed_repo_indexes_both_planes_into_one_graph() {
    // Copy the committed fixture (real sources only) into a tempdir, then add the
    // skip-dir decoys so the indexer's own pruning is exercised.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    copy_dir_all(&fixture_root(), root);
    write_skip_decoys(root);
    write_cs_skip_decoys(root);
    write_rust_skip_decoys(root);
    let repo = root.file_name().unwrap().to_str().unwrap().to_string();

    let mut store = DuckGraphStore::open_in_memory().unwrap();
    let stats = index_repo(root, &mut store).unwrap();
    let g = store.load_graph().unwrap();

    // ── The TS plane is present and intact. ──
    assert_eq!(
        g.get_node(&ts_uid(&repo, "src/app.ts", "tsMain"))
            .map(|n| n.kind),
        Some(NodeKind::Function),
        "TS function tsMain must be a node"
    );
    let ts_calls = calls(&g, &ts_uid(&repo, "src/app.ts", "tsMain"));
    assert!(
        ts_calls
            .iter()
            .any(|(d, _, _)| d == &ts_uid(&repo, "src/app.ts", "tsHelper")),
        "tsMain —Calls→ tsHelper (TS plane unchanged): {ts_calls:?}"
    );

    // ── The Python plane is present in the SAME graph. ──
    assert_eq!(
        g.get_node(&py_uid(&repo, "pkg/service.py", "Service"))
            .map(|n| n.kind),
        Some(NodeKind::Class),
        "Python class Service must be a node"
    );
    assert_eq!(
        g.get_node(&py_uid(&repo, "pkg/service.py", "Service.run"))
            .map(|n| n.kind),
        Some(NodeKind::Method),
        "Python method Service.run must be a node"
    );

    // Same-module Python call: Service.run —Calls→ py_helper (Extracted 0.95).
    let run_calls = calls(&g, &py_uid(&repo, "pkg/service.py", "Service.run"));
    assert!(
        run_calls.iter().any(
            |(d, p, c)| d == &py_uid(&repo, "pkg/service.py", "py_helper")
                && *p == Provenance::Extracted
                && (*c - 0.95).abs() < 1e-6
        ),
        "Service.run —Calls→ py_helper (Extracted 0.95): {run_calls:?}"
    );

    // Cross-module Python call via relative import: use_helper —Calls→
    // service.py_helper (Inferred 0.80).
    let use_calls = calls(&g, &py_uid(&repo, "pkg/client.py", "use_helper"));
    assert!(
        use_calls.iter().any(
            |(d, p, c)| d == &py_uid(&repo, "pkg/service.py", "py_helper")
                && *p == Provenance::Inferred
                && (*c - 0.80).abs() < 1e-6
        ),
        "use_helper —Calls→ service.py_helper via relative import (Inferred 0.80): {use_calls:?}"
    );

    // ── The C# plane is present in the SAME graph. ──
    assert_eq!(
        g.get_node(&cs_uid(&repo, "cs/Service.cs", "Mixed.Services.CsService"))
            .map(|n| n.kind),
        Some(NodeKind::Class),
        "C# class Mixed.Services.CsService must be a node (namespace-qualified fqn)"
    );
    assert_eq!(
        g.get_node(&cs_uid(
            &repo,
            "cs/Service.cs",
            "Mixed.Services.CsService.Run"
        ))
        .map(|n| n.kind),
        Some(NodeKind::Method),
        "C# method CsService.Run must be a node"
    );
    // Same-file C# call: CsService.Run —Calls→ CsHelper (Extracted 0.95).
    let cs_run_calls = calls(
        &g,
        &cs_uid(&repo, "cs/Service.cs", "Mixed.Services.CsService.Run"),
    );
    assert!(
        cs_run_calls.iter().any(|(d, p, c)| d
            == &cs_uid(&repo, "cs/Service.cs", "Mixed.Services.CsService.CsHelper")
            && *p == Provenance::Extracted
            && (*c - 0.95).abs() < 1e-6),
        "CsService.Run —Calls→ CsHelper (Extracted 0.95): {cs_run_calls:?}"
    );
    // this-method C# call: CsService.Run —Calls→ Compute via this (Inferred 0.80).
    assert!(
        cs_run_calls.iter().any(|(d, p, c)| d
            == &cs_uid(&repo, "cs/Service.cs", "Mixed.Services.CsService.Compute")
            && *p == Provenance::Inferred
            && (*c - 0.80).abs() < 1e-6),
        "CsService.Run —Calls→ Compute via this (Inferred 0.80): {cs_run_calls:?}"
    );

    // ── The Rust plane is present in the SAME graph. ──
    assert_eq!(
        g.get_node(&rust_uid(&repo, "rust/service.rs", "services::RustService"))
            .map(|n| n.kind),
        Some(NodeKind::Class),
        "Rust struct services::RustService must be a node (module-qualified fqn, struct→Class)"
    );
    assert_eq!(
        g.get_node(&rust_uid(
            &repo,
            "rust/service.rs",
            "services::RustService::run"
        ))
        .map(|n| n.kind),
        Some(NodeKind::Method),
        "Rust method RustService::run must be a node"
    );
    // Same-file Rust call: RustService::run —Calls→ rust_helper (Extracted 0.95).
    let rust_run_calls = calls(
        &g,
        &rust_uid(&repo, "rust/service.rs", "services::RustService::run"),
    );
    assert!(
        rust_run_calls.iter().any(|(d, p, c)| d
            == &rust_uid(&repo, "rust/service.rs", "services::rust_helper")
            && *p == Provenance::Extracted
            && (*c - 0.95).abs() < 1e-6),
        "RustService::run —Calls→ rust_helper (Extracted 0.95): {rust_run_calls:?}"
    );
    // self-method Rust call: RustService::run —Calls→ compute via self (Inferred 0.80).
    assert!(
        rust_run_calls.iter().any(|(d, p, c)| d
            == &rust_uid(&repo, "rust/service.rs", "services::RustService::compute")
            && *p == Provenance::Inferred
            && (*c - 0.80).abs() < 1e-6),
        "RustService::run —Calls→ compute via self (Inferred 0.80): {rust_run_calls:?}"
    );
    // The `println!` macro must NEVER become a call edge or a symbol node (the
    // load-bearing macros-are-not-calls honesty pin, exercised through the indexer).
    assert!(
        !rust_run_calls
            .iter()
            .any(|(d, _, _)| d.as_str().ends_with("println")),
        "the println! macro must not produce a call edge: {rust_run_calls:?}"
    );
    assert!(
        g.get_node(&rust_uid(&repo, "rust/service.rs", "services::println"))
            .is_none(),
        "a macro is never a symbol node"
    );

    // ── No cross-language edges (each of ts/py/cs/rust links in its own world). ──
    // Any edge whose endpoints carry different language tags would be a leak.
    for n in g.nodes() {
        let src_lang = n.uid.as_str().split('|').next().unwrap_or("");
        if !matches!(src_lang, "ts" | "py" | "cs" | "rust") {
            continue;
        }
        for (e, dst) in g.neighbors(&n.uid, Direction::Outgoing, &[]) {
            let dst_lang = dst.uid.as_str().split('|').next().unwrap_or("");
            if matches!(dst_lang, "ts" | "py" | "cs" | "rust") && dst_lang != src_lang {
                panic!(
                    "no cross-language edge expected: {} -{:?}-> {}",
                    e.src.as_str(),
                    e.kind,
                    e.dst.as_str()
                );
            }
        }
    }

    // ── Skip-dir guard: NO decoy symbol (Python, C#, OR Rust) is ever indexed. ──
    let forbidden = [
        "should_not_appear_pycache",
        "should_not_appear_venv",
        "should_not_appear_plain_venv",
        "should_not_appear_site_packages",
        "should_not_appear_node_modules_py",
        // C# runtime/build-output decoys.
        "ShouldNotAppearBin",
        "ShouldNotAppearObj",
        "ShouldNotAppearPackages",
        "ShouldNotAppearVs",
        // Rust build-output decoys (the critical `target/` prune).
        "ShouldNotAppearTargetDebug",
        "ShouldNotAppearTargetRelease",
    ];
    for n in g.nodes() {
        for bad in &forbidden {
            assert_ne!(
                &n.name,
                bad,
                "a skip-dir symbol leaked into the graph: {} ({})",
                bad,
                n.uid.as_str()
            );
        }
        assert!(
            !n.path.contains("__pycache__")
                && !n.path.contains("/venv/")
                && !n.path.starts_with("venv/")
                && !n.path.contains("/.venv/")
                && !n.path.contains("site-packages")
                && !n.path.contains("node_modules")
                // C# build-output / VS dirs (component-anchored so a legitimate
                // file like `cabinet.cs` is never excluded by substring).
                && !n.path.starts_with("bin/")
                && !n.path.contains("/bin/")
                && !n.path.starts_with("obj/")
                && !n.path.contains("/obj/")
                && !n.path.starts_with("packages/")
                && !n.path.contains("/packages/")
                && !n.path.contains("/.vs/")
                && !n.path.starts_with(".vs/")
                // Rust build-output dir (component-anchored so a legitimate file
                // like `targeting.rs` is never excluded by substring).
                && !n.path.starts_with("target/")
                && !n.path.contains("/target/"),
            "a node originated from a skip dir: {}",
            n.path
        );
    }

    // files_indexed counts the real sources only: app.ts + service.py + client.py
    // + cs/Service.cs + rust/service.rs.
    assert_eq!(
        stats.files_indexed, 5,
        "exactly the five real sources indexed (1 ts + 2 py + 1 cs + 1 rust)"
    );

    // The file-hash entries exist for every plane (so incremental reuse applies).
    let hashes = store.load_file_hashes().unwrap();
    assert!(hashes.contains_key("pkg/service.py"));
    assert!(hashes.contains_key("pkg/client.py"));
    assert!(hashes.contains_key("src/app.ts"));
    assert!(hashes.contains_key("cs/Service.cs"));
    assert!(hashes.contains_key("rust/service.rs"));
    assert!(!hashes.keys().any(|k| k.contains("__pycache__")));
    assert!(!hashes
        .keys()
        .any(|k| k.starts_with("bin/") || k.starts_with("obj/")));
    // The `target/` build dir was pruned (its decoy .rs never reached the cache).
    assert!(
        !hashes.keys().any(|k| k.starts_with("target/")),
        "target/ must be pruned — no .rs under it should be hashed/indexed"
    );
}
