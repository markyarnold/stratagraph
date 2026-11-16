//! `strata init kiro-crew` — the Kiro kit plus the repo-committed project
//! layer Kiro Crew reads.
//!
//! A SUPERSET of [`super::kiro`], not a parallel kit: every file the Kiro
//! installer writes is one Kiro Crew also reads (`.kiro/steering/strata.md`
//! through the agent's steering resource, `.kiro/settings/mcp.json` through
//! the spec's `includeMcpJson`), so this installer runs that one first and
//! then adds the two things only a Crew/Kiro-CLI session picks up:
//!
//! * `.kiro/skills/<slug>/SKILL.md` — the same four task-routed skills the
//!   Claude kit installs, as project skills. Kiro walks
//!   `.kiro/skills/*/SKILL.md`, exactly one level, so each slug is a direct
//!   child; a nested `strata/<slug>` layout (what the Claude kit uses under
//!   `.claude/skills/`) would never be discovered.
//! * `.kiro/agents/strata.json` — a workspace-local custom agent carrying
//!   the MCP server, the steering and skill resources, and three advisory
//!   hooks on the events that cannot refuse a tool call.
//!
//! Two contracts from the Crew hook model shape the hooks here
//! (<https://kiro.dev/docs/crew/capabilities/hooks/>):
//!
//! * **No `preToolUse`.** That event fails closed — exit 2 denies the call,
//!   and so does any other non-zero exit, timeout or crash. The engine's
//!   advice is advisory, so it rides `agentSpawn`, `postToolUse` and `stop`,
//!   where a non-zero exit only records a warning.
//! * **A hook command cannot read a project-directory variable.** The
//!   subprocess gets a strict environment allowlist, so every command here
//!   is relative to its cwd (the session workspace) rather than reaching for
//!   the `$CLAUDE_PROJECT_DIR` the Claude kit's own hooks use.
//!
//! The `stop` hook earns its place: [`super::content::KIRO_ROUTING`] records
//! that Kiro has no commit-specific trigger, so the commit-time
//! `detect_changes` check has had to be a rule the agent remembers. `stop`
//! fires when a turn ends, which is the closest mechanical hook to "before
//! you commit" the surface offers.

use std::path::Path;

use serde_json::{json, Value};

use super::writers::{self, WriteError};
use super::{content, FileReport, KiroVersion, RepoContext};

/// The project agent spec's file stem under `.kiro/agents/` — also the
/// agent's own `name`, which is how a user selects it (`kiro-cli --agent
/// strata`, or the IDE's agent picker).
pub const AGENT_STEM: &str = "strata";

/// Install the Kiro kit, then the Crew project layer, into `root`.
///
/// `version` is forwarded to [`super::kiro::install`] for the standalone
/// hook files' format; it does not affect the project layer, whose two file
/// shapes (skill markdown, agent JSON) are version-independent.
pub fn install(
    root: &Path,
    ctx: &RepoContext,
    version: KiroVersion,
    scope: crate::init::InstallScope,
) -> Result<Vec<FileReport>, WriteError> {
    // The Kiro installer owns the `--scope user` refusal, so no need to
    // repeat it here: a user-scope run fails there before anything is written.
    let mut reports = super::kiro::install(root, ctx, version, scope)?;

    // The skill bodies are written VERBATIM, frontmatter included. They
    // already carry the `name` and `description` a Kiro project skill needs;
    // `triggers` is optional (consulted only when trigger injection is
    // enabled) and `always` defaults to false, which is what we want — the
    // always-on channel for this content is the steering block the Kiro
    // installer just wrote. Writing them unchanged keeps one copy of the
    // skill text for every surface.
    for (slug, body) in content::skills() {
        let rel = format!(".kiro/skills/{slug}/SKILL.md");
        let outcome = writers::write_owned(&root.join(&rel), &body)?;
        reports.push(FileReport::new(rel, outcome));
    }

    let rel = format!(".kiro/agents/{AGENT_STEM}.json");
    let mut body =
        serde_json::to_string_pretty(&agent_spec(&ctx.mcp_args)).map_err(|e| WriteError::Io {
            path: rel.clone(),
            detail: format!("rendering the agent spec: {e}"),
        })?;
    body.push('\n');
    let outcome = writers::write_owned(&root.join(&rel), &body)?;
    reports.push(FileReport::new(rel, outcome));

    Ok(reports)
}

/// One Crew / Kiro-CLI agent-spec hook command.
///
/// Guarded on `./.strata` (silent outside a StrataGraph repo), guarded on
/// `command -v strata` (silent when the binary is absent — the hook
/// environment carries `PATH` but the machine need not have the CLI), and
/// always `exit 0`. Relative to cwd throughout: see the module docs on why
/// `$CLAUDE_PROJECT_DIR` is unavailable here.
fn hook_command(inner: &str) -> String {
    format!(
        "sh -c '[ -d ./.strata ] || exit 0; command -v strata >/dev/null 2>&1 || exit 0; \
         {inner}; exit 0'"
    )
}

/// The `agentSpawn` hook: the same silent-when-clean guidance line the
/// Claude kit's SessionStart hook prints — only when `.strata/` exists but
/// the graph does not, so an unindexed repo says so once and an indexed one
/// stays quiet. Stdout on `agentSpawn` enters the model context.
fn agent_spawn_command() -> String {
    hook_command(
        "[ -f ./.strata/graph.duckdb ] && exit 0; \
         echo \"StrataGraph: no index yet, run strata index . to enable cross-plane impact analysis.\"",
    )
}

/// The `postToolUse` hook: after a write, keep the on-disk index fresh in the
/// background — the engine's own stay-fresh behaviour, the same as the Claude
/// kit's PostToolUse hook and the standalone `strata-post-edit` Kiro hook.
///
/// The matcher is a GLOB, not a regex: Crew matches a tool event's matcher
/// with fnmatch, so the standalone hook's `fs_write|str_replace|fs_append`
/// alternation would match nothing here. One name suffices — Crew resolves
/// `str_replace`, `fs_append` and `delete_file` onto the same `fs_write` row.
fn post_tool_use_command() -> String {
    hook_command("(strata index . --db ./.strata/graph.duckdb >/dev/null 2>&1 &)")
}

/// The `stop` hook: when a turn ends, name the commit-time check the Kiro
/// surface has no trigger for.
///
/// Plain stdout, never the `{"decision":"block"}` form a `stop` hook may
/// return to request another turn: this is a reminder, not a failure.
fn stop_command() -> String {
    hook_command(
        "printf \"%s\\n\" \"StrataGraph: before you create a git commit, run detect_changes (or strata detect-changes) and report its per-plane affected set and risk — pause on HIGH/CRITICAL or a change that crosses a repo boundary. Advisory only.\"",
    )
}

/// The `.kiro/agents/strata.json` project agent spec.
///
/// A workspace-local custom agent: Kiro resolves `.kiro/agents/` before
/// `~/.kiro/agents/`, with no upward walk, so this is what a session started
/// at the repo root sees.
///
/// `mcpServers.strata` is declared on the spec ITSELF, not only in
/// `.kiro/settings/mcp.json`, because a spec's own servers are the one
/// channel that reaches every backend; `includeMcpJson: true` keeps the
/// workspace and user `mcp.json` servers as well, so this ADDS the engine's
/// server without narrowing what the user already had.
///
/// Deliberately absent: `model`, `tools` and `excludedTools`. Pinning a
/// model or narrowing the tool set in a committed file would override the
/// user's own choice for every session in the repo.
fn agent_spec(mcp_args: &[String]) -> Value {
    json!({
        "name": AGENT_STEM,
        "description": "Cross-plane impact analysis with StrataGraph: query the code graph, \
                        check a change's blast radius before you make it, and report risk \
                        before you commit.",
        "mcpServers": {
            "strata": { "command": "strata", "args": mcp_args }
        },
        "includeMcpJson": true,
        "resources": [
            "file://.kiro/steering/**/*.md",
            "skill://.kiro/skills/**/SKILL.md",
        ],
        "permissions": {
            "rules": [
                { "capability": "mcp", "match": ["strata/*"], "effect": "allow" }
            ]
        },
        "hooks": {
            "agentSpawn": [{
                "command": agent_spawn_command(),
                "description": "Say so once when the repo has no graph yet.",
            }],
            "postToolUse": [{
                "matcher": "fs_write",
                "command": post_tool_use_command(),
                "description": "After a write, refresh the on-disk graph in the background.",
            }],
            "stop": [{
                "command": stop_command(),
                "description": "Name the commit-time detect_changes check Kiro has no trigger for.",
            }],
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::init::content::Identity;
    use crate::init::writers::Outcome;
    use tempfile::TempDir;

    fn ctx_db() -> RepoContext {
        RepoContext {
            mcp_args: vec!["mcp".into(), "--db".into(), crate::DEFAULT_DB.into()],
            identity: Identity::NotIndexed,
        }
    }

    fn install_project(root: &Path) -> Vec<FileReport> {
        install(
            root,
            &ctx_db(),
            KiroVersion::New,
            crate::init::InstallScope::Project,
        )
        .unwrap()
    }

    /// The whole Kiro kit, plus the project layer — a superset, proven by
    /// asserting the Kiro installer's own files are still reported.
    #[test]
    fn installs_the_kiro_kit_plus_the_crew_project_layer() {
        let tmp = TempDir::new().unwrap();
        let reports = install_project(tmp.path());
        let paths: Vec<String> = reports
            .iter()
            .map(|r| r.path.display().to_string().replace('\\', "/"))
            .collect();

        for rel in [
            ".kiro/settings/mcp.json",
            ".kiro/steering/strata.md",
            ".kiro/hooks/strata-pre-edit.json",
            ".kiro/hooks/strata-post-edit.json",
        ] {
            assert!(paths.contains(&rel.to_string()), "{rel} missing: {paths:?}");
        }
        for rel in [
            ".kiro/skills/strata-guide/SKILL.md",
            ".kiro/skills/strata-exploring/SKILL.md",
            ".kiro/skills/strata-impact-analysis/SKILL.md",
            ".kiro/skills/strata-contracts-and-infra/SKILL.md",
            ".kiro/agents/strata.json",
        ] {
            assert!(paths.contains(&rel.to_string()), "{rel} missing: {paths:?}");
            assert!(tmp.path().join(rel).exists(), "{rel} not on disk");
        }
    }

    /// Kiro walks `.kiro/skills/*/SKILL.md` — exactly one level. A slug
    /// carrying a separator would nest the skill where it is never found, so
    /// the Claude kit's own `strata/<slug>` nesting must not leak over.
    #[test]
    fn project_skills_sit_one_level_under_kiro_skills() {
        for (slug, _) in content::skills() {
            assert!(
                !slug.contains('/') && !slug.contains('\\'),
                "slug {slug:?} must be one path segment"
            );
        }
    }

    /// The bodies are written verbatim, so each must already be a valid
    /// project skill: frontmatter naming itself.
    #[test]
    fn project_skill_bodies_carry_frontmatter_naming_themselves() {
        let tmp = TempDir::new().unwrap();
        install_project(tmp.path());
        for (slug, _) in content::skills() {
            let body =
                std::fs::read_to_string(tmp.path().join(format!(".kiro/skills/{slug}/SKILL.md")))
                    .unwrap();
            assert!(
                body.starts_with(&format!("---\nname: {slug}\n")),
                "{slug} must open with its own frontmatter: {body:.80}"
            );
            assert!(body.contains("description:"), "{slug}");
        }
    }

    #[test]
    fn agent_spec_is_valid_json_declaring_our_server_and_keeping_the_users_own() {
        let tmp = TempDir::new().unwrap();
        install_project(tmp.path());
        let spec: Value = serde_json::from_str(
            &std::fs::read_to_string(tmp.path().join(".kiro/agents/strata.json")).unwrap(),
        )
        .expect("the agent spec must be valid JSON");

        assert_eq!(spec["name"], AGENT_STEM);
        assert_eq!(spec["mcpServers"]["strata"]["command"], "strata");
        assert_eq!(spec["mcpServers"]["strata"]["args"][0], "mcp");
        assert_eq!(spec["includeMcpJson"], true);
        let resources: Vec<&str> = spec["resources"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(resources.contains(&"file://.kiro/steering/**/*.md"));
        assert!(resources.contains(&"skill://.kiro/skills/**/SKILL.md"));
        for key in ["model", "tools", "excludedTools"] {
            assert!(spec.get(key).is_none(), "spec must not carry {key}");
        }
    }

    /// The load-bearing assertion: a `preToolUse` hook FAILS CLOSED in Crew,
    /// so the engine's advisory kit must never put one there.
    #[test]
    fn agent_spec_declares_only_the_three_observer_event_hooks() {
        let hooks = agent_spec(&["mcp".to_string()])["hooks"]
            .as_object()
            .unwrap()
            .clone();
        assert!(
            !hooks.contains_key("preToolUse"),
            "preToolUse fails closed in Crew — this kit must stay advisory"
        );
        assert_eq!(hooks.len(), 3, "{:?}", hooks.keys().collect::<Vec<_>>());
        for event in ["agentSpawn", "postToolUse", "stop"] {
            assert!(hooks.contains_key(event), "missing {event}");
        }
    }

    /// Crew matches a tool event's matcher with fnmatch, so a regex
    /// alternation would match no tool at all.
    #[test]
    fn post_tool_use_matcher_is_a_glob_not_a_regex_alternation() {
        let spec = agent_spec(&["mcp".to_string()]);
        let matcher = spec["hooks"]["postToolUse"][0]["matcher"].as_str().unwrap();
        assert_eq!(matcher, "fs_write");
        assert!(!matcher.contains('|'), "{matcher} would never match");
    }

    #[test]
    fn non_tool_event_hooks_carry_no_matcher() {
        let spec = agent_spec(&["mcp".to_string()]);
        for event in ["agentSpawn", "stop"] {
            assert!(
                spec["hooks"][event][0].get("matcher").is_none(),
                "{event} must not carry a matcher Crew would drop"
            );
        }
    }

    /// Every hook command: valid shell, guarded both ways, cwd-relative, and
    /// always `exit 0`.
    #[test]
    fn every_hook_command_is_valid_shell_guarded_and_exits_zero() {
        for (name, command) in [
            ("agentSpawn", agent_spawn_command()),
            ("postToolUse", post_tool_use_command()),
            ("stop", stop_command()),
        ] {
            let inner = command
                .strip_prefix("sh -c '")
                .and_then(|s| s.strip_suffix('\''))
                .unwrap_or_else(|| panic!("{name} must be an `sh -c '…'` wrapper: {command}"));
            assert!(
                !inner.contains('\''),
                "{name}: the inner script must not contain a bare single quote: {inner}"
            );
            let out = std::process::Command::new("sh")
                .arg("-n")
                .arg("-c")
                .arg(inner)
                .output()
                .expect("spawn sh -n");
            assert!(
                out.status.success(),
                "{name} must be valid shell; sh -n said: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            assert!(
                command.contains("[ -d ./.strata ] || exit 0"),
                "{name} must be .strata-guarded, relative to cwd: {command}"
            );
            assert!(
                command.contains("command -v strata >/dev/null 2>&1 || exit 0"),
                "{name} must be silent when strata is not installed: {command}"
            );
            assert!(
                command.ends_with("exit 0'"),
                "{name} must always exit 0: {command}"
            );
            assert!(
                !command.contains("CLAUDE_PROJECT_DIR"),
                "{name} must not read a project-dir variable Crew does not set: {command}"
            );
        }
    }

    #[test]
    fn stop_hook_never_requests_another_turn() {
        let command = stop_command();
        assert!(
            !command.contains("decision"),
            "must not return the block-decision form: {command}"
        );
        assert!(command.contains("detect_changes"), "{command}");
    }

    #[test]
    fn rerun_is_idempotent_every_file_unchanged() {
        let tmp = TempDir::new().unwrap();
        install_project(tmp.path());
        let second = install_project(tmp.path());
        for r in &second {
            assert_eq!(
                r.outcome,
                Outcome::Unchanged,
                "{} must be unchanged on a re-run",
                r.path.display()
            );
        }
    }
}
