//! Claude Code artifact renderer for `strata init claude`.
//!
//! Writes, all merge-safely:
//! * `.mcp.json` — merge-add `mcpServers.strata` with the detected launch args
//!   (project scope only; user scope defers MCP registration to Task 5);
//! * `CLAUDE.md` + `AGENTS.md` — the same managed steering block (project scope),
//!   or `~/.claude/CLAUDE.md` with the generic identity block (user scope);
//! * `.claude/skills/strata/<slug>/SKILL.md` — the four task-routed skills;
//! * `.claude/settings.json` — scoped, silent-when-clean hooks (R1/R2), each
//!   carrying the `strata-hook` marker token for structural idempotency;
//!   PreToolUse/PostToolUse are shared across scopes, but SessionStart is the
//!   `.strata`-guarded variant for user scope (silent in non-StrataGraph repos).

use std::path::Path;

use serde_json::{json, Value};

use super::content::{self, Identity};
use super::writers::{self, hooks_event_array, upsert_hook, WriteError, HOOK_MARKER};
use super::{FileReport, InstallScope, RepoContext};

/// The production runner: exec `claude` with `args`, capturing output.
fn claude_runner(args: &[&str]) -> std::io::Result<std::process::Output> {
    std::process::Command::new("claude").args(args).output()
}

/// Register the user-scope MCP server via the official CLI (we never hand-edit
/// ~/.claude.json). Idempotent: remove any prior `strata` user server, then add.
/// `run` is injected for testability. The first call doubles as the presence
/// check: a NotFound spawn error means the `claude` CLI is absent.
pub fn register_user_mcp(
    run: impl Fn(&[&str]) -> std::io::Result<std::process::Output>,
) -> Result<(), WriteError> {
    let not_found = |_| WriteError::Command {
        detail: "the `claude` CLI was not found on PATH; install Claude Code (you are installing its kit) and re-run".into(),
    };
    // Best-effort remove (ignore its exit status); a NotFound here = claude absent.
    match run(&["mcp", "remove", "strata", "--scope", "user"]) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(not_found(e)),
        Err(e) => return Err(WriteError::Command { detail: e.to_string() }),
    }
    let out = run(&["mcp", "add", "strata", "--scope", "user", "--", "strata", "mcp"])
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                not_found(e)
            } else {
                WriteError::Command { detail: e.to_string() }
            }
        })?;
    if !out.status.success() {
        return Err(WriteError::Command {
            detail: format!(
                "`claude mcp add` failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        });
    }
    Ok(())
}

/// Install the Claude Code kit under `root`, returning a [`FileReport`] per file.
///
/// For `InstallScope::User`, `register_user_mcp` is called BEFORE any file is
/// written (abort-before-files): if the `claude` CLI is absent or the add fails,
/// no files are written and the error is returned. For `InstallScope::Project`,
/// `install_files` is called directly (which writes `.mcp.json` as before).
pub fn install(root: &Path, ctx: &RepoContext, scope: InstallScope) -> Result<Vec<FileReport>, WriteError> {
    if scope == InstallScope::User {
        register_user_mcp(claude_runner)?;
        let mut reports = install_files(root, ctx, scope)?;
        reports.insert(0, FileReport::new("claude mcp add strata (user scope)", super::writers::Outcome::Updated));
        return Ok(reports);
    }
    install_files(root, ctx, scope)
}

/// Write all Claude Code file artifacts under `root` for the given `scope`,
/// returning a [`FileReport`] per file.
///
/// This is the file-writing portion of the install, separated so Task 5 can
/// call it after the MCP shell-out without duplicating logic, and so the test
/// suite can exercise it directly.
///
/// **Project scope** (root is the repo):
///   - `.mcp.json` — merge-add `mcpServers.strata`;
///   - `CLAUDE.md` + `AGENTS.md` — managed steering block (per-repo identity);
///   - `.claude/skills/strata/<slug>/SKILL.md` — four task-routed skills;
///   - `.claude/settings.json` — hooks with project SessionStart.
///
/// **User scope** (root is the home directory, e.g. `~`):
///   - NO `.mcp.json` (Task 5 handles MCP registration via `claude mcp add`);
///   - `.claude/CLAUDE.md` — managed steering block with generic identity;
///   - `.claude/skills/strata/<slug>/SKILL.md` — four task-routed skills;
///   - `.claude/settings.json` — hooks with `.strata`-guarded SessionStart.
pub fn install_files(root: &Path, ctx: &RepoContext, scope: InstallScope) -> Result<Vec<FileReport>, WriteError> {
    let mut reports = Vec::new();

    match scope {
        InstallScope::Project => {
            // 1. .mcp.json — merge-add mcpServers.strata (project only).
            let mcp_path = root.join(".mcp.json");
            let server = mcp_server_value(&ctx.mcp_args);
            let outcome = writers::merge_json(&mcp_path, &json!({ "mcpServers": { "strata": server } }))?;
            reports.push(FileReport::new(".mcp.json", outcome));

            // 2. CLAUDE.md + AGENTS.md — per-repo identity steering block.
            let block = content::render_steering_block(&ctx.identity, content::CLAUDE_ROUTING);
            for fname in ["CLAUDE.md", "AGENTS.md"] {
                let outcome = writers::upsert_managed_block(&root.join(fname), &block)?;
                reports.push(FileReport::new(fname, outcome));
            }
        }
        InstallScope::User => {
            // 1. No .mcp.json — Task 5 handles MCP registration via `claude mcp add`.
            // 2. ~/.claude/CLAUDE.md — generic identity steering block.
            let block = content::render_steering_block(&Identity::Global, content::CLAUDE_ROUTING);
            let claude_md = root.join(".claude/CLAUDE.md");
            let outcome = writers::upsert_managed_block(&claude_md, &block)?;
            reports.push(FileReport::new(".claude/CLAUDE.md", outcome));
        }
    }

    // 3. Skills — wholly owned files (identical for both scopes).
    for (slug, body) in content::skills() {
        let rel = format!(".claude/skills/strata/{slug}/SKILL.md");
        let outcome = writers::write_owned(&root.join(&rel), &body)?;
        reports.push(FileReport::new(rel, outcome));
    }

    // 4. .claude/settings.json — scoped silent-when-clean hooks.
    //    SessionStart variant differs by scope; Pre/PostToolUse are identical.
    let settings_path = root.join(".claude/settings.json");
    let outcome = writers::edit_json(&settings_path, |settings| install_hooks_for(settings, scope))?;
    reports.push(FileReport::new(".claude/settings.json", outcome));

    Ok(reports)
}

/// The `mcpServers.strata` value: the `strata` binary with the detected args.
fn mcp_server_value(args: &[String]) -> Value {
    json!({
        "command": "strata",
        "args": args,
    })
}

/// Upsert the three StrataGraph hooks into a parsed `.claude/settings.json` root,
/// preserving any foreign hooks under the same events.
///
/// * **PreToolUse** `Edit|Write|MultiEdit` → the pre-edit blast check: COMPUTES
///   the edited file's blast radius and injects it as `additionalContext` (exit 0,
///   NO `permissionDecision`) so the agent sees what depends on the file before it
///   changes it — non-blocking, never halts or loops an edit, silent when there is
///   no index / no file / nothing to say, degrade-safe on any error.
/// * **PostToolUse** `Edit|Write|MultiEdit` → R2 stay-fresh: silent (exit 0) when
///   `.strata/` is absent, else backgrounds an incremental `strata index`.
/// * **SessionStart** → R1: chosen by `scope` — project uses the nudge-when-unindexed
///   variant; user uses the `.strata`-guarded variant (silent in non-Strata repos).
///
/// Pre- and PostToolUse groups are identical across scopes (already `$CLAUDE_PROJECT_DIR`
/// and `.strata`-guarded, so they are inert in non-Strata repos). Only SessionStart
/// differs: for User scope a global install must never nag in non-Strata projects.
fn install_hooks_for(root: &mut Value, scope: InstallScope) {
    if let Some(arr) = hooks_event_array(root, "PreToolUse") {
        upsert_hook(arr, pre_tool_use_group());
    }
    if let Some(arr) = hooks_event_array(root, "PostToolUse") {
        upsert_hook(arr, post_tool_use_group());
    }
    if let Some(arr) = hooks_event_array(root, "SessionStart") {
        let group = match scope {
            InstallScope::Project => session_start_group(),
            InstallScope::User => session_start_group_guarded(),
        };
        upsert_hook(arr, group);
    }
}

/// The PreToolUse matcher-group: before an Edit/Write/MultiEdit, COMPUTE the
/// edited file's blast radius and inject it as `additionalContext` so the agent
/// reliably sees what depends on the file *before* it changes it — the robust
/// enforcement of the impact-before-edit discipline.
///
/// The contract this command obeys, exactly (the non-negotiables):
/// * **non-blocking** — it emits only `hookSpecificOutput.additionalContext` and
///   always `exit 0`; it NEVER emits a `permissionDecision`, so it can never halt
///   or loop an edit;
/// * **silent-when-clean** — no `.strata/` directory → exit 0 with no output (no
///   kit-less noise); no `file_path` in the stdin JSON, an empty blast, or any
///   `strata` failure → exit 0 silently (degrade-safe; an editor's edit is never
///   blocked by a StrataGraph hiccup);
/// * **`jq`-optional** — when `jq` is present it parses `tool_input.file_path` and
///   builds the (correctly-escaped) output JSON with it; when `jq` is ABSENT it
///   still injects a **static advisory** `additionalContext` (the run-impact-first
///   instruction) so the discipline is enforced even without a JSON parser.
///
/// The `strata-hook` marker rides in a trailing comment for idempotent re-runs.
fn pre_tool_use_group() -> Value {
    // The computed-blast path (jq present) and the static-advisory fallback (jq
    // absent) are both fully self-contained and both `exit 0` on every branch.
    // Single-quoted `sh -c` body: no Rust interpolation except the marker.
    let command = format!(
        "sh -c 'd=\"$CLAUDE_PROJECT_DIR\"; [ -d \"$d/.strata\" ] || exit 0; input=$(cat); if command -v jq >/dev/null 2>&1; then f=$(printf \"%s\" \"$input\" | jq -r \".tool_input.file_path // empty\"); [ -n \"$f\" ] || exit 0; blast=$(strata blast \"$f\" --format agent 2>/dev/null) || exit 0; [ -n \"$blast\" ] || exit 0; jq -n --arg c \"$blast\" \"{{hookSpecificOutput:{{hookEventName:\\\"PreToolUse\\\",additionalContext:\\$c}}}}\"; exit 0; else printf \"%s\" \"{{\\\"hookSpecificOutput\\\":{{\\\"hookEventName\\\":\\\"PreToolUse\\\",\\\"additionalContext\\\":\\\"StrataGraph pre-edit: run impact/context (or strata blast <file>) on the symbols in this file BEFORE editing, and report the blast radius. Treat confidence < 0.40 or ambiguous as UNKNOWN. PAUSE if risk is HIGH/CRITICAL or crosses a repo boundary. (install jq for the auto-computed blast radius.)\\\"}}}}\"; exit 0; fi' # {HOOK_MARKER}"
    );
    json!({
        "matcher": "Edit|Write|MultiEdit",
        "hooks": [ { "type": "command", "command": command } ]
    })
}

/// The PostToolUse matcher-group: on Edit/Write/MultiEdit, keep the on-disk index
/// fresh in the background, but stay completely silent when there is nothing to
/// do (no `.strata/` directory). The `strata-hook` marker rides in the command.
fn post_tool_use_group() -> Value {
    let command = format!(
        "sh -c 'd=\"$CLAUDE_PROJECT_DIR\"; [ -d \"$d/.strata\" ] || exit 0; (strata index \"$d\" --db \"$d/.strata/graph.duckdb\" >/dev/null 2>&1 &); exit 0' # {HOOK_MARKER}"
    );
    json!({
        "matcher": "Edit|Write|MultiEdit",
        "hooks": [ { "type": "command", "command": command } ]
    })
}

/// The SessionStart matcher-group: print one guidance line only when the graph
/// DB is missing (silent-when-clean). The `strata-hook` marker rides in the
/// command.
fn session_start_group() -> Value {
    let command = format!(
        "sh -c 'd=\"$CLAUDE_PROJECT_DIR\"; [ -f \"$d/.strata/graph.duckdb\" ] && exit 0; echo \"StrataGraph: no index yet, run \\`strata index .\\` to enable cross-plane impact analysis.\"' # {HOOK_MARKER}"
    );
    json!({
        "matcher": "",
        "hooks": [ { "type": "command", "command": command } ]
    })
}

/// The SessionStart matcher-group for user (global) scope: like
/// [`session_start_group`] but also guards on the `.strata` directory being
/// present, so a global install never nags in repos that have not been
/// indexed with StrataGraph. Silent unless the repo has `.strata/` but not
/// `.strata/graph.duckdb`.
fn session_start_group_guarded() -> Value {
    let command = format!(
        "sh -c 'd=\"$CLAUDE_PROJECT_DIR\"; [ -d \"$d/.strata\" ] || exit 0; [ -f \"$d/.strata/graph.duckdb\" ] && exit 0; echo \"StrataGraph: no index yet, run \\`strata index .\\` to enable cross-plane impact analysis.\"' # {HOOK_MARKER}"
    );
    json!({ "matcher": "", "hooks": [ { "type": "command", "command": command } ] })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::init::content::Identity;
    use crate::init::writers::Outcome;
    use tempfile::TempDir;

    /// A db-mode context with a not-indexed identity (the common fresh-repo case).
    fn ctx_db() -> RepoContext {
        RepoContext {
            mcp_args: vec!["mcp".into(), "--db".into(), crate::DEFAULT_DB.into()],
            identity: Identity::NotIndexed,
        }
    }

    #[test]
    fn install_writes_all_claude_artifacts() {
        let tmp = TempDir::new().unwrap();
        let reports = install(tmp.path(), &ctx_db(), crate::init::InstallScope::Project).unwrap();

        // Every expected file exists on disk.
        for rel in [
            ".mcp.json",
            "CLAUDE.md",
            "AGENTS.md",
            ".claude/skills/strata/strata-guide/SKILL.md",
            ".claude/skills/strata/strata-exploring/SKILL.md",
            ".claude/skills/strata/strata-impact-analysis/SKILL.md",
            ".claude/skills/strata/strata-contracts-and-infra/SKILL.md",
            ".claude/settings.json",
        ] {
            assert!(
                tmp.path().join(rel).exists(),
                "expected {rel} to be written"
            );
        }
        // All created on a fresh repo.
        assert!(reports.iter().all(|r| r.outcome == Outcome::Created));
    }

    #[test]
    fn mcp_json_registers_strata_server_with_detected_args() {
        let tmp = TempDir::new().unwrap();
        install(tmp.path(), &ctx_db(), crate::init::InstallScope::Project).unwrap();
        let v: Value =
            serde_json::from_str(&std::fs::read_to_string(tmp.path().join(".mcp.json")).unwrap())
                .unwrap();
        assert_eq!(v["mcpServers"]["strata"]["command"], "strata");
        assert_eq!(
            v["mcpServers"]["strata"]["args"],
            json!(["mcp", "--db", crate::DEFAULT_DB])
        );
    }

    #[test]
    fn hooks_carry_marker_and_are_scoped() {
        let tmp = TempDir::new().unwrap();
        install(tmp.path(), &ctx_db(), crate::init::InstallScope::Project).unwrap();
        let v: Value = serde_json::from_str(
            &std::fs::read_to_string(tmp.path().join(".claude/settings.json")).unwrap(),
        )
        .unwrap();

        let post = &v["hooks"]["PostToolUse"][0];
        assert_eq!(post["matcher"], "Edit|Write|MultiEdit");
        let post_cmd = post["hooks"][0]["command"].as_str().unwrap();
        assert!(post_cmd.contains(HOOK_MARKER), "marker present: {post_cmd}");
        // R1/R2: silent-when-clean (guarded by .strata) + background index.
        assert!(post_cmd.contains(".strata"));
        assert!(post_cmd.contains("strata index"));
        // The DB path must be pinned to the project dir — `strata index <repo>`
        // alone resolves the DB relative to the hook's cwd, which Claude Code
        // does not guarantee to be the project root.
        assert!(
            post_cmd.contains("--db \"$d/.strata/graph.duckdb\""),
            "reindex must pin --db to the project dir: {post_cmd}"
        );
        assert!(post_cmd.contains("exit 0"));

        let start = &v["hooks"]["SessionStart"][0];
        let start_cmd = start["hooks"][0]["command"].as_str().unwrap();
        assert!(start_cmd.contains(HOOK_MARKER));
        // Only speaks when the DB is missing.
        assert!(start_cmd.contains("graph.duckdb"));
    }

    // ── PreToolUse pre-edit blast hook (Slice 20) — the robust enforcement ──

    #[test]
    fn pre_tool_use_hook_is_scoped_marked_and_computes_blast() {
        let tmp = TempDir::new().unwrap();
        install(tmp.path(), &ctx_db(), crate::init::InstallScope::Project).unwrap();
        let v: Value = serde_json::from_str(
            &std::fs::read_to_string(tmp.path().join(".claude/settings.json")).unwrap(),
        )
        .unwrap();

        let pre = &v["hooks"]["PreToolUse"][0];
        // Scoped to the edit tools, marked for idempotency.
        assert_eq!(pre["matcher"], "Edit|Write|MultiEdit");
        let cmd = pre["hooks"][0]["command"].as_str().unwrap();
        assert!(cmd.contains(HOOK_MARKER), "marker present: {cmd}");

        // Silent-when-clean: gated on the .strata directory being present (covers
        // both single-repo and estate members; single has .strata/graph.duckdb,
        // members have .strata/estate.toml + .strata/graph.duckdb).
        assert!(
            cmd.contains("[ -d \"$d/.strata\" ] || exit 0"),
            "must exit 0 silently when there is no .strata dir: {cmd}"
        );

        // It reads the edited file from the PreToolUse stdin JSON and COMPUTES the
        // blast via `strata blast … --format agent`.
        assert!(
            cmd.contains("tool_input.file_path"),
            "must read the edited file_path from stdin JSON: {cmd}"
        );
        assert!(
            cmd.contains("strata blast") && cmd.contains("--format agent"),
            "must compute the blast in the agent format: {cmd}"
        );

        // Non-blocking: injects additionalContext under the PreToolUse event, and
        // NEVER a permissionDecision (which would halt/loop the edit).
        assert!(
            cmd.contains("additionalContext") && cmd.contains("PreToolUse"),
            "must inject additionalContext: {cmd}"
        );
        assert!(
            !cmd.contains("permissionDecision"),
            "the hook must NOT emit a permissionDecision (never blocks/loops): {cmd}"
        );

        // Degrade-safe: exit 0 on every branch (no index, no path, empty, error).
        assert!(cmd.contains("exit 0"), "must exit 0 on every path: {cmd}");
        // Any `strata blast` failure or empty output → exit 0 silently.
        assert!(
            cmd.contains("|| exit 0"),
            "a strata failure must fall through to exit 0: {cmd}"
        );
    }

    #[test]
    fn pre_tool_use_hook_has_a_jq_optional_fallback() {
        // `jq` may be absent: the command must branch on `command -v jq` and, in
        // the else branch, still inject a STATIC advisory additionalContext so the
        // discipline is enforced even without a JSON parser.
        let group = pre_tool_use_group();
        let cmd = group["hooks"][0]["command"].as_str().unwrap();
        assert!(
            cmd.contains("command -v jq"),
            "must detect jq presence: {cmd}"
        );
        // The fallback advisory still names the impact-before-edit instruction and
        // still rides under additionalContext (no parser needed for a static string).
        assert!(
            cmd.contains("BEFORE editing") && cmd.contains("install jq"),
            "the jq-absent fallback must inject the static advisory: {cmd}"
        );
    }

    /// The pre-edit blast hook must NOT hardcode `--db "$db"` (Task 5 / Task 9:
    /// `strata blast` auto-resolves the estate). Presence guard must use `.strata/`
    /// directory, not the specific `.duckdb` file, so estate members (which have
    /// `.strata/estate.toml` + `.strata/graph.duckdb`) are also served.
    #[test]
    fn pre_edit_hook_drops_hardcoded_db_arg() {
        let group = pre_tool_use_group();
        let cmd = group["hooks"][0]["command"].as_str().unwrap();

        // Must NOT hardcode the --db path.
        assert!(
            !cmd.contains("--db \"$db\""),
            "pre-edit hook must NOT pass --db to blast (auto-resolve): {cmd}"
        );
        assert!(
            !cmd.contains("db=\"$d/.strata/graph.duckdb\""),
            "pre-edit hook must NOT declare the db variable: {cmd}"
        );

        // Must use the .strata DIRECTORY presence guard (not the .duckdb file).
        assert!(
            cmd.contains("[ -d \"$d/.strata\" ]"),
            "pre-edit hook must guard on .strata dir (not .duckdb file): {cmd}"
        );

        // Must still call blast with --format agent (without --db).
        assert!(
            cmd.contains("strata blast \"$f\" --format agent"),
            "pre-edit hook must call 'strata blast \"$f\" --format agent': {cmd}"
        );

        // Must still be non-blocking and degrade-safe.
        assert!(cmd.contains("exit 0"), "must exit 0 on every path: {cmd}");
        assert!(cmd.contains(HOOK_MARKER), "must carry hook marker: {cmd}");
    }

    #[test]
    fn pre_tool_use_hook_command_is_valid_shell() {
        // The command is a heavily-escaped `sh -c '…'`; a syntax error would make
        // every edit silently skip the blast. Parse-check it with `sh -n` (no
        // execution) so a malformed quoting is caught at build time.
        let group = pre_tool_use_group();
        let full = group["hooks"][0]["command"].as_str().unwrap();
        // Strip the trailing ` # strata-hook` marker comment and the `sh -c '…'`
        // wrapper to get the inner script, then syntax-check that.
        let inner = full
            .strip_suffix(&format!(" # {HOOK_MARKER}"))
            .expect("command ends with the marker comment");
        let inner = inner
            .strip_prefix("sh -c '")
            .and_then(|s| s.strip_suffix('\''))
            .expect("command is an `sh -c '…'` wrapper");
        // Inside single quotes a literal `'` cannot appear; sanity-check none leaked.
        assert!(
            !inner.contains('\''),
            "the inner sh script must not contain a bare single quote: {inner}"
        );
        let out = std::process::Command::new("sh")
            .arg("-n")
            .arg("-c")
            .arg(inner)
            .output()
            .expect("spawn sh -n");
        assert!(
            out.status.success(),
            "the pre-edit hook script must be valid shell; sh -n said: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn user_scope_writes_global_artifacts_and_no_mcp_json() {
        let home = TempDir::new().unwrap();
        let ctx = RepoContext { mcp_args: vec![], identity: Identity::Global };
        let reports = install_files(home.path(), &ctx, crate::init::InstallScope::User).unwrap();

        let claude = home.path().join(".claude");
        assert!(claude.join("CLAUDE.md").exists(), "global steering at ~/.claude/CLAUDE.md");
        assert!(!home.path().join(".mcp.json").exists(), "no .mcp.json for global");
        assert!(!home.path().join("CLAUDE.md").exists(), "no ~/CLAUDE.md (home root)");
        assert!(claude.join("settings.json").exists());
        assert!(claude.join("skills/strata/strata-guide/SKILL.md").exists());

        // Steering is the generic block (no repo identity counts).
        let steering = std::fs::read_to_string(claude.join("CLAUDE.md")).unwrap();
        assert!(steering.contains("installed globally by StrataGraph"));
        assert!(!steering.contains("indexed by StrataGraph as"));

        // SessionStart is the guarded variant (silent in non-Strata repos).
        // Parse the JSON and locate the StrataGraph SessionStart entry by the
        // strata-hook marker so we assert on the specific command, not just any
        // substring that happens to appear elsewhere in the file.
        let settings = std::fs::read_to_string(claude.join("settings.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&settings).unwrap();
        let ss_arr = v["hooks"]["SessionStart"]
            .as_array()
            .expect("SessionStart must be an array");
        let ss_cmd = ss_arr
            .iter()
            .find_map(|group| {
                group["hooks"].as_array()?.iter().find_map(|h| {
                    let cmd = h["command"].as_str()?;
                    if cmd.contains(HOOK_MARKER) { Some(cmd) } else { None }
                })
            })
            .expect("StrataGraph SessionStart hook (by strata-hook marker) must be present");
        assert!(
            ss_cmd.contains(r#"[ -d "$d/.strata" ] || exit 0"#),
            "user-scope SessionStart must be the .strata-guarded variant, got: {ss_cmd}"
        );

        // All files in the report were created.
        assert!(!reports.is_empty());
    }

    // ── register_user_mcp (Task 5) ───────────────────────────────────────────

    #[cfg(unix)]
    fn ok_output() -> std::process::Output {
        use std::os::unix::process::ExitStatusExt;
        std::process::Output {
            status: std::process::ExitStatus::from_raw(0),
            stdout: vec![],
            stderr: vec![],
        }
    }

    #[cfg(unix)]
    fn fail_output(stderr: &str) -> std::process::Output {
        use std::os::unix::process::ExitStatusExt;
        std::process::Output {
            status: std::process::ExitStatus::from_raw(256),
            stdout: vec![],
            stderr: stderr.as_bytes().to_vec(),
        }
    }

    #[cfg(unix)]
    #[test]
    fn register_user_mcp_runs_remove_then_add_with_exact_args() {
        use std::sync::{Arc, Mutex};
        let calls: Arc<Mutex<Vec<Vec<String>>>> = Arc::new(Mutex::new(vec![]));
        let c = calls.clone();
        let run = move |args: &[&str]| {
            c.lock().unwrap().push(args.iter().map(|s| s.to_string()).collect());
            Ok(ok_output())
        };
        register_user_mcp(run).unwrap();
        let calls = calls.lock().unwrap();
        assert_eq!(calls[0], vec!["mcp", "remove", "strata", "--scope", "user"]);
        assert_eq!(
            calls[1],
            vec!["mcp", "add", "strata", "--scope", "user", "--", "strata", "mcp"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn register_user_mcp_claude_absent_is_a_clear_error() {
        let run = |_: &[&str]| Err(std::io::Error::from(std::io::ErrorKind::NotFound));
        let err = register_user_mcp(run).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("claude") && msg.to_lowercase().contains("not"), "got: {msg}");
    }

    #[cfg(unix)]
    #[test]
    fn register_user_mcp_add_failure_surfaces_stderr() {
        use std::sync::{Arc, Mutex};
        let n = Arc::new(Mutex::new(0));
        let run = move |_: &[&str]| {
            let mut k = n.lock().unwrap();
            *k += 1;
            if *k == 1 { Ok(ok_output()) } else { Ok(fail_output("boom")) }
        };
        let err = register_user_mcp(run).unwrap_err();
        assert!(err.to_string().contains("boom"));
    }

    // ── existing idempotency tests ───────────────────────────────────────────

    #[test]
    fn second_install_is_all_unchanged() {
        let tmp = TempDir::new().unwrap();
        install(tmp.path(), &ctx_db(), crate::init::InstallScope::Project).unwrap();
        let second = install(tmp.path(), &ctx_db(), crate::init::InstallScope::Project).unwrap();
        assert!(
            second.iter().all(|r| r.outcome == Outcome::Unchanged),
            "idempotent re-run, got {second:?}"
        );
    }

    #[test]
    fn install_preserves_foreign_mcp_server_and_foreign_hook() {
        let tmp = TempDir::new().unwrap();
        // Pre-seed a foreign MCP server and a foreign Claude hook.
        std::fs::write(
            tmp.path().join(".mcp.json"),
            r#"{ "mcpServers": { "gitnexus": { "command": "npx", "args": ["gitnexus", "mcp"] } } }"#,
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join(".claude")).unwrap();
        std::fs::write(
            tmp.path().join(".claude/settings.json"),
            r#"{ "hooks": { "PostToolUse": [ { "matcher": "Bash", "hooks": [ { "type": "command", "command": "echo foreign-bash" } ] } ] } }"#,
        )
        .unwrap();

        install(tmp.path(), &ctx_db(), crate::init::InstallScope::Project).unwrap();

        let mcp: Value =
            serde_json::from_str(&std::fs::read_to_string(tmp.path().join(".mcp.json")).unwrap())
                .unwrap();
        assert_eq!(mcp["mcpServers"]["gitnexus"]["command"], "npx");
        assert_eq!(mcp["mcpServers"]["strata"]["command"], "strata");

        let settings: Value = serde_json::from_str(
            &std::fs::read_to_string(tmp.path().join(".claude/settings.json")).unwrap(),
        )
        .unwrap();
        let post = settings["hooks"]["PostToolUse"].as_array().unwrap();
        // Foreign Bash hook still there, ours added alongside it.
        assert!(
            post.iter()
                .any(|g| g["matcher"] == "Bash" && g["hooks"][0]["command"] == "echo foreign-bash"),
            "foreign Bash hook must be preserved: {post:?}"
        );
        assert!(
            post.iter().any(|g| g["matcher"] == "Edit|Write|MultiEdit"),
            "StrataGraph hook must be added: {post:?}"
        );
    }
}
