//! `strata init <agent>` — the one-command agent-integration kit.
//!
//! Installs a first-class, strictly-governed agent setup (MCP registration,
//! managed steering blocks, task-routed skills, scoped silent-when-clean hooks),
//! **idempotently** and **merge-safely**, coexisting with an existing GitNexus
//! install. The merge-safe primitives live in [`writers`]; the per-agent
//! artifact contents in [`content`]; the agent renderers in [`claude`]/[`kiro`].
//!
//! The flow is:
//! 1. [`detect_context`] inspects the project root for a workspace manifest and a
//!    loadable graph → the MCP launch args and the steering [`Identity`].
//! 2. The agent renderer ([`claude::install`] / [`kiro::install`]) writes every
//!    artifact through the writers, collecting a [`FileReport`] each.
//! 3. [`run`] formats the per-file summary + next steps.

pub mod claude;
pub mod content;
pub mod kiro;
pub mod writers;

pub use kiro::KiroVersion;

use std::path::{Path, PathBuf};

use content::Identity;
use writers::{Outcome, WriteError};

/// Which agent's kit to install.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agent {
    /// Claude Code: `.mcp.json`, `CLAUDE.md` + `AGENTS.md`, skills, `.claude/settings.json` hooks.
    Claude,
    /// Kiro: `.kiro/settings/mcp.json`, `.kiro/steering/strata.md`, and
    /// `.kiro/hooks/*` in the legacy `.kiro.hook` or new `.json` format (per
    /// [`KiroVersion`], selected by `--kiro-version`).
    Kiro,
}

impl Agent {
    /// Parse the `<agent>` argument, returning `None` for an unknown agent.
    pub fn parse(s: &str) -> Option<Agent> {
        match s {
            "claude" => Some(Agent::Claude),
            "kiro" => Some(Agent::Kiro),
            _ => None,
        }
    }

    /// The supported agent names, for the bare-`init` listing and error messages.
    pub const SUPPORTED: &'static [&'static str] = &["claude", "kiro"];
}

/// What `init` did to one file. Carried into the summary the user sees.
#[derive(Debug, Clone)]
pub struct FileReport {
    /// Path relative to the project root (for a tidy summary line).
    pub path: PathBuf,
    /// Whether the file was created, updated, or already current.
    pub outcome: Outcome,
}

impl FileReport {
    /// Record an outcome for `path`.
    pub fn new(path: impl Into<PathBuf>, outcome: Outcome) -> Self {
        FileReport {
            path: path.into(),
            outcome,
        }
    }
}

/// The detected repository context that parameterises the artifacts.
#[derive(Debug, Clone)]
pub struct RepoContext {
    /// The MCP server launch args: `["mcp", "--workspace", "strata.workspace.toml"]`
    /// when a manifest is present; bare `["mcp"]` when the directory is an estate
    /// member repo (marker present, no manifest here); else
    /// `["mcp", "--db", ".strata/graph.duckdb"]` for a standalone repo.
    pub mcp_args: Vec<String>,
    /// The identity facts for the steering block (real counts or "not indexed").
    pub identity: Identity,
}

/// The manifest filename that switches the MCP launch into estate mode.
pub const WORKSPACE_MANIFEST: &str = "strata.workspace.toml";

/// Inspect `root` for a workspace manifest and a loadable graph, producing the
/// [`RepoContext`] that drives artifact generation.
///
/// * `strata.workspace.toml` at root → estate MCP args; identity loaded from the
///   linked estate if every repo is indexed, else the "not indexed" variant.
/// * `.strata/estate.toml` marker present (estate member, no manifest in the dir) →
///   bare `["mcp"]` args (the server resolves the estate at runtime); identity loaded
///   from the marker's manifest via `graph_loader`.
/// * else → single-DB MCP args (`--db .strata/graph.duckdb`); identity loaded
///   from that DB if it exists, else "not indexed".
///
/// `graph_loader` is injected so this is unit-testable without a real DuckDB
/// store; production callers pass [`load_identity`].
pub fn detect_context(
    root: &Path,
    graph_loader: impl Fn(&Path) -> Option<Identity>,
) -> RepoContext {
    let manifest = root.join(WORKSPACE_MANIFEST);
    if manifest.exists() {
        // Workspace root: pin the manifest so `strata mcp` loads the estate.
        let identity = graph_loader(&manifest).unwrap_or(Identity::NotIndexed);
        RepoContext {
            mcp_args: vec![
                "mcp".into(),
                "--workspace".into(),
                WORKSPACE_MANIFEST.into(),
            ],
            identity,
        }
    } else if let Some(marker) = strata_index::estate_marker::read_marker(&root.join(".strata")) {
        // Estate member repo: Task 8 resolves the estate at runtime from the
        // marker, so the bare `strata mcp` (no flags) is sufficient and
        // forward-compatible with any estate layout change.
        let identity = graph_loader(&marker.manifest).unwrap_or(Identity::NotIndexed);
        RepoContext {
            mcp_args: vec!["mcp".into()],
            identity,
        }
    } else {
        // Single (standalone) repo: pin the DB path explicitly.
        let db = root.join(crate::DEFAULT_DB);
        let identity = if db.exists() {
            graph_loader(&db).unwrap_or(Identity::NotIndexed)
        } else {
            Identity::NotIndexed
        };
        RepoContext {
            mcp_args: vec!["mcp".into(), "--db".into(), crate::DEFAULT_DB.into()],
            identity,
        }
    }
}

/// Load a graph from `path` (a `.duckdb` DB or a `strata.workspace.toml`) and
/// distill it into an [`Identity`] for the steering line. Returns `None` if the
/// graph cannot be loaded (caller falls back to the "not indexed" variant).
///
/// This is the production `graph_loader` for [`detect_context`]; it bridges to
/// the existing single-DB / estate load paths in [`crate`].
pub fn load_identity(path: &Path) -> Option<Identity> {
    let is_manifest = path
        .file_name()
        .map(|n| n == WORKSPACE_MANIFEST)
        .unwrap_or(false);

    let (graph, name, is_estate) = if is_manifest {
        let (graph, _results) = crate::load_workspace_graph(path).ok()?;
        let name = workspace_name(path).unwrap_or_else(|| "estate".to_string());
        (graph, name, true)
    } else {
        let store = strata_store::DuckGraphStore::open(path).ok()?;
        use strata_store::GraphStore;
        let graph = store.load_graph().ok()?;
        let name = repo_name_from_db(path);
        (graph, name, false)
    };

    Some(identity_from_graph(&graph, name, is_estate))
}

/// Derive an [`Identity::Indexed`] from a loaded graph: counts + which planes
/// are present (contract = GraphQL fields / API operations; infra = Lambdas /
/// cloud resources / IAM / AppSync).
fn identity_from_graph(graph: &strata_core::Graph, name: String, is_estate: bool) -> Identity {
    use strata_core::NodeKind::*;
    let mut has_contract = false;
    let mut has_infra = false;
    for n in graph.nodes() {
        match n.kind {
            GraphqlField | ApiOperation => has_contract = true,
            LambdaFn | CloudResource | IamRole | AppSyncApi | AppSyncResolver
            | AppSyncDataSource => has_infra = true,
            _ => {}
        }
        if has_contract && has_infra {
            break;
        }
    }
    Identity::Indexed {
        name,
        nodes: graph.node_count(),
        edges: graph.edge_count(),
        has_contract,
        has_infra,
        is_estate,
    }
}

/// Best-effort repo name from a `.strata/graph.duckdb` path: the repo directory
/// is the grandparent of the DB (`<repo>/.strata/graph.duckdb`).
fn repo_name_from_db(db: &Path) -> String {
    db.parent()
        .and_then(Path::parent)
        .and_then(Path::file_name)
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "this repo".to_string())
}

/// Parse the `[workspace] name` out of a manifest for the estate identity line.
fn workspace_name(manifest: &Path) -> Option<String> {
    let parsed = strata_index::WorkspaceManifest::parse_file(manifest).ok()?;
    Some(parsed.workspace.name)
}

/// Where the kit is installed: a single repo (default) or the user's `~/.claude`
/// (applies to every repo). Selected by `--global` / `--scope` on `init`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallScope {
    Project,
    User,
}

impl InstallScope {
    /// Resolve the scope from the two CLI inputs. `--global` is the headline
    /// boolean; `--scope project|user` is the explicit form. They must agree.
    pub fn from_flags(global: bool, scope: Option<&str>) -> Result<InstallScope, String> {
        let from_scope = match scope {
            None => None,
            Some("user") => Some(InstallScope::User),
            Some("project") => Some(InstallScope::Project),
            Some(other) => {
                return Err(format!(
                    "unknown --scope `{other}`; supported: project, user"
                ))
            }
        };
        match (global, from_scope) {
            (true, Some(InstallScope::Project)) => {
                Err("--global conflicts with --scope project; pick one".to_string())
            }
            (true, _) => Ok(InstallScope::User),
            (false, Some(s)) => Ok(s),
            (false, None) => Ok(InstallScope::Project),
        }
    }
}

/// `strata init <agent> [--yes]` — the command entrypoint.
///
/// Detects the repo context, runs the agent's installer, and returns the
/// human-readable summary (per-file outcomes + next steps). `yes` is forwarded
/// to the no-index prompt: with `--yes` (or any non-interactive run) we never
/// block on stdin — we write the honest "not yet indexed" artifacts and tell the
/// user to run `strata index`.
pub fn run(
    agent: Agent,
    root: &Path,
    _yes: bool,
    kiro_version: Option<KiroVersion>,
    scope: InstallScope,
) -> Result<String, WriteError> {
    let ctx = detect_context(root, load_identity);
    let reports = match agent {
        Agent::Claude => claude::install(root, &ctx, scope)?,
        // `kiro_version` selects the hook-file format; `None` auto-detects it from
        // the repo's existing hooks (default New). Ignored for Claude.
        Agent::Kiro => {
            let resolved = kiro::resolve_kiro_version(root, kiro_version);
            kiro::install(root, &ctx, resolved, scope)?
        }
    };
    Ok(render_summary(agent, root, &ctx, &reports))
}

/// Format the per-file summary the user sees after `init`.
fn render_summary(agent: Agent, root: &Path, ctx: &RepoContext, reports: &[FileReport]) -> String {
    let agent_label = match agent {
        Agent::Claude => "Claude Code",
        Agent::Kiro => "Kiro",
    };
    let mut out = format!("StrataGraph agent kit for {agent_label}:\n");
    for r in reports {
        out.push_str(&format!(
            "  {:<9} {}\n",
            r.outcome.label(),
            r.path.display()
        ));
    }

    // Next steps: if the graph wasn't indexed, the first action is to index.
    out.push('\n');
    match ctx.identity {
        Identity::NotIndexed => {
            out.push_str("Next: run `strata index ");
            out.push_str(&root.display().to_string());
            out.push_str("` to build the graph, then restart your agent session.\n");
        }
        Identity::Indexed { .. } => {
            out.push_str(
                "Next: restart your agent session so the MCP server picks up the kit, then ask \"what breaks if I change X?\".\n",
            );
        }
        Identity::Global => {
            out.push_str(
                "Next: the MCP server will resolve your current repo's graph at runtime. If you're not in a StrataGraph-indexed repo yet, run `strata index .` first.\n",
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_scope_parses_flags() {
        assert_eq!(
            InstallScope::from_flags(true, None).unwrap(),
            InstallScope::User
        );
        assert_eq!(
            InstallScope::from_flags(false, Some("user")).unwrap(),
            InstallScope::User
        );
        assert_eq!(
            InstallScope::from_flags(false, Some("project")).unwrap(),
            InstallScope::Project
        );
        assert_eq!(
            InstallScope::from_flags(false, None).unwrap(),
            InstallScope::Project
        );
        // Conflict: --global with --scope project is an error.
        assert!(InstallScope::from_flags(true, Some("project")).is_err());
        // Unknown scope value is an error.
        assert!(InstallScope::from_flags(false, Some("bogus")).is_err());
    }

    #[test]
    fn agent_parse_known_and_unknown() {
        assert_eq!(Agent::parse("claude"), Some(Agent::Claude));
        assert_eq!(Agent::parse("kiro"), Some(Agent::Kiro));
        assert_eq!(Agent::parse("cursor"), None);
    }

    #[test]
    fn detect_context_db_mode_when_no_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        // No manifest, no DB → db-mode args, not-indexed identity.
        let ctx = detect_context(tmp.path(), |_| None);
        assert_eq!(ctx.mcp_args, vec!["mcp", "--db", crate::DEFAULT_DB]);
        assert!(matches!(ctx.identity, Identity::NotIndexed));
    }

    #[test]
    fn detect_context_workspace_mode_when_manifest_present() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join(WORKSPACE_MANIFEST),
            "[workspace]\nname = \"e\"\n",
        )
        .unwrap();
        // Loader returns an indexed identity to prove it is consulted.
        let ctx = detect_context(tmp.path(), |_| {
            Some(Identity::Indexed {
                name: "e".into(),
                nodes: 3,
                edges: 2,
                has_contract: false,
                has_infra: false,
                is_estate: true,
            })
        });
        assert_eq!(ctx.mcp_args, vec!["mcp", "--workspace", WORKSPACE_MANIFEST]);
        assert!(matches!(
            ctx.identity,
            Identity::Indexed {
                is_estate: true,
                ..
            }
        ));
    }

    #[test]
    fn repo_name_from_db_uses_grandparent_dir() {
        let p = Path::new("/x/my-repo/.strata/graph.duckdb");
        assert_eq!(repo_name_from_db(p), "my-repo");
    }

    /// An estate MEMBER repo: `.strata/estate.toml` present (no workspace manifest
    /// in the dir) → `mcp_args` must be the bare auto-resolve form `["mcp"]`, NOT
    /// the single-`--db` form, and the identity must be loaded via the injected
    /// loader called with the marker's manifest path.
    #[test]
    fn detect_context_estate_member_when_marker_present() {
        use strata_index::estate_marker::{write_marker, EstateMarker};

        let tmp = tempfile::tempdir().unwrap();
        let strata_dir = tmp.path().join(".strata");
        std::fs::create_dir_all(&strata_dir).unwrap();

        // Simulate a manifest path (does not need to exist on disk for this test).
        let manifest_path = tmp.path().parent().unwrap().join("strata.workspace.toml");

        write_marker(
            &strata_dir,
            &EstateMarker {
                manifest: manifest_path.clone(),
                estate: "my-estate".into(),
                repo: "repo-a".into(),
            },
        )
        .unwrap();

        // The injected graph_loader captures the path it was called with.
        use std::sync::{Arc, Mutex};
        let called_with: Arc<Mutex<Option<std::path::PathBuf>>> = Arc::new(Mutex::new(None));
        let called_with_clone = called_with.clone();

        let ctx = detect_context(tmp.path(), move |p| {
            *called_with_clone.lock().unwrap() = Some(p.to_path_buf());
            Some(Identity::Indexed {
                name: "my-estate".into(),
                nodes: 10,
                edges: 5,
                has_contract: true,
                has_infra: false,
                is_estate: true,
            })
        });

        // Must be the bare auto-resolve form (Task 8 resolves estate at runtime).
        assert_eq!(
            ctx.mcp_args,
            vec!["mcp"],
            "estate member must use bare auto-resolve mcp args, got: {:?}",
            ctx.mcp_args
        );
        // Must NOT be the single-db form.
        assert!(
            !ctx.mcp_args.contains(&"--db".to_string()),
            "estate member must NOT hardcode --db: {:?}",
            ctx.mcp_args
        );
        // Identity must be loaded from the estate manifest path (via loader).
        assert_eq!(
            called_with.lock().unwrap().as_deref(),
            Some(manifest_path.as_path()),
            "graph_loader must be called with the estate marker's manifest path"
        );
        // Identity must reflect the estate (from the injected loader).
        assert!(
            matches!(
                ctx.identity,
                Identity::Indexed {
                    is_estate: true,
                    ..
                }
            ),
            "identity must be the estate variant: {:?}",
            ctx.identity
        );
    }
}
