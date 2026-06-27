//! Integration tests for `strata init claude|kiro` — the merge-safety suite.
//!
//! These drive the **built binary** (`CARGO_BIN_EXE_strata`) against fixture
//! repos in temp dirs, proving the whole init pipeline through the real CLI
//! surface. The headline guarantee is **foreign-content preservation**: a repo
//! already configured (user prose, a GitNexus block, a foreign MCP server, a
//! foreign hook) must keep every foreign byte intact while ours is added
//! alongside. The 7 scenarios map 1:1 to the plan's test list.

use std::fs;
use std::path::Path;
use std::process::Command;

/// Absolute path to the binary cargo built for this test run.
fn strata_bin() -> &'static str {
    env!("CARGO_BIN_EXE_strata")
}

/// Run `strata init <args…>` in/​against `path`, returning (success, stdout, stderr).
fn run_init(args: &[&str]) -> (bool, String, String) {
    let out = Command::new(strata_bin())
        .arg("init")
        .args(args)
        .output()
        .expect("spawn strata init");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Read a file to a String (panicking if absent — the test wants it present).
fn read(p: &Path) -> String {
    fs::read_to_string(p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// Write a tiny indexable TS repo into `root` so `--yes` has something to index.
fn write_tiny_repo(root: &Path) {
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("src/a.ts"),
        "export function alpha() { return 1; }\n",
    )
    .unwrap();
}

// ── Scenario 1: fresh tempdir → all created; second run → ALL Unchanged ───────

#[test]
fn scenario1_fresh_install_then_idempotent_rerun() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    let (ok, out, err) = run_init(&["claude", "--path", root.to_str().unwrap()]);
    assert!(ok, "init claude must exit 0; stderr: {err}");
    // Every artifact reported created.
    for f in [
        ".mcp.json",
        "CLAUDE.md",
        "AGENTS.md",
        ".claude/skills/strata/strata-guide/SKILL.md",
        ".claude/skills/strata/strata-impact-analysis/SKILL.md",
        ".claude/settings.json",
    ] {
        assert!(root.join(f).exists(), "{f} must exist after init");
    }
    assert!(out.contains("created"), "first run reports created: {out}");

    // Slice 12: the installed Claude steering names detect_changes as the
    // pre-commit check (kit alignment).
    let claude_md = read(&root.join("CLAUDE.md"));
    assert!(
        claude_md.contains("detect_changes"),
        "Claude steering must name detect_changes; got:\n{claude_md}"
    );

    // Second run: every file Unchanged (idempotency), asserted per file — no
    // "created"/"updated" lines may appear.
    let (ok2, out2, err2) = run_init(&["claude", "--path", root.to_str().unwrap()]);
    assert!(ok2, "second init must exit 0; stderr: {err2}");
    assert!(
        !out2.contains("created") && !out2.contains("updated"),
        "second run must be all-unchanged, got:\n{out2}"
    );
    assert!(out2.contains("unchanged"), "second run reports unchanged");
}

// ── Scenario 2: foreign-content preservation (THE critical one) ───────────────

#[test]
fn scenario2_foreign_content_is_preserved_exactly() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    // Pre-seed: user prose + a GitNexus managed block in CLAUDE.md.
    let claude_md_before = "\
# My Project

Hand-written guidance the user cares about.

<!-- gitnexus:start -->
# GitNexus — Code Intelligence
Indexed as my-app. Use the gitnexus tools.
<!-- gitnexus:end -->
";
    fs::write(root.join("CLAUDE.md"), claude_md_before).unwrap();

    // Pre-seed: a foreign MCP server in .mcp.json.
    let mcp_before = r#"{
  "mcpServers": {
    "gitnexus": {
      "command": "npx",
      "args": ["gitnexus", "mcp"]
    }
  },
  "userSetting": true
}
"#;
    fs::write(root.join(".mcp.json"), mcp_before).unwrap();

    // Pre-seed: a foreign hook in .claude/settings.json.
    fs::create_dir_all(root.join(".claude")).unwrap();
    let settings_before = r#"{
  "hooks": {
    "PostToolUse": [
      {
        "matcher": "Bash",
        "hooks": [ { "type": "command", "command": "echo foreign-bash-hook" } ]
      }
    ]
  }
}
"#;
    fs::write(root.join(".claude/settings.json"), settings_before).unwrap();

    let (ok, _out, err) = run_init(&["claude", "--path", root.to_str().unwrap()]);
    assert!(ok, "init must exit 0; stderr: {err}");

    // 1. CLAUDE.md: every original byte still present (prose + gitnexus block),
    //    ours appended after, markers intact.
    let claude_after = read(&root.join("CLAUDE.md"));
    assert!(
        claude_after.starts_with(claude_md_before.trim_end_matches('\n')),
        "user prose + gitnexus block must be preserved verbatim at the front:\n{claude_after}"
    );
    assert!(
        claude_after.contains("<!-- gitnexus:start -->")
            && claude_after.contains("Indexed as my-app. Use the gitnexus tools.")
            && claude_after.contains("<!-- gitnexus:end -->"),
        "foreign gitnexus block must survive exactly"
    );
    assert!(
        claude_after.contains("<!-- strata:begin -->")
            && claude_after.contains("StrataGraph: Cross-Plane Code Intelligence"),
        "our managed block must be added alongside"
    );

    // 2. .mcp.json: foreign server + foreign key preserved; ours added.
    let mcp_v: serde_json::Value =
        serde_json::from_str(&read(&root.join(".mcp.json"))).expect("mcp.json valid");
    assert_eq!(mcp_v["mcpServers"]["gitnexus"]["command"], "npx");
    assert_eq!(mcp_v["mcpServers"]["gitnexus"]["args"][0], "gitnexus");
    assert_eq!(
        mcp_v["userSetting"], true,
        "foreign top-level key preserved"
    );
    assert_eq!(mcp_v["mcpServers"]["strata"]["command"], "strata");

    // 3. .claude/settings.json: foreign Bash hook preserved; ours added alongside.
    let settings_v: serde_json::Value =
        serde_json::from_str(&read(&root.join(".claude/settings.json"))).expect("settings valid");
    let post = settings_v["hooks"]["PostToolUse"].as_array().unwrap();
    assert!(
        post.iter().any(|g| g["matcher"] == "Bash"
            && g["hooks"][0]["command"] == "echo foreign-bash-hook"),
        "foreign Bash hook must be preserved exactly: {post:?}"
    );
    assert!(
        post.iter().any(|g| g["matcher"] == "Edit|Write|MultiEdit"),
        "StrataGraph hook must be added alongside the foreign one"
    );
}

// ── Scenario 3: stale managed block replaced in place; surrounding bytes kept ──

#[test]
fn scenario3_stale_managed_block_replaced_in_place() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    let top = "# Top prose\n\n";
    let bottom = "\n# Bottom prose\n";
    let stale = format!(
        "{top}<!-- strata:begin -->\nSTALE STRATA CONTENT FROM AN OLD VERSION\n<!-- strata:end -->{bottom}"
    );
    fs::write(root.join("AGENTS.md"), &stale).unwrap();

    let (ok, _o, err) = run_init(&["claude", "--path", root.to_str().unwrap()]);
    assert!(ok, "init must exit 0; stderr: {err}");

    let after = read(&root.join("AGENTS.md"));
    // Surrounding prose is byte-identical.
    assert!(after.starts_with("# Top prose\n\n"), "top prose preserved");
    assert!(
        after.ends_with("\n# Bottom prose\n"),
        "bottom prose preserved"
    );
    // The stale body is gone, the fresh block is in.
    assert!(
        !after.contains("STALE STRATA CONTENT"),
        "stale block must be replaced"
    );
    assert!(
        after.contains("StrataGraph: Cross-Plane Code Intelligence"),
        "fresh block present"
    );
}

// ── Scenario 4: init kiro → all files; DEFAULT is the legacy .kiro.hook schema ─

#[test]
fn scenario4_kiro_default_uses_legacy_kiro_hook_schema() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    // No --kiro-version → the default `old` (legacy `.kiro.hook`) format.
    let (ok, _o, err) = run_init(&["kiro", "--path", root.to_str().unwrap()]);
    assert!(ok, "init kiro must exit 0; stderr: {err}");

    for f in [
        ".kiro/settings/mcp.json",
        ".kiro/steering/strata.md",
        ".kiro/hooks/strata-pre-edit.kiro.hook",
        ".kiro/hooks/strata-pre-commit.kiro.hook",
        ".kiro/hooks/strata-post-commit.kiro.hook",
    ] {
        assert!(root.join(f).exists(), "{f} must exist (old default)");
    }
    assert!(
        !root.join(".kiro/hooks/strata-pre-edit.json").exists(),
        "old default must not emit the new .json format"
    );

    // pre-edit: legacy when/then → preToolUse(write) → askAgent.
    let pre_edit: serde_json::Value =
        serde_json::from_str(&read(&root.join(".kiro/hooks/strata-pre-edit.kiro.hook"))).unwrap();
    assert_eq!(pre_edit["version"], "1");
    assert_eq!(pre_edit["when"]["type"], "preToolUse");
    assert_eq!(pre_edit["when"]["toolTypes"][0], "write");
    assert_eq!(pre_edit["then"]["type"], "askAgent");

    // post-commit: postToolUse → runCommand strata index.
    let post_commit: serde_json::Value = serde_json::from_str(&read(
        &root.join(".kiro/hooks/strata-post-commit.kiro.hook"),
    ))
    .unwrap();
    assert_eq!(post_commit["when"]["type"], "postToolUse");
    assert_eq!(post_commit["then"]["type"], "runCommand");
    assert_eq!(post_commit["then"]["command"], "strata index .");
    assert_eq!(post_commit["then"]["timeout"], 120);

    // Steering + pre-commit prompt name detect_changes (format-agnostic content).
    let steering = read(&root.join(".kiro/steering/strata.md"));
    assert!(steering.contains("<!-- strata:begin -->"));
    assert!(
        steering.contains("detect_changes"),
        "Kiro steering must name detect_changes; got:\n{steering}"
    );
    let pre_commit_raw = read(&root.join(".kiro/hooks/strata-pre-commit.kiro.hook"));
    assert!(
        pre_commit_raw.contains("detect_changes"),
        "Kiro pre-commit hook prompt must name detect_changes; got:\n{pre_commit_raw}"
    );
}

// ── Scenario 4b: --kiro-version new → the v1 .json schema ─────────────────────

#[test]
fn scenario4b_kiro_new_version_uses_v1_json_schema() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    let (ok, _o, err) = run_init(&[
        "kiro",
        "--path",
        root.to_str().unwrap(),
        "--kiro-version",
        "new",
    ]);
    assert!(
        ok,
        "init kiro --kiro-version new must exit 0; stderr: {err}"
    );

    for f in [
        ".kiro/hooks/strata-pre-edit.json",
        ".kiro/hooks/strata-pre-commit.json",
        ".kiro/hooks/strata-post-commit.json",
    ] {
        assert!(root.join(f).exists(), "{f} must exist (--kiro-version new)");
    }
    assert!(
        !root.join(".kiro/hooks/strata-pre-edit.kiro.hook").exists(),
        "new format must not emit the legacy .kiro.hook"
    );

    let pre_edit: serde_json::Value =
        serde_json::from_str(&read(&root.join(".kiro/hooks/strata-pre-edit.json"))).unwrap();
    assert_eq!(pre_edit["version"], "v1");
    let pe = &pre_edit["hooks"][0];
    assert_eq!(pe["trigger"], "PreToolUse");
    assert_eq!(pe["matcher"], "fs_write|str_replace|fs_append");
    assert_eq!(pe["action"]["type"], "agent");

    let post_commit: serde_json::Value =
        serde_json::from_str(&read(&root.join(".kiro/hooks/strata-post-commit.json"))).unwrap();
    let poc = &post_commit["hooks"][0];
    assert_eq!(poc["trigger"], "PostToolUse");
    assert_eq!(poc["action"]["type"], "command");
    assert_eq!(poc["action"]["command"], "strata index .");
}

// ── Scenario 4c: unknown --kiro-version → actionable error ────────────────────

#[test]
fn scenario4c_unknown_kiro_version_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let (ok, _o, err) = run_init(&[
        "kiro",
        "--path",
        root.to_str().unwrap(),
        "--kiro-version",
        "v3",
    ]);
    assert!(!ok, "an unknown --kiro-version must be a non-zero exit");
    assert!(
        err.contains("kiro-version") && err.contains("old") && err.contains("new"),
        "error must name the supported versions; got: {err}"
    );
}

// ── Scenario 5: no-index flow (honest variant) vs --yes (real counts) ─────────

#[test]
fn scenario5a_no_index_writes_not_indexed_variant_without_hanging() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    // No .strata, no --yes: must NOT hang on stdin and must write the honest
    // "not yet indexed" identity.
    let (ok, _o, err) = run_init(&["claude", "--path", root.to_str().unwrap()]);
    assert!(ok, "init must exit 0 without hanging; stderr: {err}");
    let claude = read(&root.join("CLAUDE.md"));
    assert!(
        claude.contains("not yet indexed"),
        "no-index run must write the not-indexed identity variant:\n{claude}"
    );
}

#[test]
fn scenario5b_yes_on_a_tiny_repo_indexes_and_identity_has_real_counts() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_tiny_repo(root);

    let (ok, _o, err) = run_init(&["claude", "--path", root.to_str().unwrap(), "--yes"]);
    assert!(ok, "init --yes must exit 0; stderr: {err}");
    // The index was built.
    assert!(
        root.join(".strata/graph.duckdb").exists(),
        "--yes must build the index"
    );
    // Identity line carries real, non-zero counts.
    let claude = read(&root.join("CLAUDE.md"));
    assert!(
        claude.contains("indexed by StrataGraph") && claude.contains("nodes,"),
        "identity must carry real counts after --yes:\n{claude}"
    );
    assert!(
        !claude.contains("not yet indexed"),
        "indexed repo must not show the not-indexed variant"
    );
}

// ── Scenario 6: workspace mode → .mcp.json args contain --workspace ───────────

#[test]
fn scenario6_workspace_manifest_yields_workspace_args() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    fs::write(
        root.join("strata.workspace.toml"),
        "[workspace]\nname = \"my-estate\"\n",
    )
    .unwrap();

    let (ok, _o, err) = run_init(&["claude", "--path", root.to_str().unwrap()]);
    assert!(ok, "init must exit 0; stderr: {err}");
    let mcp: serde_json::Value =
        serde_json::from_str(&read(&root.join(".mcp.json"))).expect("mcp.json valid");
    let args = mcp["mcpServers"]["strata"]["args"].as_array().unwrap();
    let args: Vec<&str> = args.iter().map(|v| v.as_str().unwrap()).collect();
    assert_eq!(
        args,
        vec!["mcp", "--workspace", "strata.workspace.toml"],
        "workspace manifest must switch MCP args to --workspace"
    );
}

// ── Scenario 7: unknown agent → error names supported; malformed JSON safe ────

#[test]
fn scenario7a_unknown_agent_errors_naming_supported_agents() {
    let (ok, _o, err) = run_init(&["cursor"]);
    assert!(!ok, "unknown agent must be a non-zero exit");
    assert!(
        err.contains("unknown agent") && err.contains("claude") && err.contains("kiro"),
        "error must name the supported agents; got: {err}"
    );
}

#[test]
fn scenario7b_bare_init_lists_supported_agents() {
    let out = Command::new(strata_bin())
        .arg("init")
        .output()
        .expect("spawn");
    assert!(out.status.success(), "bare init lists agents and exits 0");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("claude") && stdout.contains("kiro"),
        "bare init must list supported agents; got: {stdout}"
    );
}

#[test]
fn scenario7c_malformed_existing_mcp_json_is_error_and_file_untouched() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let garbage = "{ not valid json at all ]";
    fs::write(root.join(".mcp.json"), garbage).unwrap();

    let (ok, _o, err) = run_init(&["claude", "--path", root.to_str().unwrap()]);
    assert!(!ok, "malformed .mcp.json must make init fail");
    assert!(
        err.contains("invalid JSON") || err.contains(".mcp.json"),
        "error must point at the malformed file; got: {err}"
    );
    // The file is left exactly as it was — never clobbered.
    assert_eq!(
        read(&root.join(".mcp.json")),
        garbage,
        "malformed file must be untouched"
    );
}

// ── Binary-level DoD: `strata init claude --yes` end-to-end in a tempdir ──────

#[test]
fn binary_init_claude_yes_end_to_end() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_tiny_repo(root);

    let out = Command::new(strata_bin())
        .args(["init", "claude", "--path", root.to_str().unwrap(), "--yes"])
        .output()
        .expect("spawn strata init claude --yes");
    assert!(
        out.status.success(),
        "strata init claude --yes must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // The full kit is on disk and the index was built.
    assert!(root.join(".strata/graph.duckdb").exists());
    assert!(root.join(".mcp.json").exists());
    assert!(root.join("CLAUDE.md").exists());
    assert!(root.join("AGENTS.md").exists());
    assert!(root.join(".claude/settings.json").exists());
    for slug in [
        "strata-guide",
        "strata-exploring",
        "strata-impact-analysis",
        "strata-contracts-and-infra",
    ] {
        assert!(
            root.join(format!(".claude/skills/strata/{slug}/SKILL.md"))
                .exists(),
            "skill {slug} must exist"
        );
    }

    // Every emitted JSON parses.
    for f in [".mcp.json", ".claude/settings.json"] {
        serde_json::from_str::<serde_json::Value>(&read(&root.join(f)))
            .unwrap_or_else(|e| panic!("{f} must be valid JSON: {e}"));
    }
}
