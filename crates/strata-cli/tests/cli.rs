//! End-to-end integration tests for the `strata` binary.
//!
//! These run the *built binary* (`CARGO_BIN_EXE_strata`) against fixture repos
//! in temp dirs — proving the whole pipeline (index → store → load → traverse →
//! print) works through the real command-line surface, including the
//! cross-file blast-radius proof and the friendly missing-index error.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Absolute path to the binary cargo built for this test run.
fn strata_bin() -> &'static str {
    env!("CARGO_BIN_EXE_strata")
}

/// `a.ts` imports and calls `foo` from `b.ts`; `node_modules` is gitignored.
fn write_fixture(root: &Path) {
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join("node_modules")).unwrap();
    std::fs::write(root.join(".gitignore"), "node_modules/\n").unwrap();
    std::fs::write(
        root.join("src/a.ts"),
        "import { foo } from \"./b\";\nexport function run() { foo(); }\n",
    )
    .unwrap();
    std::fs::write(root.join("src/b.ts"), "export function foo() {}\n").unwrap();
    std::fs::write(
        root.join("node_modules/dep.ts"),
        "export function shouldNotAppear() {}\n",
    )
    .unwrap();
}

#[test]
fn index_reports_counts_and_second_run_reuses() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_fixture(root);
    let db = root.join(".strata/graph.duckdb");

    // First index: non-zero nodes/edges, nothing reused.
    let out = Command::new(strata_bin())
        .args([
            "index",
            root.to_str().unwrap(),
            "--db",
            db.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "index should exit 0: {:?}", out);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("files indexed: 2"), "got: {stdout}");
    // Nodes and edges must be non-zero (the cross-file call yields an edge).
    assert!(
        !stdout.contains("nodes:         0"),
        "expected non-zero nodes; got: {stdout}"
    );
    assert!(
        !stdout.contains("edges:         0"),
        "expected non-zero edges; got: {stdout}"
    );
    assert!(
        stdout.contains("files reused:  0"),
        "first run reuses nothing: {stdout}"
    );

    // Second index over the unchanged repo: files are reused from the parse cache.
    let out2 = Command::new(strata_bin())
        .args([
            "index",
            root.to_str().unwrap(),
            "--db",
            db.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(out2.status.success());
    let stdout2 = String::from_utf8_lossy(&out2.stdout);
    assert!(
        stdout2.contains("files reused:  2"),
        "second run must reuse both files; got: {stdout2}"
    );
}

#[test]
fn index_prints_infra_failed_line_for_a_broken_template() {
    // The dogfood guarantee at the CLI surface: a template that won't parse must
    // be printed (path + error) and counted, never silently skipped.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("infra")).unwrap();
    // Looks like a CFN template (Resources + AWS::) but a TAB makes it invalid.
    fs::write(
        root.join("infra/broken.yaml"),
        "Resources:\n\tBrokenFn:\n\t\tType: AWS::Serverless::Function\n",
    )
    .unwrap();
    let db = root.join(".strata/graph.duckdb");

    let out = Command::new(strata_bin())
        .args([
            "index",
            root.to_str().unwrap(),
            "--db",
            db.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "index should still exit 0: {:?}", out);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("[infra] FAILED") && stdout.contains("infra/broken.yaml"),
        "a broken template must be printed with its path; got: {stdout}"
    );
    assert!(
        stdout.contains("1 failed"),
        "the infra summary must report the failure count; got: {stdout}"
    );
}

#[test]
fn impact_finds_cross_file_caller_through_binary() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_fixture(root);
    let db = root.join(".strata/graph.duckdb");

    // Build the index first.
    let idx = Command::new(strata_bin())
        .args([
            "index",
            root.to_str().unwrap(),
            "--db",
            db.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(idx.status.success());

    // impact foo → run (defined in src/a.ts) is a cross-file caller.
    let out = Command::new(strata_bin())
        .args(["impact", "foo", "--db", db.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success(), "impact should exit 0: {:?}", out);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("run"),
        "impact foo must list the cross-file caller `run`; got: {stdout}"
    );
    assert!(
        stdout.contains("src/a.ts"),
        "impact foo must show the caller's file src/a.ts; got: {stdout}"
    );
}

#[test]
fn context_through_binary_shows_callers() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_fixture(root);
    let db = root.join(".strata/graph.duckdb");
    Command::new(strata_bin())
        .args([
            "index",
            root.to_str().unwrap(),
            "--db",
            db.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    let out = Command::new(strata_bin())
        .args(["context", "foo", "--db", db.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Context for foo"), "got: {stdout}");
    assert!(stdout.contains("callers"), "got: {stdout}");
    // `run` calls foo, so it is a caller of foo.
    assert!(
        stdout.contains("run"),
        "context foo should list caller run; got: {stdout}"
    );
    // The contract-plane buckets are always present (even for a code symbol with
    // none) so a dead schema field honestly reads `producers (0) / consumers (0)`.
    assert!(
        stdout.contains("producers ("),
        "context must always print the producers bucket; got: {stdout}"
    );
    assert!(
        stdout.contains("consumers ("),
        "context must always print the consumers bucket; got: {stdout}"
    );
}

#[test]
fn explain_through_binary_prints_the_evidence_chain() {
    // The Track E1 proof at the CLI surface: `run` (in src/a.ts) depends on `foo`
    // (in src/b.ts) via a CALLS edge, so `strata explain foo run` must render the
    // evidence chain — the header with the will-break verdict and a hop naming the
    // edge kind, provenance, and running confidence.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_fixture(root);
    let db = root.join(".strata/graph.duckdb");

    let idx = Command::new(strata_bin())
        .args([
            "index",
            root.to_str().unwrap(),
            "--db",
            db.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(idx.status.success());

    let out = Command::new(strata_bin())
        .args(["explain", "foo", "run", "--db", db.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success(), "explain should exit 0: {:?}", out);
    let stdout = String::from_utf8_lossy(&out.stdout);
    // The header names both ends and carries the conf + verdict.
    assert!(
        stdout.contains("Why foo affects run"),
        "explain header must name target/affected; got: {stdout}"
    );
    assert!(
        stdout.contains("conf ")
            && (stdout.contains("WILL BREAK") || stdout.contains("may affect")),
        "explain header must carry the overall conf + a verdict; got: {stdout}"
    );
    // The hop names the traversed edge kind and the running confidence.
    assert!(
        stdout.contains("CALLS") && stdout.contains("running "),
        "the evidence chain must render the CALLS hop with a running confidence; got: {stdout}"
    );
    // It explains `run`'s presence, so the affected node appears in a hop target.
    assert!(
        stdout.contains("foo") && stdout.contains("run"),
        "both endpoints appear in the chain; got: {stdout}"
    );
}

#[test]
fn explain_through_binary_is_honest_when_not_in_blast_radius() {
    // `foo` does not depend on `run` (the edge is run→foo), so explain in the
    // OTHER direction must print the honest "not in blast radius" line, not an
    // empty or misleading success.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_fixture(root);
    let db = root.join(".strata/graph.duckdb");
    Command::new(strata_bin())
        .args([
            "index",
            root.to_str().unwrap(),
            "--db",
            db.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    let out = Command::new(strata_bin())
        .args(["explain", "run", "foo", "--db", db.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "explain should still exit 0: {:?}",
        out
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("is not in run's blast radius (nothing to explain)"),
        "explain must be honest about an unreachable node; got: {stdout}"
    );
}

#[test]
fn query_through_binary_finds_symbol() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_fixture(root);
    let db = root.join(".strata/graph.duckdb");
    Command::new(strata_bin())
        .args([
            "index",
            root.to_str().unwrap(),
            "--db",
            db.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    let out = Command::new(strata_bin())
        .args(["query", "foo", "--db", db.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("foo"),
        "query foo should find foo; got: {stdout}"
    );
}

#[test]
fn missing_index_is_friendly_error_nonzero_exit() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("does-not-exist.duckdb");

    let out = Command::new(strata_bin())
        .args(["impact", "something", "--db", missing.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "missing index must exit non-zero; status: {:?}",
        out.status
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no index found"),
        "expected friendly 'no index found' message; got stderr: {stderr}"
    );
}

#[test]
fn help_lists_all_subcommands() {
    let out = Command::new(strata_bin()).arg("--help").output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    for sub in [
        "index",
        "impact",
        "context",
        "query",
        "mcp",
        "detect-changes",
        "rename",
    ] {
        assert!(
            stdout.contains(sub),
            "--help must list `{sub}`; got: {stdout}"
        );
    }
}

// ══ Slice 12: `strata detect-changes` through the binary ═════════════════════════

/// Run `git <args>` at `dir`, asserting success.
fn git_at(dir: &Path, args: &[&str]) {
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

/// `strata detect-changes` reports a modified symbol + its blast radius + a risk
/// line, and exits 0 (it reports, it does not gate). Drives the built binary over
/// a real tempdir git repo with a default-location index.
#[test]
fn detect_changes_reports_modified_symbol_and_exits_zero() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git_at(root, &["init", "-q"]);
    git_at(root, &["config", "user.email", "t@t"]);
    git_at(root, &["config", "user.name", "t"]);
    git_at(root, &["config", "commit.gpgsign", "false"]);
    // `helper` is called by `caller` (same file → resolves regardless of mode).
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("src/a.ts"),
        "export function helper() { return 1; }\nexport function caller() { return helper(); }\n",
    )
    .unwrap();
    git_at(root, &["add", "-A"]);
    git_at(
        root,
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "baseline",
        ],
    );

    // Index at the default location (<root>/.strata/graph.duckdb) so the repo root
    // is derived from --db's grandparent.
    let db = root.join(".strata/graph.duckdb");
    let idx = Command::new(strata_bin())
        .args([
            "index",
            root.to_str().unwrap(),
            "--db",
            db.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(idx.status.success(), "index must succeed: {idx:?}");

    // Modify helper's body in the working tree.
    fs::write(
        root.join("src/a.ts"),
        "export function helper() { return 2; }\nexport function caller() { return helper(); }\n",
    )
    .unwrap();

    let out = Command::new(strata_bin())
        .args([
            "detect-changes",
            "--db",
            db.to_str().unwrap(),
            "--repo",
            root.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "detect-changes must exit 0 (reports, never gates); stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("code symbols") && stdout.contains("helper"),
        "must report helper as a changed code symbol; got: {stdout}"
    );
    assert!(
        stdout.contains("caller"),
        "the blast radius must include the caller; got: {stdout}"
    );
    assert!(
        stdout.contains("Risk:"),
        "a risk line must be printed; got: {stdout}"
    );
}

/// `strata detect-changes` on a non-git directory is a clear error (never a
/// silent empty report) and a non-zero exit.
#[test]
fn detect_changes_on_non_git_dir_is_a_clear_error() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // A plain (never `git init`ed) repo with an index built from it.
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/a.ts"), "export function f() {}\n").unwrap();
    let db = root.join(".strata/graph.duckdb");
    let idx = Command::new(strata_bin())
        .args([
            "index",
            root.to_str().unwrap(),
            "--db",
            db.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(idx.status.success());

    let out = Command::new(strata_bin())
        .args([
            "detect-changes",
            "--db",
            db.to_str().unwrap(),
            "--repo",
            root.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "a non-git repo must be a non-zero exit, not a silent empty report"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("not a git repository"),
        "the error must name the missing git repo; got: {stderr}"
    );
}

// ══ Test 8: CLI workspace smoke test ════════════════════════════════════════════

/// Fixture path for the committed estate (repo-a + repo-b + manifest).
fn estate_fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent() // crates/
        .unwrap()
        .join("strata-index/tests/fixtures/estate")
}

/// Copy a directory tree to `dst`, skipping `.strata/` subdirs.
fn copy_dir_all(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        if name == ".strata" {
            continue;
        }
        let sp = entry.path();
        let dp = dst.join(&name);
        if entry.file_type().unwrap().is_dir() {
            copy_dir_all(&sp, &dp);
        } else {
            fs::copy(&sp, &dp).unwrap();
        }
    }
}

/// Test 8a: `strata index --workspace <manifest>` exits 0 and prints both repos.
#[test]
fn workspace_index_exits_zero_and_lists_both_repos() {
    let tmp = tempfile::tempdir().unwrap();
    copy_dir_all(&estate_fixture_dir(), tmp.path());
    let manifest = tmp.path().join("strata.workspace.toml");

    let out = Command::new(strata_bin())
        .args([
            "index",
            "--workspace",
            manifest.to_str().unwrap(),
            "--resolve",
            "off",
        ])
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "strata index --workspace must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("repo-a"),
        "output must mention repo-a; got: {stdout}"
    );
    assert!(
        stdout.contains("repo-b"),
        "output must mention repo-b; got: {stdout}"
    );
}

/// Fixture path for the committed cross-repo GraphQL estate (producer in
/// repo-schema, gql consumer in repo-app, linked across repos by `link_estate`).
fn crossrepo_graphql_fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent() // crates/
        .unwrap()
        .join("strata-index/tests/fixtures/crossrepo_graphql")
}

/// Test 8c: `strata impact --workspace … --no-contracts` shrinks the affected set.
///
/// On the cross-repo GraphQL estate, `impact(getUser)` (the resolver in
/// repo-schema) reaches the gql consumer `loadUserProfile` in repo-app **only**
/// via the contract plane. So the default run lists the consumer, and
/// `--no-contracts` (code-only blast radius) drops it — the additive switch,
/// proven end-to-end through the binary.
#[test]
fn impact_no_contracts_shrinks_affected_set_through_binary() {
    let tmp = tempfile::tempdir().unwrap();
    copy_dir_all(&crossrepo_graphql_fixture_dir(), tmp.path());
    let manifest = tmp.path().join("strata.workspace.toml");

    // Index the estate first so each repo has its own .strata/graph.duckdb.
    let idx = Command::new(strata_bin())
        .args([
            "index",
            "--workspace",
            manifest.to_str().unwrap(),
            "--resolve",
            "off",
        ])
        .output()
        .unwrap();
    assert!(
        idx.status.success(),
        "estate index must succeed; stderr: {}",
        String::from_utf8_lossy(&idx.stderr)
    );

    // Default (contract-aware): the cross-repo gql consumer IS in the blast radius.
    let with = Command::new(strata_bin())
        .args([
            "impact",
            "getUser",
            "--workspace",
            manifest.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        with.status.success(),
        "impact --workspace must exit 0; stderr: {}",
        String::from_utf8_lossy(&with.stderr)
    );
    let with_out = String::from_utf8_lossy(&with.stdout);
    assert!(
        with_out.contains("loadUserProfile"),
        "default contract-aware impact must surface the cross-repo gql consumer; got: {with_out}"
    );

    // --no-contracts (code-only): the cross-plane consumer is NOT reached.
    let without = Command::new(strata_bin())
        .args([
            "impact",
            "getUser",
            "--workspace",
            manifest.to_str().unwrap(),
            "--no-contracts",
        ])
        .output()
        .unwrap();
    assert!(
        without.status.success(),
        "impact --no-contracts must exit 0; stderr: {}",
        String::from_utf8_lossy(&without.stderr)
    );
    let without_out = String::from_utf8_lossy(&without.stdout);
    assert!(
        !without_out.contains("loadUserProfile"),
        "--no-contracts must drop the contract-plane consumer; got: {without_out}"
    );
}

// ══ Slice 9: the Python code plane works through the binary ═════════════════════

/// A small Python repo: `pkg/service.py` defines `make_user` and calls it from
/// `build`; `pkg/client.py` imports `make_user` and calls it across modules. This
/// exercises both same-module (Extracted) and import-matched (Inferred) Python
/// links — all the way through the real CLI.
fn write_python_fixture(root: &Path) {
    std::fs::create_dir_all(root.join("pkg")).unwrap();
    std::fs::write(
        root.join("pkg/service.py"),
        "def make_user():\n    return {\"id\": 1}\n\n\ndef build():\n    return make_user()\n",
    )
    .unwrap();
    std::fs::write(
        root.join("pkg/client.py"),
        "from .service import make_user\n\n\ndef load():\n    return make_user()\n",
    )
    .unwrap();
}

/// `index` + `query`/`context`/`impact` all work on a Python symbol through the
/// built binary (the NodeKinds are the same Function/Module as TS, so the whole
/// CLI surface is unchanged — this just proves it on Python).
#[test]
fn python_symbol_works_through_binary() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_python_fixture(root);
    let db = root.join(".strata/graph.duckdb");

    // Index the Python repo.
    let idx = Command::new(strata_bin())
        .args([
            "index",
            root.to_str().unwrap(),
            "--db",
            db.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        idx.status.success(),
        "indexing a Python repo must exit 0; stderr: {}",
        String::from_utf8_lossy(&idx.stderr)
    );
    let idx_out = String::from_utf8_lossy(&idx.stdout);
    assert!(
        idx_out.contains("files indexed: 2"),
        "both Python files must be indexed; got: {idx_out}"
    );

    // query: finds the Python symbol.
    let q = Command::new(strata_bin())
        .args(["query", "make_user", "--db", db.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(q.status.success());
    let q_out = String::from_utf8_lossy(&q.stdout);
    assert!(
        q_out.contains("make_user") && q_out.contains("pkg/service.py"),
        "query must find the Python symbol in its file; got: {q_out}"
    );

    // context: make_user's callers include `build` (same module) and `load`
    // (cross-module via the relative import).
    let c = Command::new(strata_bin())
        .args(["context", "make_user", "--db", db.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(c.status.success());
    let c_out = String::from_utf8_lossy(&c.stdout);
    assert!(
        c_out.contains("Context for make_user") && c_out.contains("callers"),
        "context must print the Python symbol's caller bucket; got: {c_out}"
    );
    assert!(
        c_out.contains("build"),
        "context make_user must list the same-module caller `build`; got: {c_out}"
    );

    // impact: the reverse blast radius of make_user reaches its callers.
    let i = Command::new(strata_bin())
        .args(["impact", "make_user", "--db", db.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(i.status.success(), "impact should exit 0: {i:?}");
    let i_out = String::from_utf8_lossy(&i.stdout);
    assert!(
        i_out.contains("build") || i_out.contains("load"),
        "impact make_user must surface a Python caller; got: {i_out}"
    );
}

// ══ Slice 11: the C# code plane works through the binary ════════════════════════

/// A small C# repo: `src/Service.cs` defines `MakeUser` and calls it from `Build`
/// (same-file Extracted), and `src/Client.cs` calls `MakeUser` cross-file (the
/// unique repo-wide name → Inferred). Exercises the C# plane all the way through
/// the real CLI; the NodeKinds are the same Class/Method/Function as TS/Python, so
/// the CLI surface is unchanged.
fn write_csharp_fixture(root: &Path) {
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src/Service.cs"),
        concat!(
            "namespace App\n",
            "{\n",
            "    public class Service\n",
            "    {\n",
            "        public int MakeUser() { return 1; }\n",
            "        public int Build() { return MakeUser(); }\n",
            "    }\n",
            "}\n",
        ),
    )
    .unwrap();
    std::fs::write(
        root.join("src/Client.cs"),
        concat!(
            "namespace App\n",
            "{\n",
            "    public class Client\n",
            "    {\n",
            "        public int Load() { return MakeUser(); }\n",
            "    }\n",
            "}\n",
        ),
    )
    .unwrap();
}

/// `index` + `query`/`context`/`impact` all work on a C# symbol through the built
/// binary.
#[test]
fn csharp_symbol_works_through_binary() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_csharp_fixture(root);
    let db = root.join(".strata/graph.duckdb");

    // Index the C# repo.
    let idx = Command::new(strata_bin())
        .args([
            "index",
            root.to_str().unwrap(),
            "--db",
            db.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        idx.status.success(),
        "indexing a C# repo must exit 0; stderr: {}",
        String::from_utf8_lossy(&idx.stderr)
    );
    let idx_out = String::from_utf8_lossy(&idx.stdout);
    assert!(
        idx_out.contains("files indexed: 2"),
        "both C# files must be indexed; got: {idx_out}"
    );

    // query: finds the C# symbol in its file.
    let q = Command::new(strata_bin())
        .args(["query", "MakeUser", "--db", db.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(q.status.success());
    let q_out = String::from_utf8_lossy(&q.stdout);
    assert!(
        q_out.contains("MakeUser") && q_out.contains("src/Service.cs"),
        "query must find the C# symbol in its file; got: {q_out}"
    );

    // context: MakeUser's callers include `Build` (same file) and `Load`
    // (cross-file via the unique repo-wide name).
    let c = Command::new(strata_bin())
        .args(["context", "MakeUser", "--db", db.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(c.status.success());
    let c_out = String::from_utf8_lossy(&c.stdout);
    assert!(
        c_out.contains("Context for MakeUser") && c_out.contains("callers"),
        "context must print the C# symbol's caller bucket; got: {c_out}"
    );
    assert!(
        c_out.contains("Build"),
        "context MakeUser must list the same-file caller `Build`; got: {c_out}"
    );

    // impact: the reverse blast radius of MakeUser reaches its callers.
    let i = Command::new(strata_bin())
        .args(["impact", "MakeUser", "--db", db.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(i.status.success(), "impact should exit 0: {i:?}");
    let i_out = String::from_utf8_lossy(&i.stdout);
    assert!(
        i_out.contains("Build") || i_out.contains("Load"),
        "impact MakeUser must surface a C# caller; got: {i_out}"
    );
}

// ══ Slice 21: the Rust code plane works through the binary ═══════════════════════

/// A small Rust repo: `src/service.rs` defines `make_user` and calls it from
/// `build` (same-file Extracted), and `src/client.rs` calls `make_user` cross-file
/// (the unique repo-wide name → Inferred). Exercises the Rust plane all the way
/// through the real CLI; the NodeKinds are the same Function/Module as TS/Python/C#,
/// so the CLI surface is unchanged.
fn write_rust_fixture(root: &Path) {
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src/service.rs"),
        concat!(
            "pub fn make_user() -> i32 {\n",
            "    1\n",
            "}\n",
            "pub fn build() -> i32 {\n",
            "    make_user()\n",
            "}\n",
        ),
    )
    .unwrap();
    std::fs::write(
        root.join("src/client.rs"),
        concat!("pub fn load() -> i32 {\n", "    make_user()\n", "}\n"),
    )
    .unwrap();
}

/// `index` + `query`/`context`/`impact` all work on a Rust symbol through the built
/// binary.
#[test]
fn rust_symbol_works_through_binary() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_rust_fixture(root);
    let db = root.join(".strata/graph.duckdb");

    // Index the Rust repo.
    let idx = Command::new(strata_bin())
        .args([
            "index",
            root.to_str().unwrap(),
            "--db",
            db.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        idx.status.success(),
        "indexing a Rust repo must exit 0; stderr: {}",
        String::from_utf8_lossy(&idx.stderr)
    );
    let idx_out = String::from_utf8_lossy(&idx.stdout);
    assert!(
        idx_out.contains("files indexed: 2"),
        "both Rust files must be indexed; got: {idx_out}"
    );

    // query: finds the Rust symbol in its file.
    let q = Command::new(strata_bin())
        .args(["query", "make_user", "--db", db.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(q.status.success());
    let q_out = String::from_utf8_lossy(&q.stdout);
    assert!(
        q_out.contains("make_user") && q_out.contains("src/service.rs"),
        "query must find the Rust symbol in its file; got: {q_out}"
    );

    // context: make_user's callers include `build` (same file) and `load`
    // (cross-file via the unique repo-wide name).
    let c = Command::new(strata_bin())
        .args(["context", "make_user", "--db", db.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(c.status.success());
    let c_out = String::from_utf8_lossy(&c.stdout);
    assert!(
        c_out.contains("Context for make_user") && c_out.contains("callers"),
        "context must print the Rust symbol's caller bucket; got: {c_out}"
    );
    assert!(
        c_out.contains("build"),
        "context make_user must list the same-file caller `build`; got: {c_out}"
    );

    // impact: the reverse blast radius of make_user reaches its callers.
    let i = Command::new(strata_bin())
        .args(["impact", "make_user", "--db", db.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(i.status.success(), "impact should exit 0: {i:?}");
    let i_out = String::from_utf8_lossy(&i.stdout);
    assert!(
        i_out.contains("build") || i_out.contains("load"),
        "impact make_user must surface a Rust caller; got: {i_out}"
    );
}

/// Test 8b: `strata query <name> --workspace <manifest>` finds a symbol from a repo.
#[test]
fn workspace_query_finds_symbol_from_repo() {
    let tmp = tempfile::tempdir().unwrap();
    copy_dir_all(&estate_fixture_dir(), tmp.path());
    let manifest = tmp.path().join("strata.workspace.toml");

    // Index the estate first.
    let idx = Command::new(strata_bin())
        .args([
            "index",
            "--workspace",
            manifest.to_str().unwrap(),
            "--resolve",
            "off",
        ])
        .output()
        .unwrap();
    assert!(
        idx.status.success(),
        "index must succeed; stderr: {}",
        String::from_utf8_lossy(&idx.stderr)
    );

    // Query for "compute" — defined in repo-b.
    let out = Command::new(strata_bin())
        .args([
            "query",
            "compute",
            "--workspace",
            manifest.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "strata query --workspace must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("compute"),
        "query must find 'compute' from repo-b; got: {stdout}"
    );
}

/// The engine id is visible on every CLI surface a staleness question would
/// reach: `--version` and the index summary. (Dogfood: a stale running binary
/// silently reindexed with an older engine — the id makes skew visible.)
#[test]
fn version_and_index_summary_carry_the_engine_id() {
    let out = Command::new(strata_bin())
        .arg("--version")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains('(') && stdout.contains(')'),
        "--version must carry the engine id: {stdout}"
    );

    let dir = tempfile::tempdir().unwrap();
    write_fixture(dir.path());
    let db = dir.path().join(".strata/graph.duckdb");
    let out = Command::new(strata_bin())
        .args([
            "index",
            dir.path().to_str().unwrap(),
            "--db",
            db.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("engine:        "),
        "index summary must carry the engine id: {stdout}"
    );
}

/// `strata rename` is dry-run by default (lists edits, writes nothing) and writes
/// only with `--apply`. Drives the built binary over an indexed tempdir repo.
#[test]
fn rename_dry_run_then_apply_through_binary() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("src/a.ts"),
        "export function helper() { return 1; }\nexport function caller() { return helper(); }\n",
    )
    .unwrap();
    let db = root.join(".strata/graph.duckdb");
    let idx = Command::new(strata_bin())
        .args([
            "index",
            root.to_str().unwrap(),
            "--db",
            db.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(idx.status.success(), "index must succeed: {idx:?}");

    // Dry run (default): lists edits, file unchanged.
    let dry = Command::new(strata_bin())
        .args([
            "rename",
            "helper",
            "assist",
            "--db",
            db.to_str().unwrap(),
            "--repo",
            root.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(dry.status.success(), "dry-run rename must exit 0: {dry:?}");
    let dry_out = String::from_utf8_lossy(&dry.stdout);
    assert!(
        dry_out.contains("dry run") && dry_out.contains("edit(s)"),
        "dry run must list edits and say so; got: {dry_out}"
    );
    assert!(
        fs::read_to_string(root.join("src/a.ts"))
            .unwrap()
            .contains("function helper()"),
        "dry run must NOT write"
    );

    // Apply: writes the edits.
    let applied = Command::new(strata_bin())
        .args([
            "rename",
            "helper",
            "assist",
            "--apply",
            "--db",
            db.to_str().unwrap(),
            "--repo",
            root.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        applied.status.success(),
        "apply rename must exit 0: {applied:?}"
    );
    let after = fs::read_to_string(root.join("src/a.ts")).unwrap();
    assert!(
        after.contains("function assist()") && !after.contains("helper"),
        "apply must rewrite helper → assist: {after}"
    );
}
