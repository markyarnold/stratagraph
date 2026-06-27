//! The `strata` binary: a thin clap front-end over the testable handlers in
//! [`strata_cli`]. `main` parses arguments, calls the matching handler, prints
//! the `Ok` string (or the `mcp` long-running server), and maps errors to a
//! friendly message on stderr plus the handler's chosen exit code.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use dirs;

use strata_cli::init::{self, Agent, KiroVersion};
use strata_cli::{
    cmd_blast, cmd_context, cmd_context_workspace, cmd_detect_changes, cmd_explain,
    cmd_explain_workspace, cmd_impact, cmd_impact_workspace, cmd_index, cmd_index_workspace,
    cmd_mcp, cmd_mcp_workspace, cmd_query, cmd_query_workspace, cmd_rename, db_path, BlastFormat,
    CliError, McpLaunch,
};
use strata_index::ResolveMode;

/// StrataGraph: an incremental, cross-file code graph for TS/JS — query its
/// callers/callees, blast radius, and lexical matches, or serve it over MCP.
#[derive(Parser)]
#[command(name = "strata", version = full_version(), about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// `0.1.0 (abc123def456)` — package version + the compiled engine id, so a
/// stale binary on PATH is identifiable at a glance.
fn full_version() -> &'static str {
    // Leaked once at startup: clap's `version` wants a 'static str (without
    // its "string" feature), and one short allocation for the process lifetime
    // beats a feature flag.
    Box::leak(
        format!("{} ({})", env!("CARGO_PKG_VERSION"), strata_core::ENGINE_ID).into_boxed_str(),
    )
}

#[derive(Subcommand)]
enum Command {
    /// Build or refresh the code graph for a repository (or a workspace estate).
    Index {
        /// Path to the repository root to index.
        /// Not required when --workspace is provided.
        path: Option<PathBuf>,
        /// Graph database path (default: .strata/graph.duckdb).
        /// Mutually exclusive with --workspace.
        #[arg(long, value_name = "PATH", conflicts_with = "workspace")]
        db: Option<PathBuf>,
        /// Path to a workspace manifest (strata.workspace.toml).
        /// Indexes all repos in the estate. Mutually exclusive with --db.
        #[arg(long, value_name = "MANIFEST", conflicts_with = "db")]
        workspace: Option<PathBuf>,
        /// Resolution mode: auto (default), on, or off.
        #[arg(long, value_name = "MODE", default_value = "auto")]
        resolve: String,
        /// Index committed third-party dependency bundles instead of pruning them.
        /// By default a vendored `pip install -t .` bundle (detected via its
        /// `*.dist-info`) is excluded so it does not inflate the graph.
        #[arg(long)]
        include_vendored: bool,
    },
    /// Show the reverse blast radius (dependents) of a symbol.
    Impact {
        /// Symbol to analyse (fully-qualified name preferred, else name).
        symbol: String,
        /// Pin one candidate when <symbol> resolves to several nodes (an
        /// ambiguous symbol lists its candidates' uids — re-run with one here).
        #[arg(long, value_name = "UID")]
        uid: Option<String>,
        /// Graph database path (default: .strata/graph.duckdb).
        /// Mutually exclusive with --workspace.
        #[arg(long, value_name = "PATH", conflicts_with = "workspace")]
        db: Option<PathBuf>,
        /// Path to a workspace manifest (strata.workspace.toml).
        /// Runs impact over the estate graph. Mutually exclusive with --db.
        #[arg(long, value_name = "MANIFEST", conflicts_with = "db")]
        workspace: Option<PathBuf>,
        /// Maximum reverse-traversal depth.
        #[arg(long, default_value_t = 5)]
        depth: usize,
        /// Drop paths below this accumulated confidence.
        #[arg(long = "min-confidence", default_value_t = 0.0)]
        min_confidence: f32,
        /// Code-only blast radius: do NOT follow the contract plane
        /// (producer → operation → consumer). Contracts are INCLUDED by default,
        /// so cross-plane/cross-repo consumers of a producer are surfaced.
        #[arg(long = "no-contracts", default_value_t = false)]
        no_contracts: bool,
        /// Do NOT follow the infra plane (Assumes/Routes/Runs). Infra is INCLUDED
        /// by default, so an IamRole reaches the Lambdas that assume it (and their
        /// reach), and a handler module reaches its Lambda (§6.3).
        #[arg(long = "no-infra", default_value_t = false)]
        no_infra: bool,
    },
    /// Explain WHY one symbol is in another's blast radius: the evidence chain
    /// (each edge's kind, provenance, confidence + the running confidence that
    /// produces impact's number). The visible form of never-confident-wrong.
    Explain {
        /// The changed symbol (the impact target; fqn preferred, else name).
        target: String,
        /// The affected symbol whose presence in the blast radius to explain.
        affected: String,
        /// Pin the TARGET when it resolves to several nodes (an ambiguous end
        /// lists its candidates' uids — re-run with one here).
        #[arg(long, value_name = "UID")]
        uid: Option<String>,
        /// Pin the AFFECTED end when IT resolves to several nodes (mirrors the MCP
        /// `explain` tool's `affected_uid` — re-run with one of its candidates' uids).
        #[arg(long = "affected-uid", value_name = "UID")]
        affected_uid: Option<String>,
        /// Graph database path (default: .strata/graph.duckdb).
        /// Mutually exclusive with --workspace.
        #[arg(long, value_name = "PATH", conflicts_with = "workspace")]
        db: Option<PathBuf>,
        /// Path to a workspace manifest (strata.workspace.toml).
        /// Explains over the estate graph. Mutually exclusive with --db.
        #[arg(long, value_name = "MANIFEST", conflicts_with = "db")]
        workspace: Option<PathBuf>,
        /// Maximum reverse-traversal depth (must match the impact run explained).
        #[arg(long, default_value_t = 5)]
        depth: usize,
        /// Drop paths below this accumulated confidence.
        #[arg(long = "min-confidence", default_value_t = 0.0)]
        min_confidence: f32,
        /// Do NOT follow the contract plane (producer → operation → consumer).
        #[arg(long = "no-contracts", default_value_t = false)]
        no_contracts: bool,
        /// Do NOT follow the infra plane (Assumes/Routes/Runs).
        #[arg(long = "no-infra", default_value_t = false)]
        no_infra: bool,
    },
    /// Show the 360° context of a symbol (callers, callees, imports, members).
    Context {
        /// Symbol to inspect (fully-qualified name preferred, else name).
        symbol: String,
        /// Graph database path (default: .strata/graph.duckdb).
        /// Mutually exclusive with --workspace.
        #[arg(long, value_name = "PATH", conflicts_with = "workspace")]
        db: Option<PathBuf>,
        /// Path to a workspace manifest (strata.workspace.toml).
        /// Runs context over the estate graph. Mutually exclusive with --db.
        #[arg(long, value_name = "MANIFEST", conflicts_with = "db")]
        workspace: Option<PathBuf>,
    },
    /// Lexical search over node name, fully-qualified name, and path.
    Query {
        /// Substring to search for.
        text: String,
        /// Graph database path (default: .strata/graph.duckdb).
        /// Mutually exclusive with --workspace.
        #[arg(long, value_name = "PATH", conflicts_with = "workspace")]
        db: Option<PathBuf>,
        /// Path to a workspace manifest (strata.workspace.toml).
        /// Searches the estate graph. Mutually exclusive with --db.
        #[arg(long, value_name = "MANIFEST", conflicts_with = "db")]
        workspace: Option<PathBuf>,
    },
    /// Serve the code graph to an MCP client over stdio.
    Mcp {
        /// Graph database path (default: .strata/graph.duckdb).
        /// Mutually exclusive with --workspace.
        #[arg(long, value_name = "PATH", conflicts_with = "workspace")]
        db: Option<PathBuf>,
        /// Path to a workspace manifest (strata.workspace.toml).
        /// Serves the linked estate graph over MCP. Mutually exclusive with --db.
        #[arg(long, value_name = "MANIFEST", conflicts_with = "db")]
        workspace: Option<PathBuf>,
        /// Repository root for the `detect_changes` tool (default: the
        /// grandparent of --db when it ends `.strata/graph.duckdb`).
        /// Mutually exclusive with --workspace.
        #[arg(long, value_name = "PATH", conflicts_with = "workspace")]
        repo: Option<PathBuf>,
    },
    /// Report the changed symbols, blast radius, and risk vs HEAD (the mechanical
    /// pre-commit check). Reports — it never gates; always exits 0.
    DetectChanges {
        /// Graph database path (default: .strata/graph.duckdb). Forces
        /// single-repo mode (no estate). Conflicts with --workspace.
        #[arg(long, value_name = "PATH", conflicts_with = "workspace")]
        db: Option<PathBuf>,
        /// Repository root (default: the grandparent of --db when it ends
        /// `.strata/graph.duckdb`, else the current directory).
        #[arg(long, value_name = "PATH")]
        repo: Option<PathBuf>,
        /// Estate workspace manifest path. Forces estate mode: git-diffs the
        /// member repo and aggregates blast over the full estate graph.
        /// Conflicts with --db.
        #[arg(long, value_name = "MANIFEST", conflicts_with = "db")]
        workspace: Option<PathBuf>,
        /// Diff the staged index (`git diff --cached HEAD`) instead of the
        /// working tree.
        #[arg(long, default_value_t = false)]
        staged: bool,
    },
    /// Report the pre-edit blast radius of a FILE: the symbols it defines, the
    /// reverse blast radius of changing them, and the risk. Reports — it never
    /// gates; always exits 0. (Powers the pre-edit hook.)
    Blast {
        /// The file to assess (repo-relative, or absolute under the repo root).
        file: String,
        /// Graph database path. Forces single-repo blast (no estate).
        /// Conflicts with --workspace.
        #[arg(long, value_name = "PATH", conflicts_with = "workspace")]
        db: Option<PathBuf>,
        /// Repository root (default: the grandparent of --db when it ends
        /// `.strata/graph.duckdb`). Used to make an absolute <file> repo-relative.
        #[arg(long, value_name = "PATH")]
        repo: Option<PathBuf>,
        /// Estate workspace manifest path. Forces estate blast.
        /// Conflicts with --db.
        #[arg(long, value_name = "MANIFEST", conflicts_with = "db")]
        workspace: Option<PathBuf>,
        /// Output format: `text` (human summary, default) or `agent` (the terse
        /// token-lean block the pre-edit hook injects).
        #[arg(long, value_name = "FORMAT", default_value = "text")]
        format: String,
    },
    /// Graph-aware multi-file rename of a code symbol. Dry-run by default;
    /// pass --apply to write. Edits land only in graph-implicated files.
    Rename {
        /// The current symbol name (fully-qualified name preferred, else name).
        old: String,
        /// The new identifier.
        new: String,
        /// Graph database path (default: .strata/graph.duckdb).
        #[arg(long, value_name = "PATH")]
        db: Option<PathBuf>,
        /// Repository root (default: the grandparent of --db when it ends
        /// `.strata/graph.duckdb`, else the current directory).
        #[arg(long, value_name = "PATH")]
        repo: Option<PathBuf>,
        /// Write the edits to disk (default: dry run — lists edits only).
        #[arg(long, default_value_t = false)]
        apply: bool,
        /// Proceed even if a repo-wide symbol is already named <new>.
        #[arg(long, default_value_t = false)]
        force: bool,
        /// Pin one candidate when <old> resolves to several code nodes.
        #[arg(long, value_name = "UID")]
        uid: Option<String>,
    },
    /// Install a strictly-governed agent-integration kit (MCP, steering, skills,
    /// scoped hooks) — idempotent and merge-safe. Bare `init` lists agents.
    Init {
        /// Which agent to set up: `claude` or `kiro`. Omit to list supported agents.
        agent: Option<String>,
        /// Project root to install into (default: current directory).
        #[arg(long, value_name = "DIR", default_value = ".")]
        path: PathBuf,
        /// Run any needed `strata index` non-interactively (no prompts).
        #[arg(long, default_value_t = false)]
        yes: bool,
        /// Kiro only: hook format — `old` (legacy `.kiro.hook`, the default) or
        /// `new` (`.json`, the schema Kiro's newer version introduced).
        #[arg(long, value_name = "VERSION", default_value = "old")]
        kiro_version: String,
        /// Install into your user-level ~/.claude so the kit applies to every repo.
        #[arg(long, default_value_t = false)]
        global: bool,
        /// Install scope: `project` (default, this repo) or `user` (~/.claude, all repos).
        #[arg(long, value_name = "SCOPE")]
        scope: Option<String>,
    },
}

/// Handle `strata init [<agent>] [--path DIR] [--yes] [--global] [--scope …]`.
///
/// * no agent → list the supported agents (an `Ok` listing, exit 0);
/// * unknown agent → an actionable error naming the supported agents;
/// * known agent → optionally index first (when `--yes` and scope is Project and
///   no graph exists yet, so the steering identity line carries real counts),
///   then write the kit.
fn run_init(
    agent: Option<&str>,
    path: &Path,
    yes: bool,
    kiro_version: &str,
    global: bool,
    scope: Option<&str>,
) -> Result<Option<String>, CliError> {
    let agent = match agent {
        None => {
            return Ok(Some(format!(
                "Usage: strata init <agent>\nSupported agents: {}",
                Agent::SUPPORTED.join(", ")
            )));
        }
        Some(name) => Agent::parse(name).ok_or_else(|| {
            CliError::Other(format!(
                "unknown agent `{name}`; supported agents: {}",
                Agent::SUPPORTED.join(", ")
            ))
        })?,
    };

    let kiro_version = KiroVersion::parse(kiro_version).ok_or_else(|| {
        CliError::Other(format!(
            "unknown --kiro-version `{kiro_version}`; supported: {}",
            KiroVersion::SUPPORTED.join(", ")
        ))
    })?;

    let scope = init::InstallScope::from_flags(global, scope).map_err(CliError::Other)?;
    let root: PathBuf = match scope {
        init::InstallScope::Project => path.to_path_buf(),
        init::InstallScope::User => dirs::home_dir().ok_or_else(|| {
            CliError::Other(
                "could not resolve your home directory for a --global install".into(),
            )
        })?,
    };

    // With --yes and project scope and no index yet, build it first so the
    // identity line is real. Gated on Project scope: a global install has no
    // repo to index.
    if yes && scope == init::InstallScope::Project {
        let manifest = root.join(init::WORKSPACE_MANIFEST);
        let db = root.join(strata_cli::DEFAULT_DB);
        if !manifest.exists() && !db.exists() {
            // Single-repo index into the default DB location under `root`.
            // Auto-index on `init` never opts into vendored bundles.
            cmd_index(&root, &db, false)?;
        }
    }

    init::run(agent, &root, yes, kiro_version, scope)
        .map(Some)
        .map_err(|e| CliError::Other(e.to_string()))
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    // F6 (intentional divergence): the CLI dispatches to the testable `strata_cli`
    // handlers, which call `strata_core`/`strata_index` directly and hand-render
    // human-text columns — it does NOT route through `strata_mcp::call_tool` the way
    // the desktop GUI does. Both paths drive the SAME engine, so answers are
    // consistent; the CLI is feature-complete (members_with_dependents, candidates,
    // affected-uid). A CLI→call_tool refactor would have no user benefit and risks
    // regressing the text output, so it is deliberately not done.
    let result: Result<Option<String>, CliError> = match cli.command {
        Command::Index {
            path,
            db,
            workspace,
            resolve,
            include_vendored,
        } => {
            if let Some(manifest) = workspace {
                let mode = ResolveMode::parse(&resolve).unwrap_or(ResolveMode::Auto);
                cmd_index_workspace(&manifest, mode, include_vendored).map(Some)
            } else {
                let repo = match path {
                    Some(p) => p,
                    None => {
                        eprintln!("error: `strata index` requires either a <path> or --workspace");
                        return ExitCode::from(1);
                    }
                };
                cmd_index(&repo, &db_path(db.as_deref()), include_vendored).map(Some)
            }
        }
        Command::Impact {
            symbol,
            uid,
            db,
            workspace,
            depth,
            min_confidence,
            no_contracts,
            no_infra,
        } => {
            let include_contracts = !no_contracts;
            let include_infra = !no_infra;
            if let Some(manifest) = workspace {
                cmd_impact_workspace(
                    &manifest,
                    &symbol,
                    depth,
                    min_confidence,
                    include_contracts,
                    include_infra,
                    uid.as_deref(),
                )
                .map(Some)
            } else {
                cmd_impact(
                    &db_path(db.as_deref()),
                    &symbol,
                    depth,
                    min_confidence,
                    include_contracts,
                    include_infra,
                    uid.as_deref(),
                )
                .map(Some)
            }
        }
        Command::Explain {
            target,
            affected,
            uid,
            affected_uid,
            db,
            workspace,
            depth,
            min_confidence,
            no_contracts,
            no_infra,
        } => {
            let include_contracts = !no_contracts;
            let include_infra = !no_infra;
            if let Some(manifest) = workspace {
                cmd_explain_workspace(
                    &manifest,
                    &target,
                    &affected,
                    depth,
                    min_confidence,
                    include_contracts,
                    include_infra,
                    uid.as_deref(),
                    affected_uid.as_deref(),
                )
                .map(Some)
            } else {
                cmd_explain(
                    &db_path(db.as_deref()),
                    &target,
                    &affected,
                    depth,
                    min_confidence,
                    include_contracts,
                    include_infra,
                    uid.as_deref(),
                    affected_uid.as_deref(),
                )
                .map(Some)
            }
        }
        Command::Context {
            symbol,
            db,
            workspace,
        } => {
            if let Some(manifest) = workspace {
                cmd_context_workspace(&manifest, &symbol).map(Some)
            } else {
                cmd_context(&db_path(db.as_deref()), &symbol).map(Some)
            }
        }
        Command::Query {
            text,
            db,
            workspace,
        } => {
            if let Some(manifest) = workspace {
                cmd_query_workspace(&manifest, &text).map(Some)
            } else {
                cmd_query(&db_path(db.as_deref()), &text).map(Some)
            }
        }
        Command::Mcp {
            db,
            workspace,
            repo,
        } => {
            if let Some(manifest) = workspace {
                // Explicit --workspace: resolve precedence is --repo ->
                // $CLAUDE_PROJECT_DIR -> cwd -> None (clean "needs a repo root"
                // error from detect_changes when nothing resolves).
                let repo_root = strata_cli::resolve_mcp_cwd(
                    repo.as_deref(),
                    std::env::var("CLAUDE_PROJECT_DIR").ok().as_deref(),
                    std::env::current_dir().ok(),
                );
                cmd_mcp_workspace(&manifest, repo_root.as_deref()).map(|()| None)
            } else if db.is_some() {
                // Explicit --db: existing single-repo path (back-compat).
                cmd_mcp(&db_path(db.as_deref()), repo.as_deref()).map(|()| None)
            } else {
                // Auto-resolve: neither --db nor --workspace given. Check the
                // cwd (or --repo if given) for an estate marker; serve estate
                // or single accordingly, always carrying the member repo root
                // in ToolCtx. --repo overrides cwd so an agent launched from
                // outside the member directory can still resolve the estate.
                let cwd = strata_cli::resolve_mcp_cwd(
                    repo.as_deref(),
                    std::env::var("CLAUDE_PROJECT_DIR").ok().as_deref(),
                    std::env::current_dir().ok(),
                )
                .unwrap_or_else(|| std::path::PathBuf::from("."));
                match strata_cli::resolve_mcp_launch(&cwd) {
                    McpLaunch::Estate {
                        manifest,
                        repo_root,
                    } => cmd_mcp_workspace(&manifest, Some(&repo_root)).map(|()| None),
                    McpLaunch::Single { db, repo_root } => {
                        cmd_mcp(&db, repo_root.as_deref()).map(|()| None)
                    }
                }
            }
        }
        Command::DetectChanges {
            db,
            repo,
            workspace,
            staged,
        } => cmd_detect_changes(db.as_deref(), repo.as_deref(), workspace.as_deref(), staged)
            .map(Some),
        Command::Blast {
            file,
            db,
            repo,
            workspace,
            format,
        } => cmd_blast(
            db.as_deref(),
            repo.as_deref(),
            workspace.as_deref(),
            &file,
            BlastFormat::parse(&format),
        )
        .map(Some),
        Command::Rename {
            old,
            new,
            db,
            repo,
            apply,
            force,
            uid,
        } => cmd_rename(
            &db_path(db.as_deref()),
            repo.as_deref(),
            &old,
            &new,
            apply,
            force,
            uid.as_deref(),
        )
        .map(Some),
        Command::Init {
            agent,
            path,
            yes,
            kiro_version,
            global,
            scope,
        } => run_init(agent.as_deref(), &path, yes, &kiro_version, global, scope.as_deref()),
    };

    match result {
        Ok(Some(output)) => {
            println!("{output}");
            ExitCode::SUCCESS
        }
        Ok(None) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::from(err.exit_code() as u8)
        }
    }
}
