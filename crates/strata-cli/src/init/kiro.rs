//! Kiro artifact renderer for `strata init kiro`.
//!
//! Writes, all merge-safely:
//! * `.kiro/settings/mcp.json` — merge-add `mcpServers.strata` (same rules as Claude);
//! * `.kiro/steering/strata.md` — the managed steering block (Kiro-native steering,
//!   an improvement over GitNexus which only wrote CLAUDE/AGENTS);
//! * the two write-scoped lifecycle hooks ([`hook_specs`]), in one of two Kiro
//!   hook formats (Kiro changed its hook schema between releases), resolved by
//!   [`resolve_kiro_version`] — an explicit `--kiro-version`, else auto-detected
//!   from the repo's existing hooks, else `New`:
//!   - [`KiroVersion::Old`]: legacy `.kiro/hooks/strata-*.kiro.hook` files, a
//!     `when`/`then` shape (`{ enabled, when:{type,toolTypes}, then:{…} }`).
//!   - [`KiroVersion::New`]: `.kiro/hooks/strata-*.json` files, a
//!     `{ version:"v1", hooks:[{ trigger, matcher, action }] }` envelope.
//!
//! The two formats carry the SAME hook data; only the envelope and file extension
//! differ. Installing one version removes the other version's StrataGraph hook files,
//! so the two never coexist (a stale hook in the format Kiro no longer reads is
//! dead weight; one in the format it DOES read would double-fire). There is no
//! pre-commit hook — Kiro has no commit-specific trigger; see [`hook_specs`].

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

/// The two lifecycle hooks Kiro ships. Both ride a **mechanically scoped**
/// trigger — the write tools — so they never fire on read-only or shell
/// operations.
///
/// There is deliberately **no pre-commit hook**. Kiro can only trigger a hook by
/// tool NAME (its `matcher` is a regex over the tool name, not the command text),
/// and there is no "git commit" tool — a commit runs through the same
/// `execute_bash` tool as every build, `git log`, and `ls`. So any pre-commit
/// hook necessarily fires on *all* shell use, spamming the agent on operations
/// that are not commits (the failure a live Kiro session surfaced). The
/// commit-time `detect_changes` discipline lives instead in the steering block
/// (`MUST run detect_changes before committing`), which Kiro reads — that is its
/// correct home. (Claude Code, whose matcher can inspect the actual command text,
/// *can* run a real pre-commit hook; Kiro cannot, so this is Kiro-specific.)
fn hook_specs() -> [HookSpec; 2] {
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
            // The reindex is a runCommand hook with no agent in the loop to
            // self-disqualify, so it MUST ride a mechanically scoped trigger —
            // the WRITE tools (the Claude kit's post-edit reindex, exactly),
            // never every bash command. The MCP server hot-reloads the fresh index.
            stem: "strata-post-edit",
            name: "StrataGraph Post-Edit Reindex",
            description: "After a file edit, re-runs strata index so the on-disk graph stays fresh (the MCP server hot-reloads it).",
            trigger: Trigger::Post,
            old_tool_types: &["write"],
            new_matcher: "fs_write|str_replace|fs_append",
            action: HookAction::Command {
                command: "strata index .",
                timeout: 120,
            },
        },
    ]
}

/// Hook stems this kit previously installed and no longer ships. Removed (in
/// BOTH formats) on every install so an upgrade retires them — a stale
/// always-firing hook is exactly the failure this cleanup fixes.
///
/// - `strata-pre-commit`: dropped entirely (see [`hook_specs`] — Kiro has no
///   commit-specific trigger, so it fired on all shell use).
/// - `strata-post-commit`: superseded by `strata-post-edit` (write-tool scoped).
const RETIRED_STEMS: &[&str] = &["strata-pre-commit", "strata-post-commit"];

/// Resolve the hook-file format for an install: an explicit `--kiro-version`
/// wins; otherwise **auto-detect** from the repo's existing `.kiro/hooks` so a
/// re-run always installs the format the user's Kiro actually reads (the fix for
/// "I deleted the hooks and init didn't bring them back" — the old default wrote
/// the legacy `.kiro.hook` a newer Kiro ignores). A fresh repo with neither
/// format present defaults to [`KiroVersion::New`], the `.json` schema current
/// Kiro reads.
pub fn resolve_kiro_version(root: &Path, explicit: Option<KiroVersion>) -> KiroVersion {
    explicit
        .or_else(|| detect_kiro_version(root))
        .unwrap_or(KiroVersion::New)
}

/// Detect which hook format a repo already uses from the strata hook files under
/// `<root>/.kiro/hooks`: a `strata-*.json` ⇒ [`KiroVersion::New`], a
/// `strata-*.kiro.hook` ⇒ [`KiroVersion::Old`]. `None` when neither is present.
/// If both somehow coexist, prefer New (the current schema).
fn detect_kiro_version(root: &Path) -> Option<KiroVersion> {
    let (mut saw_json, mut saw_legacy) = (false, false);
    if let Ok(entries) = std::fs::read_dir(root.join(".kiro/hooks")) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !name.starts_with("strata-") {
                continue;
            }
            if name.ends_with(".kiro.hook") {
                saw_legacy = true;
            } else if name.ends_with(".json") {
                saw_json = true;
            }
        }
    }
    match (saw_json, saw_legacy) {
        (true, _) => Some(KiroVersion::New),
        (false, true) => Some(KiroVersion::Old),
        (false, false) => None,
    }
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
    scope: crate::init::InstallScope,
) -> Result<Vec<FileReport>, WriteError> {
    if scope == crate::init::InstallScope::User {
        return Err(WriteError::Io {
            path: "kiro".into(),
            detail: "global (--scope user) install is currently supported only for the claude kit"
                .into(),
        });
    }
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
    //    coexist, and retire renamed/removed hooks in BOTH formats (best-effort: a
    //    missing file is fine).
    for spec in &specs {
        let stale = root.join(format!(".kiro/hooks/{}.{}", spec.stem, version.other_ext()));
        if stale.exists() {
            let _ = std::fs::remove_file(&stale);
        }
    }
    for stem in RETIRED_STEMS {
        for ext in ["kiro.hook", "json"] {
            let retired = root.join(format!(".kiro/hooks/{stem}.{ext}"));
            if retired.exists() {
                let _ = std::fs::remove_file(&retired);
            }
        }
    }

    // 4. The lifecycle hooks in the selected format — wholly-owned files.
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
        let reports = install(
            tmp.path(),
            &ctx_db(),
            KiroVersion::Old,
            crate::init::InstallScope::Project,
        )
        .unwrap();
        for rel in [
            ".kiro/settings/mcp.json",
            ".kiro/steering/strata.md",
            ".kiro/hooks/strata-pre-edit.kiro.hook",
            ".kiro/hooks/strata-post-edit.kiro.hook",
        ] {
            assert!(tmp.path().join(rel).exists(), "expected {rel}");
        }
        // No pre-commit hook is installed (Kiro has no commit trigger).
        assert!(
            !tmp.path()
                .join(".kiro/hooks/strata-pre-commit.kiro.hook")
                .exists(),
            "no pre-commit hook must be installed"
        );
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

        let post = read_json(&tmp.path().join(".kiro/hooks/strata-post-edit.kiro.hook"));
        assert_eq!(post["when"]["type"], "postToolUse");
        assert_eq!(post["when"]["toolTypes"][0], "write");
        assert_eq!(post["then"]["type"], "runCommand");
        assert_eq!(post["then"]["command"], "strata index .");
        assert_eq!(post["then"]["timeout"], 120);
    }

    #[test]
    fn new_version_writes_v1_json_envelope() {
        let tmp = TempDir::new().unwrap();
        install(
            tmp.path(),
            &ctx_db(),
            KiroVersion::New,
            crate::init::InstallScope::Project,
        )
        .unwrap();
        for rel in [
            ".kiro/hooks/strata-pre-edit.json",
            ".kiro/hooks/strata-post-edit.json",
        ] {
            assert!(tmp.path().join(rel).exists(), "expected {rel}");
        }
        assert!(
            !tmp.path()
                .join(".kiro/hooks/strata-pre-commit.json")
                .exists(),
            "no pre-commit hook must be installed"
        );
        let pre_edit = read_json(&tmp.path().join(".kiro/hooks/strata-pre-edit.json"));
        assert_eq!(pre_edit["version"], "v1");
        assert!(pre_edit["when"].is_null(), "no legacy when block");
        let pe = &pre_edit["hooks"][0];
        assert_eq!(pe["trigger"], "PreToolUse");
        assert_eq!(pe["matcher"], "fs_write|str_replace|fs_append");
        assert_eq!(pe["action"]["type"], "agent");

        let post = read_json(&tmp.path().join(".kiro/hooks/strata-post-edit.json"));
        let poc = &post["hooks"][0];
        assert_eq!(poc["trigger"], "PostToolUse");
        assert_eq!(poc["action"]["type"], "command");
        assert_eq!(poc["action"]["command"], "strata index .");
        assert_eq!(poc["action"]["timeout"], 120);
    }

    #[test]
    fn retired_hooks_are_removed_on_install_in_both_formats() {
        // The over-triggering fix: the pre-commit hook (no commit trigger on Kiro)
        // and the old post-commit reindex are RETIRED. A pre-existing copy of
        // either, in EITHER format, must be deleted on install so the storm stops.
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(".kiro/hooks")).unwrap();
        for stem in ["strata-pre-commit", "strata-post-commit"] {
            for ext in ["json", "kiro.hook"] {
                std::fs::write(tmp.path().join(format!(".kiro/hooks/{stem}.{ext}")), "{}").unwrap();
            }
        }

        install(
            tmp.path(),
            &ctx_db(),
            KiroVersion::New,
            crate::init::InstallScope::Project,
        )
        .unwrap();

        for stem in ["strata-pre-commit", "strata-post-commit"] {
            for ext in ["json", "kiro.hook"] {
                assert!(
                    !tmp.path()
                        .join(format!(".kiro/hooks/{stem}.{ext}"))
                        .exists(),
                    "retired {stem}.{ext} must be removed on install"
                );
            }
        }
        // The reindex now rides the write tools, not shell/commit.
        let pe = read_json(&tmp.path().join(".kiro/hooks/strata-post-edit.json"));
        assert_eq!(
            pe["hooks"][0]["matcher"], "fs_write|str_replace|fs_append",
            "reindex fires after write tools, not after every bash command"
        );
    }

    #[test]
    fn resolve_kiro_version_detects_format_and_defaults_new() {
        // Explicit wins.
        let tmp = TempDir::new().unwrap();
        assert_eq!(
            resolve_kiro_version(tmp.path(), Some(KiroVersion::Old)),
            KiroVersion::Old
        );
        // Fresh repo (no hooks) → New (the format current Kiro reads). This is the
        // fix for "init didn't restore my hooks": the old default wrote legacy
        // `.kiro.hook` files a newer Kiro ignores.
        assert_eq!(resolve_kiro_version(tmp.path(), None), KiroVersion::New);

        // A repo with existing `.json` hooks auto-detects New.
        let newrepo = TempDir::new().unwrap();
        std::fs::create_dir_all(newrepo.path().join(".kiro/hooks")).unwrap();
        std::fs::write(
            newrepo.path().join(".kiro/hooks/strata-pre-edit.json"),
            "{}",
        )
        .unwrap();
        assert_eq!(resolve_kiro_version(newrepo.path(), None), KiroVersion::New);

        // A repo with existing `.kiro.hook` hooks auto-detects Old — so a legacy
        // Kiro user's re-run keeps the format their editor reads.
        let oldrepo = TempDir::new().unwrap();
        std::fs::create_dir_all(oldrepo.path().join(".kiro/hooks")).unwrap();
        std::fs::write(
            oldrepo.path().join(".kiro/hooks/strata-pre-edit.kiro.hook"),
            "{}",
        )
        .unwrap();
        assert_eq!(resolve_kiro_version(oldrepo.path(), None), KiroVersion::Old);
    }

    #[test]
    fn switching_version_removes_the_other_formats_hooks() {
        let tmp = TempDir::new().unwrap();
        install(
            tmp.path(),
            &ctx_db(),
            KiroVersion::New,
            crate::init::InstallScope::Project,
        )
        .unwrap();
        assert!(tmp.path().join(".kiro/hooks/strata-pre-edit.json").exists());
        // Switch to old: the stale .json hooks are removed, .kiro.hook written.
        install(
            tmp.path(),
            &ctx_db(),
            KiroVersion::Old,
            crate::init::InstallScope::Project,
        )
        .unwrap();
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
        install(
            tmp.path(),
            &ctx_db(),
            KiroVersion::Old,
            crate::init::InstallScope::Project,
        )
        .unwrap();
        let v = read_json(&tmp.path().join(".kiro/settings/mcp.json"));
        assert_eq!(v["mcpServers"]["strata"]["command"], "strata");

        let second = install(
            tmp.path(),
            &ctx_db(),
            KiroVersion::Old,
            crate::init::InstallScope::Project,
        )
        .unwrap();
        assert!(second
            .iter()
            .all(|r| r.outcome == writers::Outcome::Unchanged));
    }

    #[test]
    fn user_scope_is_unsupported_for_kiro() {
        let tmp = TempDir::new().unwrap();
        let result = install(
            tmp.path(),
            &ctx_db(),
            KiroVersion::Old,
            crate::init::InstallScope::User,
        );
        assert!(
            result.is_err(),
            "kiro install with User scope must return Err"
        );
        // No kiro files should have been written.
        assert!(
            !tmp.path().join(".kiro").exists(),
            "no .kiro directory should be created when scope is User"
        );
    }
}
