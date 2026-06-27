//! Claude Code artifact renderer for `strata init claude`.
//!
//! Writes, all merge-safely:
//! * `.mcp.json` — merge-add `mcpServers.strata` with the detected launch args;
//! * `CLAUDE.md` + `AGENTS.md` — the same managed steering block (spec §2);
//! * `.claude/skills/strata/<slug>/SKILL.md` — the four task-routed skills;
//! * `.claude/settings.json` — scoped, silent-when-clean hooks (R1/R2), each
//!   carrying the `strata-hook` marker token for structural idempotency.

use std::path::Path;

use serde_json::{json, Value};

use super::content::{self};
use super::writers::{self, hooks_event_array, upsert_hook, WriteError, HOOK_MARKER};
use super::{FileReport, RepoContext};

/// Install the Claude Code kit under `root`, returning a [`FileReport`] per file.
pub fn install(root: &Path, ctx: &RepoContext, _scope: crate::init::InstallScope) -> Result<Vec<FileReport>, WriteError> {
    let mut reports = Vec::new();

    // 1. .mcp.json — merge-add mcpServers.strata.
    let mcp_path = root.join(".mcp.json");
    let server = mcp_server_value(&ctx.mcp_args);
    let outcome = writers::merge_json(&mcp_path, &json!({ "mcpServers": { "strata": server } }))?;
    reports.push(FileReport::new(".mcp.json", outcome));

    // 2. CLAUDE.md + AGENTS.md — the same managed steering block.
    let block = content::render_steering_block(&ctx.identity, content::CLAUDE_ROUTING);
    for fname in ["CLAUDE.md", "AGENTS.md"] {
        let outcome = writers::upsert_managed_block(&root.join(fname), &block)?;
        reports.push(FileReport::new(fname, outcome));
    }

    // 3. Skills — wholly owned files.
    for (slug, body) in content::skills() {
        let rel = format!(".claude/skills/strata/{slug}/SKILL.md");
        let outcome = writers::write_owned(&root.join(&rel), &body)?;
        reports.push(FileReport::new(rel, outcome));
    }

    // 4. .claude/settings.json — scoped silent-when-clean hooks.
    let settings_path = root.join(".claude/settings.json");
    let outcome = writers::edit_json(&settings_path, install_hooks)?;
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
/// * **SessionStart** → R1: a single guidance line *only* when the graph DB is
///   missing, silent otherwise.
fn install_hooks(root: &mut Value) {
    if let Some(arr) = hooks_event_array(root, "PreToolUse") {
        upsert_hook(arr, pre_tool_use_group());
    }
    if let Some(arr) = hooks_event_array(root, "PostToolUse") {
        upsert_hook(arr, post_tool_use_group());
    }
    if let Some(arr) = hooks_event_array(root, "SessionStart") {
        upsert_hook(arr, session_start_group());
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
