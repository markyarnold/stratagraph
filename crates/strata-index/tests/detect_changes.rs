//! `detect_changes` over real tempdir git repositories.
//!
//! Each test builds a git repo (init → commit a baseline → index it → mutate
//! the working tree / index), then runs [`detect_changes`] against the loaded
//! graph and asserts the changed-symbol set, the aggregated blast radius, and
//! the risk verdict. Git is driven through the real binary (the engine shells
//! out); commits use `-c user.email/-c user.name` so no global git identity is
//! required.

use std::path::Path;
use std::process::Command;

use strata_core::Graph;
use strata_index::{
    detect_changes, index_repo, ChangeKind, ChangeScope, ContractChange, Plane, RiskLevel,
};
use strata_store::{DuckGraphStore, GraphStore};
use tempfile::TempDir;

/// Run `git <args>` at `dir`, asserting success (with the captured stderr on
/// failure so a broken fixture is debuggable).
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
    // Avoid signing / hooks interfering in CI.
    git(dir, &["config", "commit.gpgsign", "false"]);
}

/// Stage everything and commit with the identity-free committer.
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

/// Index the repo at `dir` into an in-memory DuckDB store and return the graph.
fn index(dir: &Path) -> Graph {
    let mut store = DuckGraphStore::open_in_memory().expect("open store");
    index_repo(dir, &mut store).expect("index repo");
    store.load_graph().expect("load graph")
}

/// Write `content` to `<dir>/<rel>`, creating parent dirs.
fn write(dir: &Path, rel: &str, content: &str) {
    let path = dir.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir");
    }
    std::fs::write(path, content).expect("write file");
}

// ── modify a function body → Modified + its impact ──────────────────────────────

#[test]
fn modified_fn_body_is_modified_and_impacts_callers() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    git_init(dir);
    // `helper` is called by `caller` (same file, so the call resolves
    // unambiguously regardless of cross-file resolution mode).
    write(
        dir,
        "src/a.ts",
        "export function helper() { return 1; }\nexport function caller() { return helper(); }\n",
    );
    git_commit_all(dir, "baseline");
    let graph = index(dir);

    // Change helper's body (working tree).
    write(
        dir,
        "src/a.ts",
        "export function helper() { return 2; }\nexport function caller() { return helper(); }\n",
    );

    let report = detect_changes(&graph, dir, ChangeScope::Working).expect("detect");

    // `helper` is a Modified code symbol.
    let helper = report
        .symbols
        .iter()
        .find(|s| s.key == "helper")
        .unwrap_or_else(|| panic!("helper must be a changed symbol: {:?}", report.symbols));
    assert_eq!(helper.change, ChangeKind::Modified);
    assert_eq!(helper.plane, Plane::Code);

    // The blast radius includes `caller` (it calls helper).
    assert!(
        report.affected.iter().any(|a| a.name == "caller"),
        "impact of the modified helper must include caller: {:?}",
        report.affected
    );
}

// ── delete an exported fn with callers → Removed + affected + risk reasons ───────

#[test]
fn deleted_exported_fn_with_callers_is_removed_and_reported() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    git_init(dir);
    write(
        dir,
        "src/a.ts",
        "export function gone() { return 1; }\nexport function caller() { return gone(); }\n",
    );
    git_commit_all(dir, "baseline");
    let graph = index(dir);

    // Remove `gone` and its call (working tree).
    write(dir, "src/a.ts", "export function caller() { return 0; }\n");

    let report = detect_changes(&graph, dir, ChangeScope::Working).expect("detect");
    let gone = report
        .symbols
        .iter()
        .find(|s| s.key == "gone")
        .unwrap_or_else(|| panic!("gone must be a changed symbol: {:?}", report.symbols));
    assert_eq!(gone.change, ChangeKind::Removed);

    // caller depended on gone → it is in the blast radius.
    assert!(
        report.affected.iter().any(|a| a.name == "caller"),
        "deleting gone must surface caller in the blast radius: {:?}",
        report.affected
    );
    // The risk carries an affected-count reason.
    assert!(
        report.risk.reasons.iter().any(|r| r.contains("affected")),
        "risk reasons must include an affected count: {:?}",
        report.risk.reasons
    );
}

// ── add a function → Added, no impact ───────────────────────────────────────────

#[test]
fn added_fn_is_added_with_no_upstream_impact() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    git_init(dir);
    write(
        dir,
        "src/a.ts",
        "export function existing() { return 1; }\n",
    );
    git_commit_all(dir, "baseline");
    let graph = index(dir);

    // Add a brand-new function.
    write(
        dir,
        "src/a.ts",
        "export function existing() { return 1; }\nexport function fresh() { return 2; }\n",
    );

    let report = detect_changes(&graph, dir, ChangeScope::Working).expect("detect");
    let fresh = report
        .symbols
        .iter()
        .find(|s| s.key == "fresh")
        .unwrap_or_else(|| panic!("fresh must be a changed symbol: {:?}", report.symbols));
    assert_eq!(fresh.change, ChangeKind::Added);
    // An added symbol has no upstream — it must not be the source of any affected
    // node (the graph is the OLD graph, which doesn't even contain `fresh`).
    assert!(
        report.affected.is_empty(),
        "an added-only change has no blast radius: {:?}",
        report.affected
    );
}

// ── modify schema.graphql (remove a field) → contract plane + CRITICAL ───────────

#[test]
fn removed_graphql_field_is_contract_plane_and_critical() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    git_init(dir);
    write(
        dir,
        "schema.graphql",
        "type Query {\n  getUser: User\n  getStats: Stats\n}\n",
    );
    git_commit_all(dir, "baseline");
    let graph = index(dir);

    // Remove getStats.
    write(dir, "schema.graphql", "type Query {\n  getUser: User\n}\n");

    let report = detect_changes(&graph, dir, ChangeScope::Working).expect("detect");
    let stats = report
        .symbols
        .iter()
        .find(|s| s.key.contains("getStats"))
        .unwrap_or_else(|| panic!("getStats must be a changed symbol: {:?}", report.symbols));
    assert_eq!(stats.plane, Plane::Contract);
    assert_eq!(stats.change, ChangeKind::Removed);

    // Touching contract surface → CRITICAL, with a naming reason.
    assert_eq!(report.risk.level, RiskLevel::Critical);
    assert!(
        report
            .risk
            .reasons
            .iter()
            .any(|r| r.contains("contract surface") && r.contains("getStats")),
        "the CRITICAL reason must name the contract surface: {:?}",
        report.risk.reasons
    );
}

// ── contract changes carry the operation-level breaking/additive label ──────────

#[test]
fn removed_graphql_field_is_labeled_breaking() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    git_init(dir);
    write(
        dir,
        "schema.graphql",
        "type Query {\n  getUser: User\n  getStats: Stats\n}\n",
    );
    git_commit_all(dir, "baseline");
    let graph = index(dir);
    write(dir, "schema.graphql", "type Query {\n  getUser: User\n}\n");

    let report = detect_changes(&graph, dir, ChangeScope::Working).expect("detect");
    let stats = report
        .symbols
        .iter()
        .find(|s| s.key.contains("getStats"))
        .expect("getStats changed");
    assert_eq!(
        stats.contract_change,
        Some(ContractChange::Breaking),
        "a removed operation key breaks its consumers"
    );
    assert!(
        report.risk.reasons.iter().any(|r| r.contains("breaking")),
        "the CRITICAL reason says BREAKING, not just 'touches': {:?}",
        report.risk.reasons
    );
}

#[test]
fn added_graphql_field_is_additive_and_not_critical_by_itself() {
    // Adding new contract surface cannot break an existing consumer — it must be
    // labeled additive and must NOT, by itself, escalate to CRITICAL (the old
    // behaviour cried CRITICAL for every contract-plane change, additions included).
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    git_init(dir);
    write(dir, "schema.graphql", "type Query {\n  getUser: User\n}\n");
    git_commit_all(dir, "baseline");
    let graph = index(dir);
    write(
        dir,
        "schema.graphql",
        "type Query {\n  getUser: User\n  listUsers: [User]\n}\n",
    );

    let report = detect_changes(&graph, dir, ChangeScope::Working).expect("detect");
    let added = report
        .symbols
        .iter()
        .find(|s| s.key.contains("listUsers"))
        .expect("listUsers changed");
    assert_eq!(added.change, ChangeKind::Added);
    assert_eq!(
        added.contract_change,
        Some(ContractChange::Additive),
        "new surface is additive"
    );
    assert_ne!(
        report.risk.level,
        RiskLevel::Critical,
        "an additive-only contract change must not be CRITICAL by itself: {:?}",
        report.risk.reasons
    );
    assert!(
        report.risk.reasons.iter().any(|r| r.contains("additive")),
        "the report still names the additive surface honestly: {:?}",
        report.risk.reasons
    );
}

// ── template resource change → infra plane entry ────────────────────────────────

#[test]
fn changed_template_resource_is_infra_plane() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    git_init(dir);
    write(
        dir,
        "template.json",
        r#"{
  "Resources": {
    "Fn1": { "Type": "AWS::Serverless::Function", "Properties": { "Handler": "a.handler" } }
  }
}
"#,
    );
    git_commit_all(dir, "baseline");
    let graph = index(dir);

    // Change Fn1's handler property → Modified resource.
    write(
        dir,
        "template.json",
        r#"{
  "Resources": {
    "Fn1": { "Type": "AWS::Serverless::Function", "Properties": { "Handler": "b.handler" } }
  }
}
"#,
    );

    let report = detect_changes(&graph, dir, ChangeScope::Working).expect("detect");
    let fn1 = report
        .symbols
        .iter()
        .find(|s| s.key == "Fn1")
        .unwrap_or_else(|| panic!("Fn1 must be a changed symbol: {:?}", report.symbols));
    assert_eq!(fn1.plane, Plane::Infra);
    assert_eq!(fn1.change, ChangeKind::Modified);
}

// ── staged vs working distinction ───────────────────────────────────────────────

#[test]
fn staged_and_working_scopes_are_distinct() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    git_init(dir);
    write(dir, "src/a.ts", "export function f() { return 1; }\n");
    git_commit_all(dir, "baseline");
    let graph = index(dir);

    // Stage a modification to `f`, then make a FURTHER unstaged edit adding `g`.
    write(dir, "src/a.ts", "export function f() { return 2; }\n");
    git(dir, &["add", "src/a.ts"]);
    write(
        dir,
        "src/a.ts",
        "export function f() { return 2; }\nexport function g() { return 3; }\n",
    );

    // Staged scope sees only `f` modified (the staged blob has no `g`).
    let staged = detect_changes(&graph, dir, ChangeScope::Staged).expect("staged");
    assert_eq!(staged.scope, "staged");
    assert!(
        staged.symbols.iter().any(|s| s.key == "f"),
        "staged scope must include the staged modification of f: {:?}",
        staged.symbols
    );
    assert!(
        !staged.symbols.iter().any(|s| s.key == "g"),
        "staged scope must NOT include the unstaged-only g: {:?}",
        staged.symbols
    );

    // Working scope (vs HEAD) sees both `f` modified and `g` added.
    let working = detect_changes(&graph, dir, ChangeScope::Working).expect("working");
    assert_eq!(working.scope, "working");
    assert!(
        working.symbols.iter().any(|s| s.key == "g"),
        "working scope must include the new g: {:?}",
        working.symbols
    );
}

// ── renamed file (R status) handled ─────────────────────────────────────────────

#[test]
fn renamed_file_is_tracked_as_renamed() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    git_init(dir);
    // A file whose content is stable across the rename, so git records a pure R.
    write(
        dir,
        "src/old.ts",
        "export function stable() { return 42; }\nexport function more() { return stable(); }\n",
    );
    git_commit_all(dir, "baseline");
    let graph = index(dir);

    // Rename via git so the index records the move.
    git(dir, &["mv", "src/old.ts", "src/new.ts"]);

    let report = detect_changes(&graph, dir, ChangeScope::Staged).expect("detect");
    // The file appears as a rename old→new (a pure move emits no symbol changes,
    // which is correct — the symbols are byte-identical).
    let has_rename = report.files.iter().any(|f| {
        matches!(f, strata_index::FileChange::Renamed { old_path, path }
            if old_path == "src/old.ts" && path == "src/new.ts")
    });
    assert!(
        has_rename,
        "the move must be tracked as a rename: {:?}",
        report.files
    );
}

// ── not-a-git-repo → clear Err ──────────────────────────────────────────────────

#[test]
fn non_git_directory_is_a_clear_error() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path(); // a plain temp dir, never `git init`ed
    let graph = Graph::new();
    let err = detect_changes(&graph, dir, ChangeScope::Working).unwrap_err();
    // The message must be actionable (names "not a git repository"), never a
    // silent empty report.
    let msg = err.to_string();
    assert!(
        msg.contains("not a git repository"),
        "a non-repo must error clearly, got: {msg}"
    );
}

// ── other_files: a non-plane file is listed, never given symbols ────────────────

#[test]
fn non_plane_file_is_listed_as_other() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    git_init(dir);
    write(dir, "README.md", "# hello\n");
    git_commit_all(dir, "baseline");
    let graph = index(dir);

    write(dir, "README.md", "# hello world\n");

    let report = detect_changes(&graph, dir, ChangeScope::Working).expect("detect");
    assert!(
        report.other_files.iter().any(|f| f == "README.md"),
        "a changed README must be listed as an other_file: {:?}",
        report.other_files
    );
    assert!(
        report.symbols.is_empty(),
        "a markdown change must claim no symbols: {:?}",
        report.symbols
    );
}

// ── infra: a change to an UNCAPTURED property (QueueName) is still Modified ──────
//
// Regression (reviewer, slice 12): `detect_changes` signs each resource by its
// raw parsed sub-tree, NOT the lossy typed `InfraResource` (which captures only
// the graph-wired fields — handler, role refs, the AppSync chain — and drops
// everything else). So a change to a property the struct does not capture — here
// an `AWS::SQS::Queue`'s `QueueName` — must still surface as a Modified infra
// symbol, never silently fall through to `other_files`. Covered for BOTH
// template encodings (`.json` and `.yaml`): they share one `parse_to_value`, so
// a regression in either would be a real miss in a pre-commit change detector.

/// Commit `baseline` at `rel`, index it, then change it to `changed` in the
/// working tree (the ONLY edit being an uncaptured property) and assert the
/// resource `logical_id` reads as a Modified Infra symbol — and that the
/// template is NOT demoted to `other_files`.
fn assert_uncaptured_property_change_is_modified(
    rel: &str,
    baseline: &str,
    changed: &str,
    logical_id: &str,
) {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    git_init(dir);
    write(dir, rel, baseline);
    git_commit_all(dir, "baseline");
    let graph = index(dir);

    // The only difference between the two revisions is an uncaptured property;
    // everything the typed `InfraResource` captures is byte-identical.
    write(dir, rel, changed);

    let report = detect_changes(&graph, dir, ChangeScope::Working).expect("detect");

    let sym = report
        .symbols
        .iter()
        .find(|s| s.key == logical_id)
        .unwrap_or_else(|| {
            panic!(
                "{logical_id} must be a changed infra symbol: {:?}",
                report.symbols
            )
        });
    assert_eq!(sym.plane, Plane::Infra);
    assert_eq!(sym.change, ChangeKind::Modified);

    // The exact bug shape: an undetected change demotes the whole template to
    // `other_files` (zero symbols). It must be claimed by the infra plane instead.
    assert!(
        !report.other_files.iter().any(|f| f == rel),
        "an uncaptured-property change must claim the template, not demote it to other_files: {:?}",
        report.other_files
    );
}

#[test]
fn modified_uncaptured_property_json_is_infra_modified() {
    assert_uncaptured_property_change_is_modified(
        "queue.json",
        r#"{
  "Resources": {
    "Queue1": { "Type": "AWS::SQS::Queue", "Properties": { "QueueName": "original" } }
  }
}
"#,
        r#"{
  "Resources": {
    "Queue1": { "Type": "AWS::SQS::Queue", "Properties": { "QueueName": "changed" } }
  }
}
"#,
        "Queue1",
    );
}

#[test]
fn modified_uncaptured_property_yaml_is_infra_modified() {
    // Genuine block-style YAML (not JSON-in-a-.yaml), so the YAML event loader
    // path is exercised, not the JSON fast path.
    assert_uncaptured_property_change_is_modified(
        "queue.yaml",
        r#"Resources:
  Queue1:
    Type: AWS::SQS::Queue
    Properties:
      QueueName: original
"#,
        r#"Resources:
  Queue1:
    Type: AWS::SQS::Queue
    Properties:
      QueueName: changed
"#,
        "Queue1",
    );
}
