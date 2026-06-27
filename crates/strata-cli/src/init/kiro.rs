//! Kiro artifact renderer for `strata init kiro`.
//!
//! Writes, all merge-safely:
//! * `.kiro/settings/mcp.json` — merge-add `mcpServers.strata` (same rules as Claude);
//! * `.kiro/steering/strata.md` — the managed steering block (Kiro-native steering,
//!   an improvement over GitNexus which only wrote CLAUDE/AGENTS);
//! * the three lifecycle hooks, in one of two Kiro hook formats selected by
//!   [`KiroVersion`] (Kiro changed its hook schema between releases):
//!   - [`KiroVersion::Old`] (the default): legacy `.kiro/hooks/strata-*.kiro.hook`
//!     files, a `when`/`then` shape (`{ enabled, when:{type,toolTypes}, then:{…} }`).
//!   - [`KiroVersion::New`]: `.kiro/hooks/strata-*.json` files, a
//!     `{ version:"v1", hooks:[{ trigger, matcher, action }] }` envelope.
//!
//! The two formats carry the SAME hook data (names, prompts, the `detect_changes`
//! pre-commit check, the reindex command); only the envelope and file extension
//! differ. Installing one version removes the other version's StrataGraph hook files,
//! so the two never coexist (a stale hook in the format Kiro no longer reads is
//! dead weight; one in the format it DOES read would double-fire).

use std::path::Path;

use serde_json::{json, Value};

use super::content::{self};
use super::writers::{self, WriteError};
use super::{FileReport, RepoContext};

/// Which Kiro hook-file format to emit. Kiro changed its hook schema between
/// releases; this selects the matching format so a user on either Kiro version
/// gets hooks their Kiro accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum KiroVersion {
    /// Legacy `.kiro.hook` files with a `when`/`then` shape (Kiro before the
    /// schema change). The default.
    #[default]
    Old,
    /// Newer `.json` files with a `{ version:"v1", hooks:[…] }` envelope.
    New,
}

impl KiroVersion {
    /// Parse the `--kiro-version` value, returning `None` for an unknown value.
    pub fn parse(s: &str) -> Option<KiroVersion> {
        match s {
            "old" | "legacy" => Some(KiroVersion::Old),
            "new" => Some(KiroVersion::New),
            _ => None,
        }
    }

    /// Accepted `--kiro-version` values, for help text and error messages.
    pub const SUPPORTED: &'static [&'static str] = &["old", "new"];

    /// The file extension this version's hook files use.
    fn hook_ext(self) -> &'static str {
        match self {
            KiroVersion::Old => "kiro.hook",
            KiroVersion::New => "json",
        }
    }

    /// The other format's extension (for cleaning up a stale install).
    fn other_ext(self) -> &'static str {
        match self {
            KiroVersion::Old => KiroVersion::New.hook_ext(),
            KiroVersion::New => KiroVersion::Old.hook_ext(),
        }
    }
}

/// Whether a hook fires before or after a tool runs.
#[derive(Clone, Copy)]
enum Trigger {
    Pre,
    Post,
}

/// What a hook does when it fires.
enum HookAction {
    /// Ask the agent with a prompt (`askAgent` in old, `agent` in new).
    Agent(&'static str),
    /// Run a shell command with a timeout (`runCommand` in old, `command` in new).
    Command { command: &'static str, timeout: u64 },
}

/// One lifecycle hook, defined once and rendered into either format. The data is
/// shared; only the envelope differs. The selector differs between schemas: the
/// old format uses `when.toolTypes` (a list), the new uses a `matcher` regex
/// (these even differ in value for the pre-edit hook, so both are carried).
struct HookSpec {
    /// File stem, e.g. `strata-pre-edit`.
    stem: &'static str,
    name: &'static str,
    description: &'static str,
    trigger: Trigger,
    /// Old-format `when.toolTypes` value.
    old_tool_types: &'static [&'static str],
    /// New-format `matcher` regex.
    new_matcher: &'static str,
    action: HookAction,
}

const PRE_EDIT_PROMPT: &str = "STOP: StrataGraph workflow check. Before writing to this file, confirm you have assessed its blast radius: run the blast tool ({ file }) for the whole-file view, or impact({ symbol }) for EVERY symbol, field, or operation you are about to modify. For any GraphQL field / API operation, also run context({ symbol }) and check its producers (implementers) and consumers (callers) buckets. If you have NOT done this yet, do it now before proceeding. State which symbols were analysed, their d=1/d=2 dependents, confidence bands, and risk level. NEVER present uncertain impact (<0.40 or ambiguous) as certain; say so explicitly. If risk is HIGH/CRITICAL or the change crosses a repo boundary, pause for user direction.";

const PRE_COMMIT_PROMPT: &str = "STOP: StrataGraph pre-commit check. Before committing, run the detect_changes tool (pass staged:true if you have staged a partial commit). It git-diffs your work, derives the changed symbols PER PLANE (code; contract producers/consumers; infra), aggregates the blast radius across the graph, and returns the overall risk level (LOW/MEDIUM/HIGH/CRITICAL) with reasons. Report its affected set and risk, and proceed with the commit ONLY if that scope matches your intent. If risk is HIGH/CRITICAL, crosses a repo boundary, or touches contract surface consumed elsewhere, pause for user direction first.";

/// The three lifecycle hooks. Prompts are identical across both schemas.
fn hook_specs() -> [HookSpec; 3] {
    [
        HookSpec {
            stem: "strata-pre-edit",
            name: "StrataGraph Pre-Edit Impact Check",
            description: "Before any file write, confirms impact analysis was run for every symbol/field about to change, across all planes.",
            trigger: Trigger::Pre,
            old_tool_types: &["write"],
            new_matcher: "fs_write|str_replace|fs_append",
            action: HookAction::Agent(PRE_EDIT_PROMPT),
        },
        HookSpec {
            stem: "strata-pre-commit",
            name: "StrataGraph Pre-Commit Scope Check",
            description: "Before committing, runs detect_changes for the per-plane changed symbols, blast radius, and risk.",
            trigger: Trigger::Pre,
            old_tool_types: &[".*git_add_or_commit.*"],
            new_matcher: ".*git_add_or_commit.*",
            action: HookAction::Agent(PRE_COMMIT_PROMPT),
        },
        HookSpec {
            stem: "strata-post-commit",
            name: "StrataGraph Post-Commit Reindex",
            description: "After a git commit, re-runs strata index to keep the on-disk graph fresh.",
            trigger: Trigger::Post,
            old_tool_types: &[".*git_add_or_commit.*"],
            new_matcher: ".*git_add_or_commit.*",
            action: HookAction::Command {
                command: "strata index .",
                timeout: 120,
            },
        },
    ]
}

/// Render a hook spec into `(relative_path, json_value)` for the given version.
fn render_hook(version: KiroVersion, spec: &HookSpec) -> (String, Value) {
    let rel = format!(".kiro/hooks/{}.{}", spec.stem, version.hook_ext());
    let value = match version {
        KiroVersion::Old => {
            let when = json!({
                "type": match spec.trigger { Trigger::Pre => "preToolUse", Trigger::Post => "postToolUse" },
                "toolTypes": spec.old_tool_types,
            });
            let then = match &spec.action {
                HookAction::Agent(prompt) => json!({ "type": "askAgent", "prompt": prompt }),
                HookAction::Command { command, timeout } => {
                    json!({ "type": "runCommand", "command": command, "timeout": timeout })
                }
            };
            json!({
                "enabled": true,
                "name": spec.name,
                "description": spec.description,
                "version": "1",
                "when": when,
                "then": then,
            })
        }
        KiroVersion::New => {
            let action = match &spec.action {
                HookAction::Agent(prompt) => json!({ "type": "agent", "prompt": prompt }),
                HookAction::Command { command, timeout } => {
                    json!({ "type": "command", "command": command, "timeout": timeout })
                }
            };
            json!({
                "version": "v1",
                "hooks": [{
                    "name": spec.name,
                    "description": spec.description,
                    "trigger": match spec.trigger { Trigger::Pre => "PreToolUse", Trigger::Post => "PostToolUse" },
                    "matcher": spec.new_matcher,
                    "action": action,
                }],
            })
        }
    };
    (rel, value)
}

/// Install the Kiro kit under `root` in the given hook-format `version`,
/// returning a [`FileReport`] per file.
pub fn install(
    root: &Path,
    ctx: &RepoContext,
    version: KiroVersion,
) -> Result<Vec<FileReport>, WriteError> {
    let mut reports = Vec::new();

    // 1. .kiro/settings/mcp.json — merge-add mcpServers.strata.
    let mcp_path = root.join(".kiro/settings/mcp.json");
    let server = json!({ "command": "strata", "args": ctx.mcp_args });
    let outcome = writers::merge_json(&mcp_path, &json!({ "mcpServers": { "strata": server } }))?;
    reports.push(FileReport::new(".kiro/settings/mcp.json", outcome));

    // 2. .kiro/steering/strata.md — the managed steering block (Kiro routing).
    let block = content::render_steering_block(&ctx.identity, content::KIRO_ROUTING);
    let outcome = writers::upsert_managed_block(&root.join(".kiro/steering/strata.md"), &block)?;
    reports.push(FileReport::new(".kiro/steering/strata.md", outcome));

    let specs = hook_specs();

    // 3. Remove the OTHER version's StrataGraph hook files so the two formats never
    //    coexist (best-effort: a missing file is fine).
    for spec in &specs {
        let stale = root.join(format!(".kiro/hooks/{}.{}", spec.stem, version.other_ext()));
        if stale.exists() {
            let _ = std::fs::remove_file(&stale);
        }
    }

    // 4. The three lifecycle hooks in the selected format — wholly-owned files.
    for spec in &specs {
        let (rel, hook) = render_hook(version, spec);
        let mut body = serde_json::to_string_pretty(&hook).map_err(|e| WriteError::Io {
            path: rel.clone(),
            detail: e.to_string(),
        })?;
        body.push('\n');
        let outcome = writers::write_owned(&root.join(&rel), &body)?;
        reports.push(FileReport::new(rel, outcome));
    }

    Ok(reports)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::init::content::Identity;
    use tempfile::TempDir;

    fn ctx_db() -> RepoContext {
        RepoContext {
            mcp_args: vec!["mcp".into(), "--db".into(), crate::DEFAULT_DB.into()],
            identity: Identity::NotIndexed,
        }
    }

    fn read_json(p: &std::path::Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
    }

    #[test]
    fn version_parse_and_default() {
        assert_eq!(KiroVersion::default(), KiroVersion::Old);
        assert_eq!(KiroVersion::parse("old"), Some(KiroVersion::Old));
        assert_eq!(KiroVersion::parse("legacy"), Some(KiroVersion::Old));
        assert_eq!(KiroVersion::parse("new"), Some(KiroVersion::New));
        assert_eq!(KiroVersion::parse("v2"), None);
    }

    #[test]
    fn old_version_writes_legacy_kiro_hook_shape() {
        let tmp = TempDir::new().unwrap();
        let reports = install(tmp.path(), &ctx_db(), KiroVersion::Old).unwrap();
        for rel in [
            ".kiro/settings/mcp.json",
            ".kiro/steering/strata.md",
            ".kiro/hooks/strata-pre-edit.kiro.hook",
            ".kiro/hooks/strata-pre-commit.kiro.hook",
            ".kiro/hooks/strata-post-commit.kiro.hook",
        ] {
            assert!(tmp.path().join(rel).exists(), "expected {rel}");
        }
        assert!(reports
            .iter()
            .all(|r| r.outcome == writers::Outcome::Created));

        let pre_edit = read_json(&tmp.path().join(".kiro/hooks/strata-pre-edit.kiro.hook"));
        assert_eq!(pre_edit["version"], "1");
        assert_eq!(pre_edit["when"]["type"], "preToolUse");
        assert_eq!(pre_edit["when"]["toolTypes"][0], "write");
        assert_eq!(pre_edit["then"]["type"], "askAgent");
        assert!(pre_edit["then"]["prompt"]
            .as_str()
            .unwrap()
            .contains("STOP"));

        let pre_commit = read_json(&tmp.path().join(".kiro/hooks/strata-pre-commit.kiro.hook"));
        assert!(pre_commit["then"]["prompt"]
            .as_str()
            .unwrap()
            .contains("detect_changes"));

        let post = read_json(&tmp.path().join(".kiro/hooks/strata-post-commit.kiro.hook"));
        assert_eq!(post["when"]["type"], "postToolUse");
        assert_eq!(post["then"]["type"], "runCommand");
        assert_eq!(post["then"]["command"], "strata index .");
        assert_eq!(post["then"]["timeout"], 120);
    }

    #[test]
    fn new_version_writes_v1_json_envelope() {
        let tmp = TempDir::new().unwrap();
        install(tmp.path(), &ctx_db(), KiroVersion::New).unwrap();
        for rel in [
            ".kiro/hooks/strata-pre-edit.json",
            ".kiro/hooks/strata-pre-commit.json",
            ".kiro/hooks/strata-post-commit.json",
        ] {
            assert!(tmp.path().join(rel).exists(), "expected {rel}");
        }
        let pre_edit = read_json(&tmp.path().join(".kiro/hooks/strata-pre-edit.json"));
        assert_eq!(pre_edit["version"], "v1");
        assert!(pre_edit["when"].is_null(), "no legacy when block");
        let pe = &pre_edit["hooks"][0];
        assert_eq!(pe["trigger"], "PreToolUse");
        assert_eq!(pe["matcher"], "fs_write|str_replace|fs_append");
        assert_eq!(pe["action"]["type"], "agent");

        let post = read_json(&tmp.path().join(".kiro/hooks/strata-post-commit.json"));
        let poc = &post["hooks"][0];
        assert_eq!(poc["trigger"], "PostToolUse");
        assert_eq!(poc["action"]["type"], "command");
        assert_eq!(poc["action"]["command"], "strata index .");
        assert_eq!(poc["action"]["timeout"], 120);
    }

    #[test]
    fn switching_version_removes_the_other_formats_hooks() {
        let tmp = TempDir::new().unwrap();
        install(tmp.path(), &ctx_db(), KiroVersion::New).unwrap();
        assert!(tmp.path().join(".kiro/hooks/strata-pre-edit.json").exists());
        // Switch to old: the stale .json hooks are removed, .kiro.hook written.
        install(tmp.path(), &ctx_db(), KiroVersion::Old).unwrap();
        assert!(
            !tmp.path().join(".kiro/hooks/strata-pre-edit.json").exists(),
            "stale .json hook must be removed when switching to old"
        );
        assert!(tmp
            .path()
            .join(".kiro/hooks/strata-pre-edit.kiro.hook")
            .exists());
    }

    #[test]
    fn mcp_settings_register_strata_and_second_run_unchanged() {
        let tmp = TempDir::new().unwrap();
        install(tmp.path(), &ctx_db(), KiroVersion::Old).unwrap();
        let v = read_json(&tmp.path().join(".kiro/settings/mcp.json"));
        assert_eq!(v["mcpServers"]["strata"]["command"], "strata");

        let second = install(tmp.path(), &ctx_db(), KiroVersion::Old).unwrap();
        assert!(second
            .iter()
            .all(|r| r.outcome == writers::Outcome::Unchanged));
    }
}
