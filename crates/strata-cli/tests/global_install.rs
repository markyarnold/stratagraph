//! End-to-end tests for `strata init claude --global` and related edge cases.
//!
//! These drive the **real binary** (`CARGO_BIN_EXE_strata`) with a hermetic
//! `$HOME` (a temp dir) and a fake `claude` on `$PATH` that records its argv,
//! so no test touches the developer's real `~/.claude`.
//!
//! Tests:
//! 1. `global_install_writes_to_home_and_calls_claude_mcp_add` — the happy
//!    path: artifacts land under `~/.claude`, no `~/.mcp.json`, and the fake
//!    claude log contains the exact `mcp add strata --scope user -- strata mcp`
//!    invocation.
//! 2. `global_install_idempotent` — a second `--global` run still succeeds.
//! 3. `global_install_aborts_when_claude_absent` — with no `claude` on PATH
//!    the command exits non-zero and `~/.claude` is NOT created (all-or-nothing).
//! 4. `kiro_global_is_unsupported` — `init kiro --global` exits non-zero.

/// Build a PATH that prepends `dir` (containing a fake `claude` script) in
/// front of the real PATH, and return it as an `OsString` for `.env("PATH", …)`.
///
/// The fake `claude` appends its argv (space-joined) to `$CLAUDE_FAKE_LOG` and
/// exits 0. It is created inside `dir`; callers are responsible for keeping
/// `dir` alive for the duration of the test.
#[cfg(unix)]
fn with_fake_claude(dir: &std::path::Path) -> std::ffi::OsString {
    use std::os::unix::fs::PermissionsExt;

    let bin = dir.join("claude");
    std::fs::write(
        &bin,
        "#!/bin/sh\necho \"$@\" >> \"$CLAUDE_FAKE_LOG\"\nexit 0\n",
    )
    .unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();

    let prev = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![dir.to_path_buf()];
    paths.extend(std::env::split_paths(&prev));
    std::env::join_paths(paths).unwrap()
}

// ── Test 1: happy path ────────────────────────────────────────────────────────

#[cfg(unix)]
#[test]
fn global_install_writes_to_home_and_calls_claude_mcp_add() {
    let home = tempfile::tempdir().unwrap();
    let bindir = tempfile::tempdir().unwrap();
    let log = bindir.path().join("calls.log");

    let status = std::process::Command::new(env!("CARGO_BIN_EXE_strata"))
        .args(["init", "claude", "--global"])
        .env("HOME", home.path())
        .env("PATH", with_fake_claude(bindir.path()))
        .env("CLAUDE_FAKE_LOG", &log)
        .status()
        .unwrap();
    assert!(status.success(), "strata init claude --global must succeed");

    // Artifacts land under ~/.claude (not at the home root).
    let claude = home.path().join(".claude");
    assert!(
        claude.join("CLAUDE.md").exists(),
        "~/.claude/CLAUDE.md must exist"
    );
    assert!(
        claude.join("settings.json").exists(),
        "~/.claude/settings.json must exist"
    );
    assert!(
        claude.join("skills/strata/strata-guide/SKILL.md").exists(),
        "~/.claude/skills/strata/strata-guide/SKILL.md must exist"
    );

    // No ~/.mcp.json — user scope defers MCP to `claude mcp add`.
    assert!(
        !home.path().join(".mcp.json").exists(),
        "~/.mcp.json must NOT be written for --global"
    );

    // The fake claude log must contain the mcp add invocation.
    let calls = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(
        calls.contains("mcp add strata --scope user -- strata mcp"),
        "fake claude log must contain 'mcp add strata --scope user -- strata mcp'; got: {calls}"
    );
}

// ── Test 2: idempotency ───────────────────────────────────────────────────────

#[cfg(unix)]
#[test]
fn global_install_idempotent() {
    let home = tempfile::tempdir().unwrap();
    let bindir = tempfile::tempdir().unwrap();
    let log = bindir.path().join("calls.log");
    let path = with_fake_claude(bindir.path());

    // First run.
    let s1 = std::process::Command::new(env!("CARGO_BIN_EXE_strata"))
        .args(["init", "claude", "--global"])
        .env("HOME", home.path())
        .env("PATH", &path)
        .env("CLAUDE_FAKE_LOG", &log)
        .status()
        .unwrap();
    assert!(s1.success(), "first --global run must succeed");

    // Second run on the same home.
    let s2 = std::process::Command::new(env!("CARGO_BIN_EXE_strata"))
        .args(["init", "claude", "--global"])
        .env("HOME", home.path())
        .env("PATH", &path)
        .env("CLAUDE_FAKE_LOG", &log)
        .status()
        .unwrap();
    assert!(
        s2.success(),
        "second (idempotent) --global run must also succeed"
    );

    // Artifacts still present after both runs.
    let claude = home.path().join(".claude");
    assert!(claude.join("CLAUDE.md").exists());
    assert!(claude.join("settings.json").exists());
    assert!(claude.join("skills/strata/strata-guide/SKILL.md").exists());
}

// ── Test 3: abort when claude absent ─────────────────────────────────────────

#[cfg(unix)]
#[test]
fn global_install_aborts_when_claude_absent() {
    let home = tempfile::tempdir().unwrap();
    // An empty dir on PATH: no `claude` binary present.
    let emptybin = tempfile::tempdir().unwrap();

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_strata"))
        .args(["init", "claude", "--global"])
        .env("HOME", home.path())
        .env("PATH", emptybin.path())
        .output()
        .unwrap();

    assert!(
        !out.status.success(),
        "must exit non-zero when `claude` is absent"
    );
    // All-or-nothing: ~/.claude must NOT have been created.
    assert!(
        !home.path().join(".claude").exists(),
        "~/.claude must NOT be created when `claude` is absent on PATH"
    );
}

// ── Test 4: kiro --global is unsupported ─────────────────────────────────────

#[cfg(unix)]
#[test]
fn kiro_global_is_unsupported() {
    let home = tempfile::tempdir().unwrap();

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_strata"))
        .args(["init", "kiro", "--global"])
        .env("HOME", home.path())
        .output()
        .unwrap();

    assert!(
        !out.status.success(),
        "strata init kiro --global must exit non-zero (unsupported)"
    );
}
