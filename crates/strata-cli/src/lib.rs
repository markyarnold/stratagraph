//! strata-cli: the `strata` binary's command handlers.
//!
//! Each subcommand is a small, testable function that takes its arguments and
//! returns a `Result<String, CliError>`. `main` prints the `Ok` string and maps
//! the `Err` variant to a friendly message + process exit code. Keeping the
//! handlers free of `println!`/`std::process::exit` makes them unit-testable.

use std::path::{Path, PathBuf};

use strata_core::{context, explain, impact, query, Explanation, Graph, ImpactOptions, Node};
use strata_index::{
    blast_for_file, blast_for_file_in_repo, detect_changes, detect_changes_in_repo,
    index_estate_with_options, index_repo_named, index_repo_with_options, link_estate, rename,
    BlastReport, ChangeScope, FileChange, IndexContext, IndexOptions, Plane, RenameOptions,
    RenameOutcome, ResolveMode, Risk, RiskLevel, WorkspaceManifest,
};
use strata_mcp::{resolve_symbol, serve_stdio_reloadable, GraphReloader, ResolveOutcome, ToolCtx};
use strata_store::{DuckGraphStore, GraphStore};

pub mod init;
mod reload;

pub use reload::{SingleDbReloader, WorkspaceReloader};

/// Default on-disk graph database, relative to the current directory.
pub const DEFAULT_DB: &str = ".strata/graph.duckdb";

/// Errors surfaced by the CLI handlers. `main` maps each to an exit code.
#[derive(Debug)]
pub enum CliError {
    /// The graph database does not exist yet (run `strata index` first).
    NoIndex { db: PathBuf },
    /// The requested symbol matched nothing in the graph.
    SymbolNotFound { symbol: String },
    /// The symbol matched more than one node; `message` lists the candidates.
    Ambiguous { message: String },
    /// Any underlying store / IO / index failure.
    Other(String),
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CliError::NoIndex { db } => write!(
                f,
                "no index found at {}; run `strata index <repo>` first",
                db.display()
            ),
            CliError::SymbolNotFound { symbol } => write!(
                f,
                "symbol not found: {symbol} (try `strata query {symbol}` to search)"
            ),
            CliError::Ambiguous { message } => write!(f, "{message}"),
            CliError::Other(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for CliError {}

/// The process exit code each error maps to.
///
/// `2` is reserved for an *ambiguous* symbol (the caller must disambiguate),
/// matching the plan's contract; everything else is the generic failure `1`.
impl CliError {
    pub fn exit_code(&self) -> i32 {
        match self {
            CliError::Ambiguous { .. } => 2,
            _ => 1,
        }
    }
}

// ── DB helpers ────────────────────────────────────────────────────────────────

/// Resolve the DB path: the explicit `--db` if given, else [`DEFAULT_DB`].
pub fn db_path(db: Option<&Path>) -> PathBuf {
    db.map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_DB))
}

/// Open an existing graph DB for a query command, erroring clearly if absent.
fn load_existing_graph(db: &Path) -> Result<Graph, CliError> {
    if !db.exists() {
        return Err(CliError::NoIndex {
            db: db.to_path_buf(),
        });
    }
    let store = DuckGraphStore::open(db).map_err(|e| CliError::Other(e.to_string()))?;
    store
        .load_graph()
        .map_err(|e| CliError::Other(e.to_string()))
}

/// Resolve the working repo for `strata mcp`: `--repo` if given, else a non-empty
/// `$CLAUDE_PROJECT_DIR` (set by Claude Code for stdio MCP servers, so a global
/// server resolves the project the user is in), else the process cwd. Returns
/// `None` when none resolves, so the `--workspace` route keeps its clean
/// "needs a repo root" error while the auto-resolve route supplies its own `.`.
pub fn resolve_mcp_cwd(
    repo: Option<&Path>,
    project_dir_env: Option<&str>,
    cwd: Option<PathBuf>,
) -> Option<PathBuf> {
    if let Some(r) = repo {
        return Some(r.to_path_buf());
    }
    if let Some(env) = project_dir_env {
        if !env.is_empty() {
            return Some(PathBuf::from(env));
        }
    }
    cwd
}

// ── workspace helpers ──────────────────────────────────────────────────────────

/// Load and validate a workspace manifest from `manifest_path`.
fn load_manifest(manifest_path: &Path) -> Result<(WorkspaceManifest, PathBuf), CliError> {
    let manifest = WorkspaceManifest::parse_file(manifest_path)
        .map_err(|e| CliError::Other(format!("workspace manifest error: {e}")))?;
    let manifest_dir = manifest_path
        .parent()
        .unwrap_or(Path::new("."))
        .to_path_buf();
    Ok((manifest, manifest_dir))
}

/// Load a linked estate graph from a workspace manifest, returning the graph
/// **and** the per-repo load statuses.
///
/// This is the testable core the `strata mcp --workspace` route serves (and the
/// shared seam every `--workspace` query handler funnels through). It mirrors the
/// desktop's `open_workspace` (`apps/strata-desktop/src-tauri/src/commands.rs`):
/// `WorkspaceManifest::parse_file` → `link_estate` (the cross-repo linker, not a
/// bare union, so the estate sees deduped canonical contract nodes and the
/// cross-repo CONSUMES edges — brief §4) → per-repo outcomes surfaced.
///
/// Failure policy:
/// * a malformed/missing manifest → `Err` (parse/validate failure);
/// * a single broken repo → recorded `ok: false` in the returned statuses, the
///   good repos still load (graceful degradation, R2);
/// * **every** repo failing → `Err`: an empty estate served silently would be a
///   lie, so an all-failed load is an error, not a zero-node graph.
///
/// Per-repo failures are also reported to **stderr** at load time
/// (`[estate] <repo>: <error>`) so a server launched over stdio still tells the
/// operator which repo did not load while serving the rest.
pub fn load_workspace_graph(
    manifest_path: &Path,
) -> Result<(Graph, Vec<strata_index::RepoIndexResult>), CliError> {
    let (manifest, manifest_dir) = load_manifest(manifest_path)?;
    let (graph, _coverage, results) = link_estate(&manifest, &manifest_dir);

    // Surface any per-repo load failures on stderr so a server started over stdio
    // still reports which repo did not load (good repos are still served).
    for r in &results {
        if !r.ok {
            eprintln!(
                "[estate] {}: {}",
                r.name,
                r.error.as_deref().unwrap_or("unknown error")
            );
        }
    }

    // An estate whose every repo failed has nothing real to serve — error out
    // rather than hand back an empty graph that looks like a clean, tiny estate.
    if !results.is_empty() && results.iter().all(|r| !r.ok) {
        return Err(CliError::Other(format!(
            "no repos in the estate at {} could be loaded ({} repo(s) failed; run `strata index --workspace` first)",
            manifest_path.display(),
            results.len()
        )));
    }

    Ok((graph, results))
}

/// Load the estate graph from a manifest, mapping errors to [`CliError`].
///
/// Thin wrapper over [`load_workspace_graph`] for the `--workspace` query
/// handlers, which only need the graph (the per-repo statuses are already
/// reported to stderr by `load_workspace_graph`).
fn load_estate_graph(manifest_path: &Path) -> Result<Graph, CliError> {
    let (graph, _results) = load_workspace_graph(manifest_path)?;
    Ok(graph)
}

// ── index ─────────────────────────────────────────────────────────────────────

/// `strata index <repo> [--db <p>]` — build/refresh the graph and report counts.
///
/// Creates the parent directory of `db` (e.g. `.strata/`) if needed.
///
/// **Estate-aware:** when `repo` carries a valid `.strata/estate.toml` marker,
/// the graph is indexed under the manifest-declared name (the estate UID
/// `package`) rather than the directory basename.  This keeps the member's
/// `graph.duckdb` consistent with the estate so `link_estate` can union it
/// without UID collisions.  The marker file itself is untouched.
pub fn cmd_index(repo: &Path, db: &Path, include_vendored: bool) -> Result<String, CliError> {
    if let Some(parent) = db.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| {
                CliError::Other(format!("could not create {}: {e}", parent.display()))
            })?;
        }
    }
    let mut store = DuckGraphStore::open(db).map_err(|e| CliError::Other(e.to_string()))?;
    let options = IndexOptions {
        include_vendored,
        ..IndexOptions::default()
    };
    // When the repo is an estate member, use the manifest-declared name so
    // all node UIDs are estate-qualified (package = manifest name, not basename).
    // The marker file (.strata/estate.toml) is left untouched — only
    // graph.duckdb is rewritten.
    let stats = match strata_index::resolve_context(repo) {
        IndexContext::Estate {
            repo: repo_name, ..
        } => index_repo_named(repo, &repo_name, &mut store, &options)
            .map_err(|e| CliError::Other(e.to_string()))?,
        IndexContext::Single { .. } => index_repo_with_options(repo, &mut store, &options)
            .map_err(|e| CliError::Other(e.to_string()))?,
    };

    let mut out = format!(
        "Indexed {repo}\n  engine:        {engine}\n  files indexed: {indexed}\n  files parsed:  {parsed}\n  files reused:  {reused}\n  nodes:         {nodes}\n  edges:         {edges}",
        engine = strata_core::ENGINE_ID,
        repo = repo.display(),
        indexed = stats.files_indexed,
        parsed = stats.files_parsed,
        reused = stats.files_reused,
        nodes = stats.nodes,
        edges = stats.edges,
    );
    // Infrastructure-plane summary — shown when CFN/SAM templates were detected OR
    // when one failed to parse, so a real template can never be silently skipped.
    // A plain code repo (no templates, no failures) prints nothing extra.
    let infra = &stats.infra_link;
    if infra.templates_detected > 0 || infra.templates_failed > 0 {
        // Each per-template parse failure on its own line first, so it is
        // impossible to miss (the 8k-line silent-skip must never recur).
        for diag in &stats.infra_diagnostics {
            out.push_str(&format!("\n  [infra] FAILED {diag}"));
        }
        out.push_str(&format!(
            "\n  infra:         {templates} template(s), {failed} failed, {resources} resource(s); \
             {resolvers_linked}/{resolvers_total} resolvers linked, \
             {runs_linked} Lambda(s) → handler",
            templates = infra.templates_detected,
            failed = infra.templates_failed,
            resources = infra.resources_total,
            resolvers_linked = infra.resolvers_linked,
            resolvers_total = infra.resolvers_total,
            runs_linked = infra.lambdas_runs_linked,
        ));
    }
    // Terragrunt structural summary — shown only when units were detected (a repo
    // with no `terragrunt.hcl` prints nothing extra). Surfaces the unit count and
    // how many declared dependencies resolved to a known same-repo unit.
    let tg = &stats.terragrunt;
    if tg.units_detected > 0 {
        out.push_str(&format!(
            "\n  terragrunt:    {units} unit(s); {linked}/{total} dependencies resolved",
            units = tg.units_detected,
            linked = tg.deps_linked,
            total = tg.deps_total,
        ));
    }
    // Data-plane summary — shown when `.sql` schema files were detected OR one
    // failed to parse, so a real schema can never be silently skipped. A repo with
    // no SQL schema prints nothing extra (additive).
    let data = &stats.data_link;
    if data.schemas_detected > 0 || data.schemas_failed > 0 {
        for diag in &stats.data_diagnostics {
            out.push_str(&format!("\n  [data] FAILED {diag}"));
        }
        out.push_str(&format!(
            "\n  data:          {schemas} schema(s), {failed} failed, {tables} table(s), \
             {columns} column(s); {fks_linked}/{fks_total} foreign keys linked",
            schemas = data.schemas_detected,
            failed = data.schemas_failed,
            tables = data.tables_total,
            columns = data.columns_total,
            fks_linked = data.fks_linked,
            fks_total = data.fks_total,
        ));
        // Code→table links: raw-SQL Reads/Writes (M2) and ORM model→table MapsTo
        // (M2b). Each prints linked/total so an unresolved count is visible (honest
        // never-invent accounting).
        out.push_str(&format!(
            "\n  data:          {reads}/{reads_total} reads, {writes}/{writes_total} writes, \
             {orm}/{orm_total} ORM model(s) linked to tables",
            reads = data.reads_linked,
            reads_total = data.reads_linked + data.reads_unresolved,
            writes = data.writes_linked,
            writes_total = data.writes_linked + data.writes_unresolved,
            orm = data.orm_models_linked,
            orm_total = data.orm_models_total,
        ));
        // Informational (NOT a failure): individual statements skipped inside files
        // that DID parse — a PL/pgSQL `DO`/`CREATE FUNCTION` body sqlparser can't
        // read. The surrounding tables were still extracted; this just makes the
        // skip visible (the data-plane robustness signal).
        if data.statements_skipped > 0 {
            out.push_str(&format!(
                "\n  data:          {skipped} unparseable statement(s) skipped \
                 (tables around them were kept)",
                skipped = data.statements_skipped,
            ));
        }
    }
    Ok(out)
}

/// `strata index --workspace <manifest.toml> [--resolve auto|on|off] [--include-vendored]` —
/// index the estate.
pub fn cmd_index_workspace(
    manifest_path: &Path,
    resolve: ResolveMode,
    include_vendored: bool,
) -> Result<String, CliError> {
    let (manifest, _manifest_dir) = load_manifest(manifest_path)?;
    let stats = index_estate_with_options(&manifest, manifest_path, resolve, include_vendored);

    let mut out = format!(
        "Indexed estate '{}' ({} repos)\n",
        stats.estate,
        stats.repos.len()
    );
    for r in &stats.repos {
        if let Some(s) = &r.stats {
            out.push_str(&format!(
                "  [ok] {}: {} nodes, {} edges\n",
                r.name, s.nodes, s.edges
            ));
        } else {
            out.push_str(&format!(
                "  [FAIL] {}: {}\n",
                r.name,
                r.error.as_deref().unwrap_or("unknown error")
            ));
        }
    }
    out.push_str(&format!(
        "  total: {} nodes, {} edges",
        stats.total_nodes, stats.total_edges
    ));
    Ok(out)
}

// ── impact ─────────────────────────────────────────────────────────────────────

/// The §15.6 will-break verdict as a printer label — the call in words, for the
/// affected-node tables shared by `impact` and `detect-changes`.
fn break_verdict(will_break: bool) -> &'static str {
    if will_break {
        "WILL BREAK"
    } else {
        "may affect"
    }
}

/// Header for the affected-node table, indented to match [`affected_row`] (the
/// impact printers pass two spaces, the change report four).
fn affected_header(indent: &str) -> String {
    format!(
        "{indent}{:>5}  {:>4}  {:>3}  {:<10}  {}\n",
        "depth", "conf", "amb", "verdict", "name (path)"
    )
}

/// One affected-node row — the columnar shape
/// `depth  conf  amb  verdict  name (path)`, prefixed by `indent`. `verdict` is
/// the §15.6 will-break call ([`break_verdict`]); the depth/confidence/ambiguity
/// columns are unchanged from before the label existed.
fn affected_row(
    indent: &str,
    depth: usize,
    confidence: f32,
    ambiguous: bool,
    will_break: bool,
    name: &str,
    path: &str,
) -> String {
    format!(
        "{indent}{depth:>5}  {confidence:>4.2}  {amb:>3}  {verdict:<10}  {name} ({path})\n",
        amb = if ambiguous { "yes" } else { "no" },
        verdict = break_verdict(will_break),
    )
}

/// How many member names the zero-direct hint lists before summarising the rest
/// as a count — a short, runnable nudge, not a full dump.
const MEMBER_HINT_MAX: usize = 5;

/// Render the empty-`affected` tail of an impact report into `out`.
///
/// The honesty pivot for a **member-bearing** target: impact reverse-walks
/// INCOMING edges and members hang off OUTGOING `Defines`/`HasColumn`, so
/// `impact(Type)` can read zero even when a method/column HAS dependents. Rather
/// than the bare, misleading "nothing depends on this" (which looks dead and
/// borderline violates "NEVER claim nothing depends on this"), when
/// `members_with_dependents` is non-empty we say so and point the user at a member
/// with a runnable `strata impact <member>`. The hint is framed as "its members
/// have dependents" — never "the type has these direct dependents".
///
/// When `members_with_dependents` is empty (a genuinely dead container, or a
/// member-less target) the original bare line is printed unchanged — dead = dead,
/// honesty preserved.
fn render_zero_affected(out: &mut String, name: &str, members: &[strata_core::MemberDependent]) {
    if members.is_empty() {
        out.push_str("  (nothing depends on this within the given depth/confidence)");
        return;
    }
    // 0 direct deps on the TYPE, but N members do — name a few + a runnable step.
    let shown: Vec<&str> = members
        .iter()
        .take(MEMBER_HINT_MAX)
        .map(|m| m.name.as_str())
        .collect();
    let more = members.len().saturating_sub(shown.len());
    let suffix = if more > 0 {
        format!(", … (+{more} more)")
    } else {
        String::new()
    };
    out.push_str(&format!(
        "  0 dependents on {name} itself; {n} of its members have dependents: {list}{suffix}\n",
        n = members.len(),
        list = shown.join(", "),
    ));
    // A concrete, runnable next step on the first such member.
    out.push_str(&format!(
        "  try: strata impact {first}",
        first = members[0].name,
    ));
}

/// `strata impact <symbol> [--db] [--depth] [--min-confidence] [--no-contracts]
/// [--no-infra]` — reverse blast radius.
///
/// `include_contracts`/`include_infra` default to true at the CLI surface (the
/// `--no-contracts`/`--no-infra` flags set them false): contract-aware impact
/// surfaces cross-plane/cross-repo consumers of a producer (brief §5); infra-aware
/// impact surfaces an IamRole's assuming Lambdas and their reach (§6.3). A graph
/// with no contract/infra edges is unaffected either way.
#[allow(clippy::too_many_arguments)]
pub fn cmd_impact(
    db: &Path,
    symbol: &str,
    depth: usize,
    min_confidence: f32,
    include_contracts: bool,
    include_infra: bool,
    uid: Option<&str>,
) -> Result<String, CliError> {
    let graph = load_existing_graph(db)?;
    let node = resolve_one_or_uid(&graph, symbol, uid)?;

    let opts = ImpactOptions {
        max_depth: depth,
        min_confidence,
        include_imports: false,
        include_contracts,
        include_infra,
    };
    let result = impact(&graph, &node.uid, &opts);

    let mut out = String::new();
    out.push_str(&format!(
        "Impact of {} ({}) — {} affected:\n",
        node.name,
        node.path,
        result.affected.len()
    ));
    if result.affected.is_empty() {
        render_zero_affected(&mut out, &node.name, &result.members_with_dependents);
        return Ok(out);
    }
    out.push_str(&affected_header("  "));
    for a in &result.affected {
        // The node always exists in the graph (impact only reports reachable uids).
        let path = graph
            .get_node(&a.uid)
            .map(|n| n.path.clone())
            .unwrap_or_default();
        out.push_str(&affected_row(
            "  ",
            a.depth,
            a.confidence,
            a.ambiguous,
            a.will_break,
            &a.name,
            &path,
        ));
    }
    Ok(out.trim_end().to_string())
}

// ── explain ──────────────────────────────────────────────────────────────────

/// Render the evidence chain for `explain(target, affected)` as the human-readable
/// CLI output — the §14.2 Track E1 "why is X affected?" view. Shared by the
/// single-repo and `--workspace` handlers, so both print the identical shape:
///
/// ```text
/// Why getUser affects client.ts (conf 0.90, WILL BREAK):
///   getUser  —PRODUCES (Extracted 0.95)→  Query.getUser           running 0.95
///   Query.getUser  ←CONSUMES (Extracted 0.95)←  client.ts          running 0.90
/// ```
///
/// The header carries the overall confidence and the §15.6 will-break verdict
/// (reusing [`break_verdict`]/[`will_break_label`], the same call impact prints);
/// each hop names the edge kind, its provenance + own confidence, and the running
/// (accumulated) confidence. AMBIGUOUS hops are explicitly marked.
fn render_explanation(
    graph: &Graph,
    target: &Node,
    affected: &Node,
    explanation: &Explanation,
) -> String {
    // Show the friendly node name for each hop endpoint (the uid is the fallback
    // for a node that somehow vanished). Reading names, not uids, is the whole
    // point — the chain must be legible.
    let display = |uid: &strata_core::Uid| -> String {
        graph
            .get_node(uid)
            .map(|n| n.name.clone())
            .unwrap_or_else(|| uid.as_str().to_string())
    };

    let will_break = strata_core::will_break_label(explanation.confidence, explanation.ambiguous);
    let mut out = format!(
        "Why {} affects {} (conf {:.2}, {}{}):\n",
        target.name,
        affected.name,
        explanation.confidence,
        break_verdict(will_break),
        if explanation.ambiguous {
            ", via AMBIGUOUS"
        } else {
            ""
        },
    );
    if explanation.hops.is_empty() {
        // target == affected: a node trivially explains itself.
        out.push_str("  (the target is the affected node — nothing to traverse)");
        return out;
    }
    for hop in &explanation.hops {
        let amb = if hop.provenance == strata_core::Provenance::Ambiguous {
            "  [AMBIGUOUS]"
        } else {
            ""
        };
        // The edge kind is shouted (PRODUCES/CONSUMES/CALLS/…) and the provenance
        // shown as-is (Extracted/Inferred/Ambiguous). Bound out of the outer
        // format! so the args are plain (clippy::format_in_format_args).
        let kind = format!("{:?}", hop.edge_kind).to_uppercase();
        let prov = format!("{:?}", hop.provenance);
        out.push_str(&format!(
            "  {from}  —{kind} ({prov} {conf:.2})→  {to}    running {running:.2}{amb}\n",
            from = display(&hop.from),
            conf = hop.confidence,
            to = display(&hop.to),
            running = hop.running_confidence,
        ));
    }
    out.trim_end().to_string()
}

/// The honest not-in-blast-radius line for the `explain` printers: `affected` does
/// not depend on `target`, so there is nothing to explain (an explicit negative,
/// never a silent empty success).
fn explain_unreachable_line(target: &Node, affected: &Node) -> String {
    format!(
        "{} is not in {}'s blast radius (nothing to explain)",
        affected.name, target.name
    )
}

/// `strata explain <target> <affected> [--db] [--depth] [--min-confidence]
/// [--no-contracts] [--no-infra]` — the evidence chain explaining why `affected`
/// is in `target`'s blast radius.
///
/// Reuses the SAME reverse walk `impact` runs (via [`strata_core::explain`]), so
/// the rendered overall confidence equals the number `strata impact` reports for
/// that node. An unreachable affected node prints the honest "not in blast radius"
/// line; `target == affected` prints the trivial self case.
#[allow(clippy::too_many_arguments)]
pub fn cmd_explain(
    db: &Path,
    target: &str,
    affected: &str,
    depth: usize,
    min_confidence: f32,
    include_contracts: bool,
    include_infra: bool,
    uid: Option<&str>,
    affected_uid: Option<&str>,
) -> Result<String, CliError> {
    let graph = load_existing_graph(db)?;
    explain_over_graph(
        &graph,
        target,
        affected,
        depth,
        min_confidence,
        include_contracts,
        include_infra,
        uid,
        affected_uid,
    )
}

/// The shared core of `strata explain` (single-repo and `--workspace`): resolve
/// both ends over `graph`, run [`strata_core::explain`], and render the chain.
///
/// `uid` pins the TARGET and `affected_uid` pins the AFFECTED end when either is
/// ambiguous (matching the MCP `explain` tool's `uid` / `affected_uid`); without a
/// pin each end disambiguates by fqn, and an ambiguous end lists its candidates +
/// the `--uid`/`--affected-uid` hint rather than dead-ending.
#[allow(clippy::too_many_arguments)]
fn explain_over_graph(
    graph: &Graph,
    target: &str,
    affected: &str,
    depth: usize,
    min_confidence: f32,
    include_contracts: bool,
    include_infra: bool,
    uid: Option<&str>,
    affected_uid: Option<&str>,
) -> Result<String, CliError> {
    let target_node = resolve_one_or_uid(graph, target, uid)?;
    let affected_node = resolve_one_or_uid(graph, affected, affected_uid)?;

    let opts = ImpactOptions {
        max_depth: depth,
        min_confidence,
        include_imports: false,
        include_contracts,
        include_infra,
    };
    match explain(graph, &target_node.uid, &affected_node.uid, &opts) {
        None => Ok(explain_unreachable_line(&target_node, &affected_node)),
        Some(explanation) => Ok(render_explanation(
            graph,
            &target_node,
            &affected_node,
            &explanation,
        )),
    }
}

// ── context ────────────────────────────────────────────────────────────────────

/// `strata context <symbol> [--db]` — the 360° view of one symbol.
pub fn cmd_context(db: &Path, symbol: &str) -> Result<String, CliError> {
    let graph = load_existing_graph(db)?;
    let node = resolve_one(&graph, symbol)?;
    // context() returns Some because the uid came straight from the graph.
    let ctx = context(&graph, &node.uid)
        .ok_or_else(|| CliError::Other("internal: resolved node vanished".into()))?;

    let mut out = String::new();
    out.push_str(&format!(
        "Context for {} ({:?}) — {}\n  uid: {}\n",
        ctx.node.name, ctx.node.kind, ctx.node.path, ctx.node.uid
    ));
    if let Some(container) = &ctx.container {
        out.push_str(&format!(
            "  container: {} ({})\n",
            container.name, container.path
        ));
    }
    // Contract plane first (additive) — for a schema field/operation these are the
    // buckets that apply; always printed, so a dead field reads `producers (0)`.
    bucket(&mut out, "producers", &ctx.producers);
    bucket(&mut out, "consumers", &ctx.consumers);
    bucket(&mut out, "produces", &ctx.produces);
    bucket(&mut out, "consumes", &ctx.consumes);
    // Infra plane next — a role's `assumed_by` lists its Lambdas, the
    // resolver→DS→lambda chain shows from both ends, a handler module's `run_by`
    // lists its Lambda. Always printed (empty → `(0)`).
    bucket(&mut out, "assumes", &ctx.assumes);
    bucket(&mut out, "assumed_by", &ctx.assumed_by);
    bucket(&mut out, "routes_to", &ctx.routes_to);
    bucket(&mut out, "routed_from", &ctx.routed_from);
    bucket(&mut out, "runs", &ctx.runs);
    bucket(&mut out, "run_by", &ctx.run_by);
    // Data plane — a Table's `mapped_by` lists the ORM model classes that map to it;
    // a model class's `maps_to` is its table (Slice 25). Always printed.
    bucket(&mut out, "mapped_by", &ctx.mapped_by);
    bucket(&mut out, "maps_to", &ctx.maps_to);
    bucket(&mut out, "callers", &ctx.callers);
    bucket(&mut out, "callees", &ctx.callees);
    bucket(&mut out, "imports_in", &ctx.imports_in);
    bucket(&mut out, "imports_out", &ctx.imports_out);
    bucket(&mut out, "members", &ctx.members);
    Ok(out.trim_end().to_string())
}

fn bucket(out: &mut String, label: &str, nodes: &[strata_core::Node]) {
    out.push_str(&format!("  {label} ({}):\n", nodes.len()));
    for n in nodes {
        out.push_str(&format!("    - {} ({})\n", n.name, n.path));
    }
}

// ── query ──────────────────────────────────────────────────────────────────────

/// `strata query <text> [--db]` — lexical search over name/fqn/path.
pub fn cmd_query(db: &Path, text: &str) -> Result<String, CliError> {
    let graph = load_existing_graph(db)?;
    let hits = query(&graph, text);
    if hits.is_empty() {
        return Ok(format!("No matches for {text:?}"));
    }
    let mut out = format!("{} match(es) for {text:?}:\n", hits.len());
    for n in &hits {
        out.push_str(&format!(
            "  {} [{:?}] {}\n    {}\n",
            n.name, n.kind, n.path, n.uid
        ));
    }
    // `query` finds nodes; the relationships live one command away. Humans get
    // the same next-step steering the agent kit teaches.
    out.push_str(
        "\nNext: `strata context <name>` for relationships, `strata impact <name>` for blast radius.\n",
    );
    Ok(out.trim_end().to_string())
}

// ── workspace variants ─────────────────────────────────────────────────────────

/// `strata impact <symbol> --workspace <manifest> [--no-contracts] [--no-infra]`
/// — estate blast radius. `include_contracts`/`include_infra` default to true (the
/// `--no-contracts`/`--no-infra` flags set them false): contract-aware impact
/// surfaces cross-repo consumers of a producer (brief §5); infra-aware impact
/// surfaces an IamRole's assuming Lambdas and their reach (§6.3).
#[allow(clippy::too_many_arguments)]
pub fn cmd_impact_workspace(
    manifest_path: &Path,
    symbol: &str,
    depth: usize,
    min_confidence: f32,
    include_contracts: bool,
    include_infra: bool,
    uid: Option<&str>,
) -> Result<String, CliError> {
    let graph = load_estate_graph(manifest_path)?;
    let node = resolve_one_or_uid(&graph, symbol, uid)?;

    let opts = ImpactOptions {
        max_depth: depth,
        min_confidence,
        include_imports: false,
        include_contracts,
        include_infra,
    };
    let result = impact(&graph, &node.uid, &opts);

    let mut out = String::new();
    out.push_str(&format!(
        "Impact of {} ({}) — {} affected:\n",
        node.name,
        node.path,
        result.affected.len()
    ));
    if result.affected.is_empty() {
        render_zero_affected(&mut out, &node.name, &result.members_with_dependents);
        return Ok(out);
    }
    out.push_str(&affected_header("  "));
    for a in &result.affected {
        let path = graph
            .get_node(&a.uid)
            .map(|n| n.path.clone())
            .unwrap_or_default();
        out.push_str(&affected_row(
            "  ",
            a.depth,
            a.confidence,
            a.ambiguous,
            a.will_break,
            &a.name,
            &path,
        ));
    }
    Ok(out.trim_end().to_string())
}

/// `strata explain <target> <affected> --workspace <manifest> [--no-contracts]
/// [--no-infra]` — the evidence chain over the estate graph. Same renderer and
/// same engine walk as the single-repo route, so the overall confidence equals
/// the estate `impact`'s number for that node.
#[allow(clippy::too_many_arguments)]
pub fn cmd_explain_workspace(
    manifest_path: &Path,
    target: &str,
    affected: &str,
    depth: usize,
    min_confidence: f32,
    include_contracts: bool,
    include_infra: bool,
    uid: Option<&str>,
    affected_uid: Option<&str>,
) -> Result<String, CliError> {
    let graph = load_estate_graph(manifest_path)?;
    explain_over_graph(
        &graph,
        target,
        affected,
        depth,
        min_confidence,
        include_contracts,
        include_infra,
        uid,
        affected_uid,
    )
}

/// `strata context <symbol> --workspace <manifest>` — estate 360° view.
pub fn cmd_context_workspace(manifest_path: &Path, symbol: &str) -> Result<String, CliError> {
    let graph = load_estate_graph(manifest_path)?;
    let node = resolve_one(&graph, symbol)?;
    let ctx = context(&graph, &node.uid)
        .ok_or_else(|| CliError::Other("internal: resolved node vanished".into()))?;

    let mut out = String::new();
    out.push_str(&format!(
        "Context for {} ({:?}) — {}\n  uid: {}\n",
        ctx.node.name, ctx.node.kind, ctx.node.path, ctx.node.uid
    ));
    if let Some(container) = &ctx.container {
        out.push_str(&format!(
            "  container: {} ({})\n",
            container.name, container.path
        ));
    }
    // Contract plane first (additive) — same ordering as the single-repo printer.
    bucket(&mut out, "producers", &ctx.producers);
    bucket(&mut out, "consumers", &ctx.consumers);
    bucket(&mut out, "produces", &ctx.produces);
    bucket(&mut out, "consumes", &ctx.consumes);
    // Infra plane next — same ordering as the single-repo printer.
    bucket(&mut out, "assumes", &ctx.assumes);
    bucket(&mut out, "assumed_by", &ctx.assumed_by);
    bucket(&mut out, "routes_to", &ctx.routes_to);
    bucket(&mut out, "routed_from", &ctx.routed_from);
    bucket(&mut out, "runs", &ctx.runs);
    bucket(&mut out, "run_by", &ctx.run_by);
    // Data plane — a Table's `mapped_by` lists the ORM model classes that map to it;
    // a model class's `maps_to` is its table (Slice 25). Always printed.
    bucket(&mut out, "mapped_by", &ctx.mapped_by);
    bucket(&mut out, "maps_to", &ctx.maps_to);
    bucket(&mut out, "callers", &ctx.callers);
    bucket(&mut out, "callees", &ctx.callees);
    bucket(&mut out, "imports_in", &ctx.imports_in);
    bucket(&mut out, "imports_out", &ctx.imports_out);
    bucket(&mut out, "members", &ctx.members);
    Ok(out.trim_end().to_string())
}

/// `strata query <text> --workspace <manifest>` — lexical search over the estate.
pub fn cmd_query_workspace(manifest_path: &Path, text: &str) -> Result<String, CliError> {
    let graph = load_estate_graph(manifest_path)?;
    let hits = query(&graph, text);
    if hits.is_empty() {
        return Ok(format!("No matches for {text:?}"));
    }
    let mut out = format!("{} match(es) for {text:?}:\n", hits.len());
    for n in &hits {
        out.push_str(&format!(
            "  {} [{:?}] {}\n    {}\n",
            n.name, n.kind, n.path, n.uid
        ));
    }
    // `query` finds nodes; the relationships live one command away. Humans get
    // the same next-step steering the agent kit teaches.
    out.push_str(
        "\nNext: `strata context <name>` for relationships, `strata impact <name>` for blast radius.\n",
    );
    Ok(out.trim_end().to_string())
}

// ── mcp ────────────────────────────────────────────────────────────────────────

/// The resolved launch mode for `strata mcp` auto-resolution (no explicit
/// `--db` or `--workspace`). Pure (no I/O beyond reading the estate marker):
/// testable without a running server.
#[derive(Debug, PartialEq)]
pub enum McpLaunch {
    /// No estate marker present: serve the local `.strata/graph.duckdb`.
    Single {
        db: PathBuf,
        repo_root: Option<PathBuf>,
    },
    /// Estate marker found and validated: serve the linked estate graph and
    /// carry the member repo root in `ToolCtx` so `detect_changes` can diff it.
    Estate {
        manifest: PathBuf,
        repo_root: PathBuf,
    },
}

/// Resolve the MCP launch mode from a working directory (pure, no graph load).
///
/// Calls [`strata_index::resolve_context`] to check for an estate marker in
/// `cwd/.strata/estate.toml`. If found and valid, returns `Estate { manifest,
/// repo_root: cwd }`; otherwise returns `Single { db, repo_root: Some(cwd) }`.
/// This is the testable seam: the serve wiring in `Command::Mcp` dispatch
/// calls this and then branches into the appropriate `serve_reloadable` path.
pub fn resolve_mcp_launch(cwd: &Path) -> McpLaunch {
    match strata_index::resolve_context(cwd) {
        IndexContext::Estate { manifest, .. } => McpLaunch::Estate {
            manifest,
            repo_root: cwd.to_path_buf(),
        },
        IndexContext::Single { db } => McpLaunch::Single {
            db,
            repo_root: Some(cwd.to_path_buf()),
        },
    }
}

/// `strata mcp [--db] [--repo]` — load the graph from a single DuckDB store and
/// serve the MCP stdio server.
///
/// The `detect_changes` tool needs the repository working directory; it is
/// derived as the **grandparent** of `--db` when the DB path ends in the
/// canonical `.strata/graph.duckdb` (so `<repo>/.strata/graph.duckdb` → `<repo>`),
/// overridable by an explicit `--repo <path>`. When neither yields a root (a
/// non-canonical `--db` and no `--repo`), the ctx carries `None` and
/// `detect_changes` returns its clear "needs a repo root" error.
pub fn cmd_mcp(db: &Path, repo: Option<&Path>) -> Result<(), CliError> {
    let graph = load_existing_graph(db)?;
    let repo_root = repo
        .map(Path::to_path_buf)
        .or_else(|| repo_root_from_db(db));
    // Hot-reload (Track E3): serve the loaded graph but pick up an on-disk
    // reindex (the editor's PostToolUse `strata index` hook, or a manual one)
    // without a restart. The reloader is baselined to the db's current signal so
    // nothing looks stale until the next index; a failed reload keeps this graph.
    let reloader = SingleDbReloader::new(db);
    serve_reloadable(graph, reloader, ToolCtx { repo_root })
}

/// `strata mcp --workspace <manifest>` — load a **linked estate** graph and serve
/// it over MCP. Same serve path as the `--db` route, so the tool surface is
/// byte-identical; per-repo load outcomes are reported to stderr at startup by
/// [`load_workspace_graph`] (good repos served, all-failed → error).
///
/// `repo_root` is the member repo directory the agent is currently working in
/// (passed from the cwd or `--repo` flag in the dispatch). When given, the ctx
/// carries it so `detect_changes` can git-diff that member's working tree. When
/// absent (e.g. an explicit `--workspace` launched from outside any member repo),
/// `detect_changes` returns its clear "needs a repo root" error.
pub fn cmd_mcp_workspace(manifest_path: &Path, repo_root: Option<&Path>) -> Result<(), CliError> {
    let (graph, _results) = load_workspace_graph(manifest_path)?;
    // Hot-reload the estate too: a change to the manifest or any repo's index
    // swaps in a freshly-linked estate graph (degrade-safe; per-repo failures
    // degrade per the existing estate policy).
    let reloader = WorkspaceReloader::new(manifest_path);
    serve_reloadable(
        graph,
        reloader,
        ToolCtx {
            repo_root: repo_root.map(Path::to_path_buf),
        },
    )
}

/// The repository working directory implied by a `--db` path, when it has the
/// canonical `<repo>/.strata/graph.duckdb` shape: the DB's grandparent. Returns
/// `None` for any other DB path (a custom location), so the caller falls back to
/// `--repo` or to the ctx-less "needs a repo root" error.
fn repo_root_from_db(db: &Path) -> Option<PathBuf> {
    // Canonical shape: file `graph.duckdb` inside a `.strata` dir whose parent is
    // the repo. Only then is the grandparent the repo root.
    let is_graph_db = db.file_name().map(|n| n == "graph.duckdb").unwrap_or(false);
    let in_strata = db
        .parent()
        .and_then(Path::file_name)
        .map(|n| n == ".strata")
        .unwrap_or(false);
    if is_graph_db && in_strata {
        db.parent().and_then(Path::parent).map(Path::to_path_buf)
    } else {
        None
    }
}

/// Serve an already-loaded graph (+ ctx) over the **hot-reloading** MCP stdio
/// server: `reloader` supplies a fresh graph whenever the on-disk index changes
/// (degrade-safe). Both `mcp` routes funnel through here so `--db` and
/// `--workspace` differ only in which [`GraphReloader`] they build.
fn serve_reloadable(
    graph: Graph,
    reloader: impl GraphReloader,
    ctx: ToolCtx,
) -> Result<(), CliError> {
    serve_stdio_reloadable(graph, reloader, ctx)
        .map_err(|e| CliError::Other(format!("mcp server error: {e}")))
}

// ── detect-changes ───────────────────────────────────────────────────────────────

/// `strata detect-changes [--staged] [--db | --workspace] [--repo]` — the
/// mechanical pre-commit check: git-diff → changed symbols per plane →
/// aggregated blast radius over the loaded graph → risk. Human-readable; **always
/// exits 0** (it reports, it does not gate — gating is the caller's choice).
///
/// Three-way dispatch (mirrors `cmd_blast`):
/// 1. Explicit `--workspace <manifest>` → estate mode: load the full estate graph,
///    git-diff the **member** repo root, aggregate over the estate (cross-repo
///    consumers appear in the affected set). Conflicts with `--db`.
/// 2. Explicit `--db <path>` → single-repo (back-compat; no estate visible).
/// 3. Neither → auto-resolve via [`strata_index::resolve_context`] from the repo
///    root (`--repo` if given, else grandparent of the default db, else `.`).
///    Estate → estate mode; Single → existing single-repo path.
pub fn cmd_detect_changes(
    db: Option<&Path>,
    repo: Option<&Path>,
    workspace: Option<&Path>,
    staged: bool,
) -> Result<String, CliError> {
    let scope = if staged {
        ChangeScope::Staged
    } else {
        ChangeScope::Working
    };

    let report = if let Some(manifest) = workspace {
        // Explicit --workspace: estate mode.
        detect_changes_estate(manifest, repo, scope)?
    } else if let Some(db) = db {
        // Explicit --db: single-repo (back-compat, no estate).
        let graph = load_existing_graph(db)?;
        let repo_root = repo
            .map(Path::to_path_buf)
            .or_else(|| repo_root_from_db(db))
            .unwrap_or_else(|| PathBuf::from("."));
        detect_changes(&graph, &repo_root, scope).map_err(|e| CliError::Other(e.to_string()))?
    } else {
        // Auto-resolve: find the repo root, then check for an estate marker.
        let repo_root = repo
            .map(Path::to_path_buf)
            .or_else(|| {
                let default_db = PathBuf::from(DEFAULT_DB);
                repo_root_from_db(&default_db)
            })
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        match strata_index::resolve_context(&repo_root) {
            IndexContext::Estate {
                manifest,
                repo_root: member_root,
                ..
            } => detect_changes_estate(&manifest, Some(&member_root), scope)?,
            IndexContext::Single { db } => {
                let graph = load_existing_graph(&db)?;
                detect_changes(&graph, &repo_root, scope)
                    .map_err(|e| CliError::Other(e.to_string()))?
            }
        }
    };

    Ok(render_change_report(&report))
}

/// Estate detect-changes helper: load the estate graph and git-diff the member
/// repo, scoping symbol resolution to that repo so cross-repo path collisions
/// don't corrupt the changed-symbol set. The impact traversal fans out over the
/// full estate graph so cross-repo consumers appear in `affected`.
fn detect_changes_estate(
    manifest: &Path,
    repo: Option<&Path>,
    scope: ChangeScope,
) -> Result<strata_index::ChangeReport, CliError> {
    let (graph, _results) = load_workspace_graph(manifest)?;

    // Determine the member repo root and name.
    let manifest_dir = manifest.parent().unwrap_or(Path::new("."));
    let (manifest_data, _) = load_manifest(manifest)?;

    // If an explicit repo path was given, use it directly; otherwise find the
    // manifest entry whose absolute path matches the repo path.
    let (repo_root, repo_name): (PathBuf, Option<String>) = if let Some(r) = repo {
        // Explicit --repo: find the matching manifest entry by path prefix.
        let r_abs = std::fs::canonicalize(r)
            .or_else(|_| std::path::absolute(r))
            .unwrap_or_else(|_| r.to_path_buf());
        let name = manifest_data.repos.iter().find_map(|e| {
            let candidate = manifest_dir.join(&e.path);
            candidate.canonicalize().ok().and_then(|canon| {
                if r_abs.starts_with(&canon) || canon == r_abs {
                    Some(e.name.clone())
                } else {
                    None
                }
            })
        });
        (r_abs, name)
    } else {
        // No --repo: use cwd, find which manifest entry it falls under.
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let cwd_abs = std::fs::canonicalize(&cwd).unwrap_or(cwd);
        let mut found_root = cwd_abs.clone();
        let mut found_name: Option<String> = None;
        for entry in &manifest_data.repos {
            let candidate = manifest_dir.join(&entry.path);
            if let Ok(canon) = candidate.canonicalize() {
                if cwd_abs.starts_with(&canon) {
                    found_root = canon;
                    found_name = Some(entry.name.clone());
                    break;
                }
            }
        }
        (found_root, found_name)
    };

    detect_changes_in_repo(&graph, &repo_root, scope, repo_name.as_deref())
        .map_err(|e| CliError::Other(e.to_string()))
}

/// Render a [`strata_index::ChangeReport`] as the human-readable CLI output:
/// changed files, changed symbols grouped per plane, the affected table
/// (depth/conf/amb), and the `Risk: LEVEL — reasons` line.
fn render_change_report(report: &strata_index::ChangeReport) -> String {
    let mut out = String::new();
    out.push_str(&format!("Changes ({} scope)\n", report.scope));

    // ── Changed files. ──
    if report.files.is_empty() {
        out.push_str("  (no changed files)\n");
    } else {
        out.push_str(&format!("  {} changed file(s):\n", report.files.len()));
        for f in &report.files {
            let line = match f {
                FileChange::Added { path } => format!("    A  {path}"),
                FileChange::Deleted { path } => format!("    D  {path}"),
                FileChange::Modified { path } => format!("    M  {path}"),
                FileChange::Renamed { old_path, path } => format!("    R  {old_path} → {path}"),
            };
            out.push_str(&line);
            out.push('\n');
        }
    }

    // ── Changed symbols, grouped per plane. ──
    for (plane, label) in [
        (Plane::Code, "code"),
        (Plane::Contract, "contract"),
        (Plane::Infra, "infra"),
    ] {
        let in_plane: Vec<_> = report.symbols.iter().filter(|s| s.plane == plane).collect();
        if in_plane.is_empty() {
            continue;
        }
        out.push_str(&format!("  {label} symbols ({}):\n", in_plane.len()));
        for s in in_plane {
            let kind = match s.change {
                strata_index::ChangeKind::Added => "added",
                strata_index::ChangeKind::Removed => "removed",
                strata_index::ChangeKind::Modified => "modified",
            };
            // Contract-plane symbols carry the operation-level breaking/additive
            // verdict; other planes have no label.
            let label = match s.contract_change {
                Some(strata_index::ContractChange::Breaking) => "  [BREAKING]",
                Some(strata_index::ContractChange::Additive) => "  [additive]",
                None => "",
            };
            out.push_str(&format!("    {kind:>8}  {}  ({}){label}\n", s.key, s.file));
        }
    }

    // ── Non-plane files (md, config, …) — listed, never claimed to bear symbols. ──
    if !report.other_files.is_empty() {
        out.push_str(&format!("  other files ({}):\n", report.other_files.len()));
        for f in &report.other_files {
            out.push_str(&format!("    {f}\n"));
        }
    }

    // ── Aggregated blast radius. ──
    if report.affected.is_empty() {
        out.push_str("  affected: (nothing in the loaded graph depends on these changes)\n");
    } else {
        out.push_str(&format!("  affected ({}):\n", report.affected.len()));
        out.push_str(&affected_header("    "));
        for a in &report.affected {
            out.push_str(&affected_row(
                "    ",
                a.depth,
                a.confidence,
                a.ambiguous,
                a.will_break,
                &a.name,
                &a.path,
            ));
        }
    }

    // ── Risk verdict. ──
    let level = match report.risk.level {
        RiskLevel::Low => "LOW",
        RiskLevel::Medium => "MEDIUM",
        RiskLevel::High => "HIGH",
        RiskLevel::Critical => "CRITICAL",
    };
    out.push_str(&format!(
        "Risk: {level} — {}",
        report.risk.reasons.join("; ")
    ));
    out
}

// ── blast ──────────────────────────────────────────────────────────────────────

/// The output format for `strata blast`: a human summary, or the terse
/// token-lean block the pre-edit hook injects as `additionalContext`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlastFormat {
    /// A human-readable summary (the default).
    Text,
    /// The agent-facing block for hook injection: symbols, affected count, risk +
    /// reasons, and the standing pre-edit instruction.
    Agent,
}

impl BlastFormat {
    /// Parse `--format` (`text` | `agent`); anything else falls back to `text`.
    pub fn parse(s: &str) -> BlastFormat {
        match s.to_ascii_lowercase().as_str() {
            "agent" => BlastFormat::Agent,
            _ => BlastFormat::Text,
        }
    }
}

/// `strata blast <file> [--db|--repo|--workspace] [--format text|agent]` — the
/// **pre-edit blast radius of a file**: the symbols it defines, the aggregated
/// reverse blast radius of changing them, and the risk. **Always exits 0** (it
/// informs the edit; it never gates) — the same report-not-gate contract as
/// `detect-changes`.
///
/// Priority of graph source:
/// 1. Explicit `--workspace MANIFEST` → load the estate, scope by the file's repo.
/// 2. Explicit `--db DB` → single-repo blast (back-compat, no estate).
/// 3. Neither → auto-resolve via [`strata_index::resolve_context`]:
///    - Estate member → estate blast (cross-repo consumer surfaces).
///    - Standalone repo → single-repo blast.
///
/// The `<file>` is matched against the graph's repo-relative node paths, so
/// either a repo-relative path (`src/a.ts`) or an absolute path under the repo
/// root works (`path_matches` suffixes). A file with no indexed symbols is an
/// honest empty report, never a fake all-clear.
pub fn cmd_blast(
    db: Option<&Path>,
    repo: Option<&Path>,
    workspace: Option<&Path>,
    file: &str,
    format: BlastFormat,
) -> Result<String, CliError> {
    let report = if let Some(manifest) = workspace {
        // Explicit --workspace: estate blast; derive the repo name from the file.
        blast_estate(manifest, file, None)?
    } else if let Some(db) = db {
        // Explicit --db: single-repo (back-compat, no estate).
        let graph = load_existing_graph(db)?;
        let repo_root = repo
            .map(Path::to_path_buf)
            .or_else(|| repo_root_from_db(db));
        let rel = blast_rel_path(repo_root.as_deref(), file);
        blast_for_file(&graph, &rel)
    } else {
        // Auto-resolve: walk up from the file to find the .strata dir, then
        // decide single vs estate via the estate.toml marker.
        let root = repo_root_of_file(Path::new(file));
        match strata_index::resolve_context(&root) {
            IndexContext::Estate {
                manifest,
                repo,
                repo_root: _,
            } => blast_estate(&manifest, file, Some(&repo))?,
            IndexContext::Single { db } => {
                let graph = load_existing_graph(&db)?;
                let rel = blast_rel_path(Some(&root), file);
                blast_for_file(&graph, &rel)
            }
        }
    };
    Ok(match format {
        BlastFormat::Text => render_blast_text(&report),
        BlastFormat::Agent => render_blast_agent(&report),
    })
}

/// Estate-blast helper shared by the explicit-`--workspace` and auto-Estate
/// branches of [`cmd_blast`]: loads the estate graph and scopes the blast to the
/// repo that contains `file`.
///
/// `file` may be absolute (the pre-edit hook case) or relative. The repo is
/// identified by finding the manifest entry whose resolved path is a prefix of
/// the absolute `file` path. `known_repo` is the repo name already resolved by
/// `resolve_context` on the auto path — when supplied it is used directly and
/// only the repo root (for rel-path stripping) still needs to be derived.
///
/// # Never-confident-wrong guarantee
///
/// When no estate member matches the file (a member dir can't canonicalize, or
/// the file isn't under any listed member), we do **NOT** fall back to an
/// unscoped estate blast (which would surface other repos' symbols without
/// scoping). Instead we:
///
/// 1. Try a single-repo blast on the file's own `.strata/graph.duckdb` (walk up
///    from the file to find the local `.strata/` dir, load it, and blast scoped
///    to that single graph).
/// 2. If even that is unavailable, return an honest empty [`BlastReport`] with a
///    `note` explaining that the file is not under any indexed estate member.
fn blast_estate(
    manifest: &Path,
    file: &str,
    known_repo: Option<&str>,
) -> Result<BlastReport, CliError> {
    let (graph, _results) = load_workspace_graph(manifest)?;

    // Canonicalize the file path for prefix matching.
    let file_abs = std::fs::canonicalize(file)
        .or_else(|_| std::path::absolute(Path::new(file)))
        .unwrap_or_else(|_| Path::new(file).to_path_buf());

    let (repo_name, repo_root) = if let Some(name) = known_repo {
        // Auto-resolve fast path: we already know the repo name; still need the
        // root to derive the repo-relative path for blast_for_file_in_repo.
        let manifest_dir = manifest.parent().unwrap_or(Path::new("."));
        let (manifest_data, _) = load_manifest(manifest)?;
        let root = manifest_data
            .repos
            .iter()
            .find(|r| r.name == name)
            .and_then(|r| manifest_dir.join(&r.path).canonicalize().ok());
        (Some(name.to_string()), root)
    } else {
        // Explicit --workspace path: derive the repo from the file's absolute path.
        let manifest_dir = manifest.parent().unwrap_or(Path::new("."));
        let (manifest_data, _) = load_manifest(manifest)?;
        let mut found_name: Option<String> = None;
        let mut found_root: Option<PathBuf> = None;
        for entry in &manifest_data.repos {
            let candidate = manifest_dir.join(&entry.path);
            if let Ok(canon) = candidate.canonicalize() {
                if file_abs.starts_with(&canon) {
                    found_name = Some(entry.name.clone());
                    found_root = Some(canon);
                    break;
                }
            }
        }
        (found_name, found_root)
    };

    if repo_name.is_some() {
        // Happy path: file belongs to a known estate member.
        let rel = blast_rel_path(repo_root.as_deref(), file);
        return Ok(blast_for_file_in_repo(&graph, &rel, repo_name.as_deref()));
    }

    // No estate member matched — NEVER blast unscoped across the estate.
    // Fall back to a single-repo blast using the file's own local graph.
    let local_root = repo_root_of_file(Path::new(file));
    let local_db = local_root.join(".strata").join("graph.duckdb");
    if local_db.exists() {
        if let Ok(local_graph) = load_existing_graph(&local_db) {
            let rel = blast_rel_path(Some(&local_root), file);
            return Ok(blast_for_file(&local_graph, &rel));
        }
    }

    // Last resort: honest empty report — the file is not under any indexed
    // estate member and has no local graph either.
    Ok(BlastReport {
        file: file.to_string(),
        symbols: Vec::new(),
        affected: Vec::new(),
        risk: Risk {
            level: RiskLevel::Low,
            reasons: vec!["file is not under any indexed estate member".to_string()],
        },
        note: Some(
            "file is not under any indexed estate member and no local graph is available; \
             blast radius is unknown — index this repo or run `strata index --workspace` first"
                .to_string(),
        ),
    })
}

/// Walk up parent directories from `file` until a directory that contains a
/// `.strata/` subdirectory is found; return that directory as the repo root.
/// Falls back to the file's own parent directory (or `.`) if nothing is found.
fn repo_root_of_file(file: &Path) -> PathBuf {
    let start = if file.is_absolute() {
        file.parent().unwrap_or(Path::new("/")).to_path_buf()
    } else {
        // Make relative paths absolute against cwd so the walk makes sense.
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(file)
            .parent()
            .unwrap_or(Path::new("."))
            .to_path_buf()
    };

    let mut current = start.as_path();
    loop {
        if current.join(".strata").is_dir() {
            return current.to_path_buf();
        }
        match current.parent() {
            Some(p) => current = p,
            None => break,
        }
    }
    // Fallback: the file's parent (suffix match in blast_for_file still works).
    start
}

/// Normalise the blast `<file>` to the repo-relative form the graph stores.
///
/// `blast_for_file` matches via `path_matches` (exact / suffix either way), so an
/// already-relative path is returned as-is. When the file is given as an absolute
/// path *under* `repo_root` (the common hook case — Claude Code passes
/// `tool_input.file_path` absolute), strip the repo-root prefix so it matches the
/// stored repo-relative node paths. A path that is not under the repo root (or no
/// known root) is returned unchanged (the suffix match still has a chance).
fn blast_rel_path(repo_root: Option<&Path>, file: &str) -> String {
    let p = Path::new(file);
    if p.is_relative() {
        return file.to_string();
    }
    if let Some(root) = repo_root {
        if let Ok(stripped) = p.strip_prefix(root) {
            return stripped.to_string_lossy().into_owned();
        }
    }
    file.to_string()
}

/// The risk level as an uppercase label (shared by both blast renderers and the
/// change-report printer's rubric).
fn risk_level_label(level: RiskLevel) -> &'static str {
    match level {
        RiskLevel::Low => "LOW",
        RiskLevel::Medium => "MEDIUM",
        RiskLevel::High => "HIGH",
        RiskLevel::Critical => "CRITICAL",
    }
}

/// Render a [`BlastReport`] as the human-readable `strata blast` summary: the file,
/// the symbols it defines, the affected table, and the risk line. The empty-report
/// note (no indexed symbols) is surfaced explicitly.
fn render_blast_text(report: &BlastReport) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "Editing {} touches {} symbol(s); blast radius {} affected — {}\n",
        report.file,
        report.symbols.len(),
        report.affected.len(),
        risk_level_label(report.risk.level),
    ));
    if let Some(note) = &report.note {
        out.push_str(&format!("  note: {note}\n"));
        return out.trim_end().to_string();
    }
    // The file's own symbols (what an edit could change).
    if !report.symbols.is_empty() {
        out.push_str(&format!("  symbols ({}):\n", report.symbols.len()));
        for s in &report.symbols {
            out.push_str(&format!("    - {} [{}]\n", s.fqn, s.kind));
        }
    }
    // The aggregated blast radius.
    if report.affected.is_empty() {
        out.push_str("  affected: (nothing in the loaded graph depends on these symbols)\n");
    } else {
        out.push_str(&format!("  affected ({}):\n", report.affected.len()));
        out.push_str(&affected_header("    "));
        for a in &report.affected {
            out.push_str(&affected_row(
                "    ",
                a.depth,
                a.confidence,
                a.ambiguous,
                a.will_break,
                &a.name,
                &a.path,
            ));
        }
    }
    out.push_str(&format!(
        "Risk: {} — {}",
        risk_level_label(report.risk.level),
        report.risk.reasons.join("; ")
    ));
    out
}

/// The standing pre-edit instruction injected verbatim in every agent-format blast
/// block — the discipline the hook enforces whether or not the agent remembers it.
const BLAST_AGENT_INSTRUCTION: &str = "Before editing, run `impact`/`context` on the symbols above and report the blast radius. Treat confidence < 0.40 or `ambiguous` as UNKNOWN — never present it as certain. PAUSE for direction if risk is HIGH/CRITICAL, crosses a repo boundary, or touches contract surface.";

/// Render a [`BlastReport`] as the terse, token-lean block the PreToolUse hook
/// injects as `additionalContext`. It carries: the file, the symbols it defines,
/// the affected count + the top dependents, the risk + reasons, and the standing
/// pre-edit instruction. Token-lean by design — capped at the top dependents so a
/// large blast radius does not flood the agent's context.
fn render_blast_agent(report: &BlastReport) -> String {
    /// How many top dependents the agent block lists (token budget): the blast
    /// radius can be large, so the injected context shows the highest-confidence
    /// few and states the remainder as a count.
    const TOP_N: usize = 8;

    let mut out = String::new();
    out.push_str(&format!(
        "StrataGraph pre-edit blast — {}: {} symbol(s) defined here, {} dependent(s) in the blast radius. Risk {}.",
        report.file,
        report.symbols.len(),
        report.affected.len(),
        risk_level_label(report.risk.level),
    ));

    // No indexed symbols → say so honestly and still carry the instruction.
    if let Some(note) = &report.note {
        out.push_str(&format!("\n  ({note})"));
        out.push_str(&format!("\n{BLAST_AGENT_INSTRUCTION}"));
        return out;
    }

    // The symbols an edit could change (what to run impact/context on).
    if !report.symbols.is_empty() {
        let names: Vec<String> = report
            .symbols
            .iter()
            .map(|s| format!("{} [{}]", s.fqn, s.kind))
            .collect();
        out.push_str(&format!("\n  symbols: {}", names.join(", ")));
    }

    // The top dependents (highest confidence first; affected is already sorted).
    if report.affected.is_empty() {
        out.push_str(
            "\n  dependents: none in the loaded graph (NOT a guarantee — the index may be stale).",
        );
    } else {
        out.push_str("\n  top dependents (depth/conf/verdict):");
        for a in report.affected.iter().take(TOP_N) {
            out.push_str(&format!(
                "\n    - {} ({}) d={} conf={:.2}{} {}",
                a.name,
                a.path,
                a.depth,
                a.confidence,
                if a.ambiguous { " AMBIGUOUS" } else { "" },
                break_verdict(a.will_break),
            ));
        }
        if report.affected.len() > TOP_N {
            out.push_str(&format!(
                "\n    … and {} more",
                report.affected.len() - TOP_N
            ));
        }
    }

    // The risk reasons, then the standing instruction.
    out.push_str(&format!(
        "\n  risk reasons: {}",
        report.risk.reasons.join("; ")
    ));
    out.push_str(&format!("\n{BLAST_AGENT_INSTRUCTION}"));
    out
}

// ── rename ───────────────────────────────────────────────────────────────────────

/// `strata rename <old> <new> [--apply] [--force] [--uid <u>] [--db] [--repo]` —
/// graph-aware multi-file rename, **dry-run by default** (writes only with
/// `--apply`).
///
/// Edits land only in graph-implicated files (the def file + call/import-connected
/// files); a same-named identifier in an unrelated file is never touched. Each
/// edit is confidence-tagged. An ambiguous target prints the candidates (pick one
/// with `--uid`); a name collision refuses unless `--force`; a non-code target is
/// a clear queued error. The repo root is derived as for `detect-changes`.
#[allow(clippy::too_many_arguments)]
pub fn cmd_rename(
    db: &Path,
    repo: Option<&Path>,
    old: &str,
    new: &str,
    apply: bool,
    force: bool,
    uid: Option<&str>,
) -> Result<String, CliError> {
    let graph = load_existing_graph(db)?;
    let repo_root = repo
        .map(Path::to_path_buf)
        .or_else(|| repo_root_from_db(db))
        .unwrap_or_else(|| PathBuf::from("."));
    let opts = RenameOptions {
        apply,
        force,
        uid: uid.map(str::to_owned),
    };
    let outcome =
        rename(&graph, &repo_root, old, new, &opts).map_err(|e| CliError::Other(e.to_string()))?;
    Ok(render_rename_outcome(&outcome))
}

/// Render a [`strata_index::RenameOutcome`] as human-readable CLI output: a
/// candidate list (ambiguous), or the plan/applied edit set with the
/// implicated-file scope and a per-edit confidence column.
fn render_rename_outcome(outcome: &RenameOutcome) -> String {
    match outcome {
        RenameOutcome::Candidates { symbol, candidates } => {
            let mut out = format!(
                "ambiguous symbol {symbol}: {} code candidates — re-run with --uid <uid>:\n",
                candidates.len()
            );
            for c in candidates {
                out.push_str(&format!(
                    "  - {} [{}] ({})\n    {}\n",
                    c.name, c.kind, c.path, c.uid
                ));
            }
            out.trim_end().to_string()
        }
        RenameOutcome::Plan {
            old,
            new,
            applied,
            implicated_files,
            edits,
            reindex_recommended,
            ..
        } => {
            let mut out = String::new();
            let verb = if *applied {
                "Renamed"
            } else {
                "Rename (dry run):"
            };
            out.push_str(&format!("{verb} {old} → {new}\n"));
            out.push_str(&format!(
                "  {} implicated file(s): {}\n",
                implicated_files.len(),
                implicated_files.join(", ")
            ));
            if edits.is_empty() {
                out.push_str("  (no identifier tokens to rewrite)\n");
            } else {
                out.push_str(&format!("  {} edit(s):\n", edits.len()));
                out.push_str("    conf   line:col  file\n");
                for e in edits {
                    out.push_str(&format!(
                        "    {:>4.2}   {}:{}  {}\n",
                        e.confidence, e.line, e.col, e.file
                    ));
                }
            }
            if !*applied {
                out.push_str("  (dry run — re-run with --apply to write these edits)\n");
            } else if *reindex_recommended {
                out.push_str("  reindex recommended: run `strata index .` (the edit hook normally covers it)\n");
            }
            out.trim_end().to_string()
        }
    }
}

// ── shared symbol resolution ────────────────────────────────────────────────────

/// Resolve `symbol` to exactly one node, mapping ambiguity/absence to [`CliError`].
///
/// On ambiguity the error LISTS the candidates (uid / kind / name / path) plus a
/// `--uid` re-run hint — not a bare count — so an ambiguous symbol is a signpost,
/// never a dead-end. `--uid` callers should use [`resolve_one_or_uid`].
fn resolve_one(graph: &Graph, symbol: &str) -> Result<strata_core::Node, CliError> {
    match resolve_symbol(graph, symbol) {
        ResolveOutcome::None => Err(CliError::SymbolNotFound {
            symbol: symbol.to_string(),
        }),
        ResolveOutcome::One(node) => Ok(node),
        ResolveOutcome::Many(candidates) => Err(ambiguous_error(symbol, &candidates)),
    }
}

/// Resolve `symbol` honouring an optional `--uid` pin.
///
/// With `uid`, resolve straight from the graph ([`Graph::get_node`]); a uid that
/// is not in the graph is a clear [`CliError::SymbolNotFound`] — never a silent
/// fall-back to name resolution (which could pick the wrong node). Without `uid`,
/// behave exactly like [`resolve_one`] (single match, or the candidate listing).
fn resolve_one_or_uid(
    graph: &Graph,
    symbol: &str,
    uid: Option<&str>,
) -> Result<strata_core::Node, CliError> {
    match uid {
        Some(uid) => graph
            .get_node(&strata_core::Uid(uid.to_string()))
            .cloned()
            .ok_or_else(|| CliError::SymbolNotFound {
                symbol: uid.to_string(),
            }),
        None => resolve_one(graph, symbol),
    }
}

/// Build the ambiguous-symbol [`CliError`]: the candidate list (one per line as
/// `<uid>  [<kind>]  <name>  (<path>)`) followed by the actionable `--uid` hint.
/// Shared so `impact`/`explain`/`context` all render the same signpost.
fn ambiguous_error(symbol: &str, candidates: &[strata_core::Node]) -> CliError {
    let mut msg = format!(
        "ambiguous symbol {symbol}: {} candidates — pick one:\n",
        candidates.len()
    );
    for n in candidates {
        msg.push_str(&format!(
            "  {uid}  [{kind:?}]  {name}  ({path})\n",
            uid = n.uid,
            kind = n.kind,
            name = n.name,
            path = n.path,
        ));
    }
    msg.push_str("re-run with --uid <uid> (or a fully-qualified name) to disambiguate");
    CliError::Ambiguous { message: msg }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_path_uses_default_when_none() {
        assert_eq!(db_path(None), PathBuf::from(DEFAULT_DB));
    }

    #[test]
    fn db_path_uses_override_when_given() {
        let p = PathBuf::from("/tmp/custom.duckdb");
        assert_eq!(db_path(Some(&p)), p);
    }

    // ── repo_root_from_db: the canonical `<repo>/.strata/graph.duckdb` derivation ──

    #[test]
    fn repo_root_from_db_uses_grandparent_for_canonical_path() {
        let db = Path::new("/x/my-repo/.strata/graph.duckdb");
        assert_eq!(
            repo_root_from_db(db),
            Some(PathBuf::from("/x/my-repo")),
            "the repo root is the grandparent of a canonical .strata/graph.duckdb"
        );
    }

    #[test]
    fn repo_root_from_db_is_none_for_a_custom_db_path() {
        // A non-canonical DB location can't imply a repo root — the caller falls
        // back to --repo or the ctx-less "needs a repo root" error.
        assert_eq!(repo_root_from_db(Path::new("/tmp/custom.duckdb")), None);
        assert_eq!(
            repo_root_from_db(Path::new("/x/my-repo/other/graph.duckdb")),
            None,
            "graph.duckdb not inside a `.strata` dir is not canonical"
        );
    }

    // ── render_change_report: the human-readable detect-changes printer ──

    #[test]
    fn render_change_report_groups_planes_and_prints_risk() {
        use strata_index::{
            AffectedNode, ChangeKind, ChangeReport, ChangedSymbol, FileChange, Plane, Risk,
            RiskLevel,
        };
        let report = ChangeReport {
            scope: "working".into(),
            files: vec![FileChange::Modified {
                path: "src/a.ts".into(),
            }],
            symbols: vec![
                ChangedSymbol {
                    plane: Plane::Code,
                    change: ChangeKind::Modified,
                    key: "helper".into(),
                    file: "src/a.ts".into(),
                    contract_change: None,
                },
                ChangedSymbol {
                    plane: Plane::Contract,
                    change: ChangeKind::Removed,
                    key: "Query.getStats".into(),
                    file: "schema.graphql".into(),
                    contract_change: Some(strata_index::ContractChange::Breaking),
                },
            ],
            other_files: vec!["README.md".into()],
            affected: vec![AffectedNode {
                uid: "ts|app|src/a.ts|caller|()".into(),
                name: "caller".into(),
                kind: "Function".into(),
                path: "src/a.ts".into(),
                depth: 1,
                confidence: 0.9,
                ambiguous: false,
                will_break: true,
            }],
            risk: Risk {
                level: RiskLevel::Critical,
                reasons: vec!["touches contract surface: Query.getStats".into()],
            },
        };
        let out = render_change_report(&report);
        // Plane grouping: both a code and a contract section, each naming its symbol.
        assert!(out.contains("code symbols (1):") && out.contains("helper"));
        assert!(out.contains("contract symbols (1):") && out.contains("Query.getStats"));
        // The affected table and the changed file.
        assert!(out.contains("affected (1):") && out.contains("caller"));
        // The affected node carries the §15.6 verdict column (caller reaches the
        // change cleanly at 0.9 ≥ 0.40 ⇒ WILL BREAK).
        assert!(
            out.contains("WILL BREAK"),
            "the affected table must stamp the will-break verdict; got:\n{out}"
        );
        assert!(out.contains("M  src/a.ts"));
        // Other files listed, never claimed as symbols.
        assert!(out.contains("README.md"));
        // The risk line names the level and the reason.
        assert!(
            out.contains("Risk: CRITICAL — touches contract surface: Query.getStats"),
            "risk line must name level + reasons; got:\n{out}"
        );
    }

    // ── affected-node table helpers (the shared will-break verdict column) ──

    #[test]
    fn affected_row_labels_the_will_break_verdict() {
        // The §15.6 verdict in words, and one rendered row of each kind.
        assert_eq!(break_verdict(true), "WILL BREAK");
        assert_eq!(break_verdict(false), "may affect");

        let will = affected_row("  ", 1, 0.95, false, true, "caller", "a.ts");
        assert!(
            will.contains("WILL BREAK"),
            "will-break row carries it: {will:?}"
        );
        assert!(
            will.contains("caller (a.ts)"),
            "the name (path) is still present: {will:?}"
        );

        let may = affected_row("  ", 2, 0.30, true, false, "weak", "b.ts");
        assert!(
            may.contains("may affect"),
            "may-affect row says so: {may:?}"
        );
        assert!(
            !may.contains("WILL BREAK"),
            "a may-affect row is not labelled will-break: {may:?}"
        );
    }

    // ── blast renderers (Slice 20: the pre-edit blast radius printers) ──────────

    /// A populated BlastReport: file `src/a.ts` defines `target`, one dependent
    /// `caller` reached cleanly (WILL BREAK), risk MEDIUM.
    fn sample_blast_report() -> BlastReport {
        use strata_index::{AffectedNode, BlastSymbol, Risk, RiskLevel};
        BlastReport {
            file: "src/a.ts".into(),
            symbols: vec![BlastSymbol {
                fqn: "target".into(),
                name: "target".into(),
                kind: "Function".into(),
            }],
            affected: vec![AffectedNode {
                uid: "ts|app|src/b.ts|caller|()".into(),
                name: "caller".into(),
                kind: "Function".into(),
                path: "src/b.ts".into(),
                depth: 1,
                confidence: 0.9,
                ambiguous: false,
                will_break: true,
            }],
            risk: Risk {
                level: RiskLevel::Medium,
                reasons: vec!["1 affected".into()],
            },
            note: None,
        }
    }

    #[test]
    fn render_blast_text_summarizes_file_symbols_affected_and_risk() {
        let out = render_blast_text(&sample_blast_report());
        // The headline summary line: file, symbol count, affected count, risk.
        assert!(
            out.contains("Editing src/a.ts touches 1 symbol(s); blast radius 1 affected — MEDIUM"),
            "text headline must summarise file/symbols/affected/risk; got:\n{out}"
        );
        // The file's own symbol and the affected dependent table.
        assert!(out.contains("target [Function]"), "lists the file's symbol");
        assert!(
            out.contains("caller (src/b.ts)") && out.contains("WILL BREAK"),
            "lists the affected dependent with its verdict; got:\n{out}"
        );
        assert!(
            out.contains("Risk: MEDIUM — 1 affected"),
            "risk line present"
        );
    }

    #[test]
    fn render_blast_agent_is_terse_and_carries_the_standing_instruction() {
        let out = render_blast_agent(&sample_blast_report());
        // The agent block names the file, the symbols, the dependents, the risk,
        // and — critically — the standing pre-edit instruction the hook enforces.
        assert!(out.contains("StrataGraph pre-edit blast — src/a.ts"));
        assert!(out.contains("symbols: target [Function]"));
        assert!(
            out.contains("caller (src/b.ts)") && out.contains("d=1") && out.contains("WILL BREAK"),
            "agent block lists the top dependent with depth/verdict; got:\n{out}"
        );
        assert!(out.contains("Risk MEDIUM"));
        // The standing instruction — the discipline injected every edit.
        assert!(
            out.contains("Before editing, run `impact`/`context`")
                && out.contains("< 0.40")
                && out.contains("PAUSE for direction if risk is HIGH/CRITICAL"),
            "the agent block must carry the standing pre-edit instruction; got:\n{out}"
        );
    }

    #[test]
    fn render_blast_agent_empty_report_is_honest_and_still_instructs() {
        use strata_index::{Risk, RiskLevel};
        // No indexed symbols → the agent block must say so honestly (not a fake
        // all-clear) AND still carry the standing instruction.
        let report = BlastReport {
            file: "brand/new.ts".into(),
            symbols: vec![],
            affected: vec![],
            risk: Risk {
                level: RiskLevel::Low,
                reasons: vec!["no indexed symbols (new/unindexed file)".into()],
            },
            note: Some("no indexed symbols for this file — it is new, unindexed, or not a code/contract/infra file; this is not a guarantee that nothing depends on it".into()),
        };
        let out = render_blast_agent(&report);
        assert!(
            out.contains("no indexed symbols") && out.contains("not a guarantee"),
            "empty agent block must be honest, not a fake all-clear; got:\n{out}"
        );
        assert!(
            out.contains("Before editing, run `impact`/`context`"),
            "even an empty report must carry the standing instruction; got:\n{out}"
        );
    }

    #[test]
    fn render_blast_agent_caps_top_dependents() {
        use strata_index::{AffectedNode, BlastSymbol, Risk, RiskLevel};
        // A blast radius larger than the TOP_N cap must show the top few and state
        // the remainder as a count (token-lean injection).
        let affected: Vec<AffectedNode> = (0..12)
            .map(|i| AffectedNode {
                uid: format!("ts|app|f{i}.ts|x{i}|()"),
                name: format!("x{i}"),
                kind: "Function".into(),
                path: format!("f{i}.ts"),
                depth: 1,
                confidence: 0.9,
                ambiguous: false,
                will_break: true,
            })
            .collect();
        let report = BlastReport {
            file: "hub.ts".into(),
            symbols: vec![BlastSymbol {
                fqn: "hub".into(),
                name: "hub".into(),
                kind: "Function".into(),
            }],
            affected,
            risk: Risk {
                level: RiskLevel::Medium,
                reasons: vec!["12 affected".into()],
            },
            note: None,
        };
        let out = render_blast_agent(&report);
        assert!(
            out.contains("… and 4 more"),
            "12 dependents capped at 8 ⇒ '… and 4 more'; got:\n{out}"
        );
    }

    #[test]
    fn blast_format_parse_maps_agent_and_defaults_text() {
        assert_eq!(BlastFormat::parse("agent"), BlastFormat::Agent);
        assert_eq!(BlastFormat::parse("AGENT"), BlastFormat::Agent);
        assert_eq!(BlastFormat::parse("text"), BlastFormat::Text);
        assert_eq!(
            BlastFormat::parse("nonsense"),
            BlastFormat::Text,
            "unknown format falls back to text"
        );
    }

    #[test]
    fn blast_rel_path_strips_repo_root_for_absolute_paths() {
        // The hook passes an absolute file_path; blast_rel_path must reduce it to
        // the repo-relative form the graph stores.
        let root = Path::new("/x/my-repo");
        assert_eq!(
            blast_rel_path(Some(root), "/x/my-repo/src/a.ts"),
            "src/a.ts",
            "an absolute path under the repo root is made repo-relative"
        );
        // A relative path is returned unchanged.
        assert_eq!(blast_rel_path(Some(root), "src/a.ts"), "src/a.ts");
        // A path not under the repo root is returned unchanged (suffix match still
        // has a chance in blast_for_file).
        assert_eq!(
            blast_rel_path(Some(root), "/other/place/a.ts"),
            "/other/place/a.ts"
        );
        // No known root → unchanged.
        assert_eq!(blast_rel_path(None, "/abs/a.ts"), "/abs/a.ts");
    }

    // ── render_explanation: the evidence-chain printer (Track E1) ──────────────

    use strata_core::{
        Confidence, Edge, EdgeKind, Explanation, NodeKind, PathHop, Provenance, Span, Uid,
    };

    fn explain_node(uid: &str, name: &str) -> Node {
        Node {
            uid: Uid(uid.into()),
            kind: NodeKind::Function,
            name: name.into(),
            fqn: name.into(),
            path: format!("{name}.ts"),
            span: Span::default(),
            provenance: Provenance::Extracted,
            confidence: Confidence::new(1.0),
        }
    }

    /// A graph holding the named nodes a hop references, so `render_explanation`
    /// can resolve uids to the friendly names it prints.
    fn explain_graph(nodes: &[(&str, &str)]) -> Graph {
        let mut g = Graph::new();
        for (uid, name) in nodes {
            g.add_node(explain_node(uid, name));
        }
        g
    }

    #[test]
    fn render_explanation_prints_the_evidence_chain_with_verdict() {
        // The §14.2 brief example: getUser →PRODUCES→ Query.getUser ←CONSUMES←
        // client.ts, conf 0.95 then 0.90, clean ⇒ WILL BREAK.
        let g = explain_graph(&[
            ("u|getUser", "getUser"),
            ("u|field", "Query.getUser"),
            ("u|client", "client.ts"),
        ]);
        let target = explain_node("u|getUser", "getUser");
        let affected = explain_node("u|client", "client.ts");
        let explanation = Explanation {
            hops: vec![
                PathHop {
                    from: Uid("u|getUser".into()),
                    to: Uid("u|field".into()),
                    edge_kind: EdgeKind::Produces,
                    provenance: Provenance::Extracted,
                    confidence: 0.95,
                    running_confidence: 0.95,
                },
                PathHop {
                    from: Uid("u|field".into()),
                    to: Uid("u|client".into()),
                    edge_kind: EdgeKind::Consumes,
                    provenance: Provenance::Extracted,
                    confidence: 0.95,
                    running_confidence: 0.9025,
                },
            ],
            confidence: 0.9025,
            ambiguous: false,
        };
        let out = render_explanation(&g, &target, &affected, &explanation);
        // Header: the overall confidence + the will-break verdict.
        assert!(
            out.contains("Why getUser affects client.ts (conf 0.90, WILL BREAK)"),
            "header names target/affected/conf/verdict; got:\n{out}"
        );
        // Each hop renders the friendly node NAMES (not uids), the kind,
        // provenance, edge conf, and running conf.
        assert!(
            out.contains("getUser  —PRODUCES (Extracted 0.95)→  Query.getUser")
                && out.contains("running 0.95"),
            "first hop renders names/kind/provenance/running; got:\n{out}"
        );
        assert!(
            out.contains("Query.getUser  —CONSUMES (Extracted 0.95)→  client.ts")
                && out.contains("running 0.90"),
            "second hop renders the CONSUMES edge + decayed running; got:\n{out}"
        );
        // A clean chain is not marked AMBIGUOUS anywhere.
        assert!(
            !out.contains("AMBIGUOUS"),
            "a clean chain has no ambiguity mark"
        );
    }

    #[test]
    fn render_explanation_marks_ambiguous_hops_and_header() {
        // An ambiguous hop is flagged on the line AND the header says "via AMBIGUOUS"
        // and the verdict drops to "may affect" (ambiguous is never will-break).
        let g = explain_graph(&[("u|a", "a"), ("u|c", "c")]);
        let target = explain_node("u|a", "a");
        let affected = explain_node("u|c", "c");
        let explanation = Explanation {
            hops: vec![PathHop {
                from: Uid("u|a".into()),
                to: Uid("u|c".into()),
                edge_kind: EdgeKind::Calls,
                provenance: Provenance::Ambiguous,
                confidence: 0.3,
                running_confidence: 0.3,
            }],
            confidence: 0.3,
            ambiguous: true,
        };
        let out = render_explanation(&g, &target, &affected, &explanation);
        assert!(
            out.contains("may affect") && out.contains("via AMBIGUOUS"),
            "ambiguous chain header is may-affect + via AMBIGUOUS; got:\n{out}"
        );
        assert!(
            out.contains("[AMBIGUOUS]"),
            "the ambiguous hop line is explicitly marked; got:\n{out}"
        );
    }

    #[test]
    fn render_explanation_self_path_says_nothing_to_traverse() {
        let g = explain_graph(&[("u|a", "a")]);
        let n = explain_node("u|a", "a");
        let explanation = Explanation {
            hops: vec![],
            confidence: 1.0,
            ambiguous: false,
        };
        let out = render_explanation(&g, &n, &n, &explanation);
        assert!(
            out.contains("the target is the affected node"),
            "self path is the trivial case; got:\n{out}"
        );
    }

    #[test]
    fn explain_unreachable_line_is_honest() {
        let target = explain_node("u|t", "getUser");
        let affected = explain_node("u|x", "island.ts");
        let line = explain_unreachable_line(&target, &affected);
        assert_eq!(
            line,
            "island.ts is not in getUser's blast radius (nothing to explain)"
        );
    }

    #[test]
    fn missing_db_is_no_index_error() {
        let err = cmd_query(Path::new("/nonexistent/strata.duckdb"), "x").unwrap_err();
        assert!(matches!(err, CliError::NoIndex { .. }));
        assert_eq!(err.exit_code(), 1);
        assert!(err.to_string().contains("no index found"));
    }

    #[test]
    fn ambiguous_error_exits_with_code_2() {
        let err = CliError::Ambiguous {
            message: "two".into(),
        };
        assert_eq!(err.exit_code(), 2);
    }

    // ── impact/explain ambiguity: list candidates + --uid, never dead-end (B7) ──
    //
    // The user-found ambiguous-symbol dead-end: `strata impact <ambiguous>` printed just a
    // count. Now an ambiguous symbol lists the candidates (uid/kind/name/path) +
    // a `--uid` hint so the user can pick; `--uid` resolves the exact node.

    /// A graph with two same-named `dup` Function nodes (distinct uids) plus a
    /// `caller` that depends on the first — saved to a temp DuckDB so the `cmd_*`
    /// handlers (which load from disk) can be driven end-to-end. Returns the temp
    /// dir (keep it alive) and the db path.
    fn ambiguous_fixture_db() -> (TempDir, PathBuf) {
        let tmp = TempDir::new().expect("tempdir");
        let db = tmp.path().join("graph.duckdb");
        let mut g = Graph::new();
        let mk = |uid: &str, name: &str, path: &str| Node {
            uid: Uid(uid.into()),
            kind: NodeKind::Function,
            name: name.into(),
            fqn: name.into(),
            path: path.into(),
            span: Span::default(),
            provenance: Provenance::Extracted,
            confidence: Confidence::new(1.0),
        };
        g.add_node(mk("svc/a.ts|publish", "publish", "svc/a.ts"));
        g.add_node(mk("svc/b.ts|publish", "publish", "svc/b.ts"));
        g.add_node(mk("svc/c.ts|caller", "caller", "svc/c.ts"));
        // caller →Calls→ the a.ts publish, so impact on that pinned node is non-empty.
        g.add_edge(Edge {
            src: Uid("svc/c.ts|caller".into()),
            dst: Uid("svc/a.ts|publish".into()),
            kind: EdgeKind::Calls,
            provenance: Provenance::Inferred,
            confidence: Confidence::new(0.9),
        });
        let mut store = DuckGraphStore::open(&db).expect("open store");
        store.save_graph(&g).expect("save graph");
        (tmp, db)
    }

    #[test]
    fn cmd_impact_ambiguous_lists_candidates_with_uid_hint() {
        let (_tmp, db) = ambiguous_fixture_db();
        let err = cmd_impact(&db, "publish", 5, 0.0, true, true, None).unwrap_err();
        let msg = match &err {
            CliError::Ambiguous { message } => message.clone(),
            other => panic!("expected Ambiguous, got {other:?}"),
        };
        assert_eq!(err.exit_code(), 2, "ambiguity is exit code 2");
        // BOTH candidates listed, each with its uid, kind, name and path.
        assert!(msg.contains("svc/a.ts|publish"), "uid 1 listed:\n{msg}");
        assert!(msg.contains("svc/b.ts|publish"), "uid 2 listed:\n{msg}");
        assert!(
            msg.contains("(svc/a.ts)") && msg.contains("(svc/b.ts)"),
            "paths listed:\n{msg}"
        );
        assert!(msg.contains("[Function]"), "kind listed:\n{msg}");
        // The way forward — the actionable hint that turns the dead-end into a path.
        assert!(
            msg.contains("--uid"),
            "the --uid re-run hint is present:\n{msg}"
        );
    }

    #[test]
    fn cmd_impact_with_uid_resolves_the_exact_node() {
        let (_tmp, db) = ambiguous_fixture_db();
        let out = cmd_impact(&db, "publish", 5, 0.0, true, true, Some("svc/a.ts|publish")).unwrap();
        // The pinned a.ts publish has `caller` as a dependent — a real impact result.
        assert!(
            out.contains("Impact of publish (svc/a.ts)"),
            "pinned target header:\n{out}"
        );
        assert!(
            out.contains("caller"),
            "the pinned node's dependent is listed:\n{out}"
        );
    }

    #[test]
    fn cmd_impact_unknown_uid_is_symbol_not_found() {
        let (_tmp, db) = ambiguous_fixture_db();
        let err = cmd_impact(&db, "publish", 5, 0.0, true, true, Some("nope")).unwrap_err();
        assert!(
            matches!(err, CliError::SymbolNotFound { ref symbol } if symbol == "nope"),
            "an unknown --uid is a clear not-found, never a silent name fall-back: {err:?}"
        );
    }

    // ── impact member-dependent hint: never bare-say "nothing depends on this" ──
    //
    // `strata impact <Type>` on a member-bearing node prints the bare "(nothing
    // depends on this …)" even when the type's MEMBERS have dependents (deps hang
    // off member nodes, not the type). When members have dependents the CLI now
    // points the user at them; a truly-dead container keeps the honest bare line.

    /// A graph saved to a temp DuckDB: a `Widget` Class —Defines→ `render` Method;
    /// `caller` —Calls→ `render`. So `impact(Widget)` is zero-direct but `render`
    /// has a dependent. Plus a `Dead` Class —Defines→ `noop` Method with NO caller
    /// (a genuinely dead container). Plus a `Ghost` Table —HasColumn→ `Ghost.col`
    /// Column with NO FK referrer and NO Reads/Writes (a genuinely dead TABLE — the
    /// data-plane honesty case: `HasColumn` is also reverse-walked, so without
    /// excluding the container self-reach the column would falsely surface). Returns
    /// the temp dir (keep alive) and db path.
    fn member_dependent_fixture_db() -> (TempDir, PathBuf) {
        let tmp = TempDir::new().expect("tempdir");
        let db = tmp.path().join("graph.duckdb");
        let mut g = Graph::new();
        let mk = |uid: &str, name: &str, path: &str, kind: NodeKind| Node {
            uid: Uid(uid.into()),
            kind,
            name: name.into(),
            fqn: name.into(),
            path: path.into(),
            span: Span::default(),
            provenance: Provenance::Extracted,
            confidence: Confidence::new(1.0),
        };
        g.add_node(mk("w|Widget", "Widget", "widget.ts", NodeKind::Class));
        g.add_node(mk("w|render", "render", "widget.ts", NodeKind::Method));
        g.add_node(mk("w|caller", "caller", "app.ts", NodeKind::Function));
        g.add_node(mk("d|Dead", "Dead", "dead.ts", NodeKind::Class));
        g.add_node(mk("d|noop", "noop", "dead.ts", NodeKind::Method));
        g.add_node(mk("t|Ghost", "Ghost", "schema.sql", NodeKind::Table));
        g.add_node(mk(
            "t|Ghost.col",
            "Ghost.col",
            "schema.sql",
            NodeKind::Column,
        ));
        g.add_edge(Edge {
            src: Uid("w|Widget".into()),
            dst: Uid("w|render".into()),
            kind: EdgeKind::Defines,
            provenance: Provenance::Extracted,
            confidence: Confidence::new(0.95),
        });
        g.add_edge(Edge {
            src: Uid("w|caller".into()),
            dst: Uid("w|render".into()),
            kind: EdgeKind::Calls,
            provenance: Provenance::Inferred,
            confidence: Confidence::new(0.9),
        });
        g.add_edge(Edge {
            src: Uid("d|Dead".into()),
            dst: Uid("d|noop".into()),
            kind: EdgeKind::Defines,
            provenance: Provenance::Extracted,
            confidence: Confidence::new(0.95),
        });
        g.add_edge(Edge {
            src: Uid("t|Ghost".into()),
            dst: Uid("t|Ghost.col".into()),
            kind: EdgeKind::HasColumn,
            provenance: Provenance::Extracted,
            confidence: Confidence::new(0.95),
        });
        let mut store = DuckGraphStore::open(&db).expect("open store");
        store.save_graph(&g).expect("save graph");
        (tmp, db)
    }

    #[test]
    fn cmd_impact_member_bearing_target_hints_at_member_dependents() {
        let (_tmp, db) = member_dependent_fixture_db();
        let out = cmd_impact(&db, "Widget", 5, 0.0, true, true, None).unwrap();
        // The bare "nothing depends on this" line must NOT appear.
        assert!(
            !out.contains("nothing depends on this"),
            "a member-bearing target with member-deps must not bare-say nothing-depends:\n{out}"
        );
        // It states zero direct deps on the type ITSELF, the member-dep count, and
        // names the member with a runnable next step.
        assert!(
            out.contains("0 dependents on Widget itself"),
            "hint states zero direct deps on the type itself:\n{out}"
        );
        assert!(
            out.contains("1 of its members have dependents"),
            "hint states how many members have dependents:\n{out}"
        );
        assert!(
            out.contains("render"),
            "hint names the member that has dependents:\n{out}"
        );
        assert!(
            out.contains("strata impact"),
            "hint gives a runnable next command:\n{out}"
        );
    }

    #[test]
    fn cmd_impact_truly_dead_container_keeps_bare_message() {
        let (_tmp, db) = member_dependent_fixture_db();
        // `Dead` has a member `noop` but nobody calls it → genuinely dead → the
        // honest bare line stays (dead = dead).
        let out = cmd_impact(&db, "Dead", 5, 0.0, true, true, None).unwrap();
        assert!(
            out.contains("nothing depends on this"),
            "a genuinely dead container keeps the honest bare message:\n{out}"
        );
        assert!(
            !out.contains("of its members have dependents"),
            "a dead container must NOT show the member-dependent hint:\n{out}"
        );
    }

    #[test]
    fn cmd_impact_dead_table_keeps_bare_message() {
        let (_tmp, db) = member_dependent_fixture_db();
        // `Ghost` is a Table with one column that has NO FK referrer and NO
        // Reads/Writes → genuinely dead. `HasColumn` is also a reverse-walk edge, so
        // impact(Ghost.col) reaches its parent `Ghost`; the fix excludes that
        // outer-target self-reach, so the column does NOT falsely surface and the CLI
        // prints the honest bare line (data-plane "looks-alive-when-dead" guard).
        let out = cmd_impact(&db, "Ghost", 5, 0.0, true, true, None).unwrap();
        assert!(
            out.contains("nothing depends on this"),
            "a genuinely dead table keeps the honest bare message:\n{out}"
        );
        assert!(
            !out.contains("of its members have dependents"),
            "a dead table must NOT show the member-dependent hint:\n{out}"
        );
    }

    #[test]
    fn cmd_explain_ambiguous_target_lists_candidates() {
        let (_tmp, db) = ambiguous_fixture_db();
        // target `publish` is ambiguous; affected `caller` is unique.
        let err =
            cmd_explain(&db, "publish", "caller", 5, 0.0, true, true, None, None).unwrap_err();
        let msg = match &err {
            CliError::Ambiguous { message } => message.clone(),
            other => panic!("expected Ambiguous, got {other:?}"),
        };
        assert!(msg.contains("svc/a.ts|publish") && msg.contains("svc/b.ts|publish"));
        assert!(
            msg.contains("--uid"),
            "explain ambiguity carries the --uid hint:\n{msg}"
        );
    }

    #[test]
    fn cmd_explain_with_uid_resolves_target() {
        let (_tmp, db) = ambiguous_fixture_db();
        // Pin the a.ts publish; caller depends on it → a reachable chain.
        let out = cmd_explain(
            &db,
            "publish",
            "caller",
            5,
            0.0,
            true,
            true,
            Some("svc/a.ts|publish"),
            None,
        )
        .unwrap();
        assert!(
            out.contains("Why publish affects caller"),
            "pinned target produces the evidence chain header:\n{out}"
        );
    }

    /// A fixture where the AFFECTED end is ambiguous: two `caller` nodes, one of
    /// which (a.ts) calls the unique `publish` target — so `explain publish caller`
    /// is ambiguous on the affected end and `--affected-uid` pins it (F5).
    fn ambiguous_affected_fixture_db() -> (TempDir, PathBuf) {
        let tmp = TempDir::new().expect("tempdir");
        let db = tmp.path().join("graph.duckdb");
        let mut g = Graph::new();
        let mk = |uid: &str, name: &str, path: &str| Node {
            uid: Uid(uid.into()),
            kind: NodeKind::Function,
            name: name.into(),
            fqn: name.into(),
            path: path.into(),
            span: Span::default(),
            provenance: Provenance::Extracted,
            confidence: Confidence::new(1.0),
        };
        g.add_node(mk("svc/t.ts|publish", "publish", "svc/t.ts"));
        g.add_node(mk("svc/a.ts|caller", "caller", "svc/a.ts"));
        g.add_node(mk("svc/b.ts|caller", "caller", "svc/b.ts"));
        // The a.ts caller depends on publish → pinning that caller gives a chain.
        g.add_edge(Edge {
            src: Uid("svc/a.ts|caller".into()),
            dst: Uid("svc/t.ts|publish".into()),
            kind: EdgeKind::Calls,
            provenance: Provenance::Inferred,
            confidence: Confidence::new(0.9),
        });
        let mut store = DuckGraphStore::open(&db).expect("open store");
        store.save_graph(&g).expect("save graph");
        (tmp, db)
    }

    /// F5: an ambiguous AFFECTED end lists its candidates with the `--affected-uid`
    /// hint (not a dead-end), and pinning one with `--affected-uid` resolves it into
    /// the evidence chain — mirroring the MCP `explain` tool's `affected_uid`.
    #[test]
    fn cmd_explain_affected_uid_resolves_ambiguous_affected_end() {
        let (_tmp, db) = ambiguous_affected_fixture_db();

        // Without a pin, the ambiguous affected end is a signpost listing candidates.
        let err =
            cmd_explain(&db, "publish", "caller", 5, 0.0, true, true, None, None).unwrap_err();
        let msg = match &err {
            CliError::Ambiguous { message } => message.clone(),
            other => panic!("expected Ambiguous on the affected end, got {other:?}"),
        };
        assert!(
            msg.contains("svc/a.ts|caller") && msg.contains("svc/b.ts|caller"),
            "both affected candidates listed:\n{msg}"
        );

        // Pinning the a.ts caller with --affected-uid resolves it → the chain header.
        let out = cmd_explain(
            &db,
            "publish",
            "caller",
            5,
            0.0,
            true,
            true,
            None,
            Some("svc/a.ts|caller"),
        )
        .unwrap();
        assert!(
            out.contains("Why publish affects caller"),
            "pinned affected end produces the evidence chain header:\n{out}"
        );
    }

    // ── load_workspace_graph (the `strata mcp --workspace` core) ──────────────
    //
    // These mirror the desktop `open_workspace` tests' construction pattern
    // (`apps/strata-desktop/src-tauri/src/commands.rs`): index tiny fixture repos
    // into their own `.strata/graph.duckdb`, write a manifest, then load it.
    // `load_workspace_graph` is the testable seam the `mcp --workspace` route
    // serves; it returns the linked estate graph plus the per-repo statuses.

    use strata_index::{index_estate, index_repo};
    use strata_store::DuckGraphStore;
    use tempfile::TempDir;

    /// Index a tiny TS repo with one exported `fn_name` into
    /// `<root>/<name>/.strata/graph.duckdb`. The directory basename is `name`, so
    /// `index_repo` derives `name` as the repo_name — which matches the manifest
    /// repo `name`/`path` written by [`write_manifest`], exactly as a real
    /// `strata index <repo>` followed by an estate load would line up. Returns the
    /// repo directory.
    fn index_tiny_repo(root: &Path, name: &str, fn_name: &str) -> PathBuf {
        let repo = root.join(name);
        std::fs::create_dir_all(&repo).expect("mk repo dir");
        std::fs::write(
            repo.join("src.ts"),
            format!("export function {fn_name}() {{ return 1; }}\n"),
        )
        .expect("write ts");
        let strata_dir = repo.join(".strata");
        std::fs::create_dir_all(&strata_dir).expect("mk .strata");
        let mut store = DuckGraphStore::open(&strata_dir.join("graph.duckdb")).expect("open store");
        index_repo(&repo, &mut store).expect("index tiny repo");
        repo
    }

    /// Write a `strata.workspace.toml` listing `repos` (name == path) under `root`.
    fn write_manifest(root: &Path, name: &str, repos: &[&str]) -> PathBuf {
        let mut toml = format!("[workspace]\nname = \"{name}\"\n");
        for r in repos {
            toml.push_str(&format!("\n[[repos]]\nname = \"{r}\"\npath = \"{r}\"\n"));
        }
        let manifest = root.join("strata.workspace.toml");
        std::fs::write(&manifest, toml).expect("write manifest");
        manifest
    }

    #[test]
    fn load_workspace_graph_loads_cross_repo_estate() {
        // Two indexed repos + a manifest → the estate graph unions both, and the
        // per-repo statuses are all ok. (A plain code estate has no contract node;
        // the cross-repo *contract* node is pinned in the next test.)
        let tmp = TempDir::new().expect("tempdir");
        index_tiny_repo(tmp.path(), "repo-a", "alphaFn");
        index_tiny_repo(tmp.path(), "repo-b", "betaFn");
        let manifest = write_manifest(tmp.path(), "estate", &["repo-a", "repo-b"]);

        let (graph, statuses) = load_workspace_graph(&manifest).expect("estate loads");
        assert!(graph.node_count() > 0, "estate graph must have nodes");
        assert_eq!(statuses.len(), 2, "one status per manifest repo");
        assert!(statuses.iter().all(|s| s.ok), "both repos load ok");
        // Both repos' symbols are present (the union really happened).
        assert!(
            query(&graph, "alphaFn").iter().any(|n| n.name == "alphaFn"),
            "repo-a's symbol must be in the estate graph"
        );
        assert!(
            query(&graph, "betaFn").iter().any(|n| n.name == "betaFn"),
            "repo-b's symbol must be in the estate graph"
        );
    }

    #[test]
    fn load_workspace_graph_has_canonical_contract_node() {
        // The committed crossrepo_graphql estate links a producer in repo-schema
        // to a consumer in repo-app via a CANONICAL GraphqlField node — the
        // cross-repo contract node `load_workspace_graph` must surface (this is
        // why it uses `link_estate`, not a bare union).
        let tmp = TempDir::new().expect("tempdir");
        copy_dir_no_strata(&fixture("crossrepo_graphql"), tmp.path());
        let manifest = tmp.path().join("strata.workspace.toml");
        // Index the estate in place (each repo gets its own .strata/graph.duckdb).
        let parsed = WorkspaceManifest::parse_file(&manifest).expect("manifest parses");
        let _ = index_estate(&parsed, &manifest, ResolveMode::Off);

        let (graph, statuses) = load_workspace_graph(&manifest).expect("gql estate loads");
        assert!(
            statuses.iter().all(|s| s.ok),
            "both repos link ok: {statuses:?}"
        );
        // Exactly one canonical GraphqlField for Query.getUser — the cross-repo
        // contract node `link_estate` collapses the repos onto. Asserting on the
        // (kind, fqn) is robust to the canonical UID's internal encoding.
        let canonical: Vec<_> = graph
            .nodes()
            .filter(|n| n.kind == strata_core::NodeKind::GraphqlField && n.fqn == "Query.getUser")
            .collect();
        assert_eq!(
            canonical.len(),
            1,
            "exactly one canonical Query.getUser GraphqlField must be present in the estate graph"
        );
    }

    #[test]
    fn load_workspace_graph_one_broken_repo_still_loads_other() {
        // One good (indexed) repo + one bad (never-indexed) repo: the estate still
        // loads the good repo's graph, and the bad repo's status carries ok:false
        // with an error — never an aborted load.
        let tmp = TempDir::new().expect("tempdir");
        index_tiny_repo(tmp.path(), "good", "goodFn");
        // `bad` exists as a dir but was never indexed (no .strata/graph.duckdb).
        std::fs::create_dir_all(tmp.path().join("bad")).expect("mk bad");
        let manifest = write_manifest(tmp.path(), "estate", &["good", "bad"]);

        let (graph, statuses) = load_workspace_graph(&manifest).expect("partial estate loads");
        assert!(
            graph.node_count() > 0,
            "the good repo's graph must load even though `bad` failed"
        );
        let good = statuses
            .iter()
            .find(|s| s.name == "good")
            .expect("good status");
        assert!(good.ok, "good repo loads");
        let bad = statuses
            .iter()
            .find(|s| s.name == "bad")
            .expect("bad status");
        assert!(!bad.ok, "bad repo surfaced as not-ok");
        assert!(bad.error.is_some(), "bad repo carries an error message");
    }

    #[test]
    fn load_workspace_graph_all_repos_failed_is_error() {
        // Every repo in the manifest is un-indexed → an empty estate would be a
        // lie, so this must ERROR rather than serve a zero-node graph.
        let tmp = TempDir::new().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join("a")).expect("mk a");
        std::fs::create_dir_all(tmp.path().join("b")).expect("mk b");
        let manifest = write_manifest(tmp.path(), "estate", &["a", "b"]);

        let err = load_workspace_graph(&manifest).unwrap_err();
        assert!(
            matches!(err, CliError::Other(_)),
            "all-repos-failed must be an error, not an empty graph: {err:?}"
        );
    }

    #[test]
    fn load_workspace_graph_missing_manifest_is_error() {
        let err = load_workspace_graph(Path::new("/no/such/strata.workspace.toml")).unwrap_err();
        assert!(matches!(err, CliError::Other(_)), "missing manifest errors");
    }

    #[test]
    fn load_workspace_graph_invalid_manifest_is_error() {
        let tmp = TempDir::new().expect("tempdir");
        let manifest = tmp.path().join("strata.workspace.toml");
        std::fs::write(&manifest, "this = = not valid ===").expect("write");
        let err = load_workspace_graph(&manifest).unwrap_err();
        assert!(matches!(err, CliError::Other(_)), "invalid manifest errors");
    }

    /// Path to a strata-index test fixture directory (reused cross-crate, exactly
    /// as the desktop `commands.rs` tests reach the committed fixtures).
    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("strata-index")
            .join("tests")
            .join("fixtures")
            .join(name)
    }

    /// Copy a directory tree to `dst`, skipping any `.strata/` subdir (so the
    /// fixture is re-indexed fresh inside the tempdir).
    fn copy_dir_no_strata(src: &Path, dst: &Path) {
        std::fs::create_dir_all(dst).expect("mk dst");
        for entry in std::fs::read_dir(src).expect("read fixture dir") {
            let entry = entry.expect("dir entry");
            if entry.file_name() == ".strata" {
                continue;
            }
            let from = entry.path();
            let to = dst.join(entry.file_name());
            if entry.file_type().expect("file type").is_dir() {
                copy_dir_no_strata(&from, &to);
            } else {
                std::fs::copy(&from, &to).expect("copy file");
            }
        }
    }

    // ── resolve_mcp_launch: the pure auto-resolve seam (Task 8) ──────────────

    /// A directory with only a `graph.duckdb` (no estate marker) resolves to
    /// `McpLaunch::Single` with the directory as the repo root.
    #[test]
    fn resolve_mcp_launch_single_when_no_estate_marker() {
        let tmp = TempDir::new().expect("tempdir");
        // Create a .strata/graph.duckdb so the Single db path is canonical.
        let strata = tmp.path().join(".strata");
        std::fs::create_dir_all(&strata).expect("mk .strata");
        std::fs::write(strata.join("graph.duckdb"), b"").expect("create fake duckdb");

        let launch = resolve_mcp_launch(tmp.path());
        assert!(
            matches!(
                &launch,
                McpLaunch::Single { db, repo_root: Some(root) }
                if db == &tmp.path().join(".strata/graph.duckdb")
                    && root == tmp.path()
            ),
            "no estate marker → Single with repo_root = cwd; got: {launch:?}"
        );
    }

    /// Fix B: `resolve_mcp_launch` uses whatever directory it is given —
    /// simulating what happens when `--repo <path>` overrides `current_dir()`
    /// in the auto-resolve MCP dispatch. Passing the member repo directory must
    /// yield `Estate`; passing a fresh directory without a marker must yield
    /// `Single`. Both results depend purely on the path argument, not on the
    /// process cwd, so this pins that the dispatch wiring is correct.
    #[test]
    fn resolve_mcp_launch_repo_flag_overrides_cwd() {
        // Set up a crossrepo estate so one directory has a valid estate marker.
        let crossrepo_src = fixture("crossrepo");
        let tmp = TempDir::new().expect("tempdir");
        copy_dir_no_strata(&crossrepo_src, tmp.path());
        let manifest = tmp.path().join("strata.workspace.toml");
        cmd_index_workspace(&manifest, ResolveMode::Auto, false)
            .expect("estate indexing must succeed");

        let producer_dir = tmp.path().join("repo-producer");

        // Calling resolve_mcp_launch with the member dir (--repo <member>) → Estate.
        let launch_with_repo = resolve_mcp_launch(&producer_dir);
        assert!(
            matches!(&launch_with_repo, McpLaunch::Estate { .. }),
            "--repo pointing at estate member must resolve to Estate; got: {launch_with_repo:?}"
        );

        // Calling resolve_mcp_launch with an unrelated dir (no marker) → Single.
        let other = TempDir::new().expect("tempdir");
        std::fs::create_dir_all(other.path().join(".strata")).expect("mk .strata");
        let launch_other = resolve_mcp_launch(other.path());
        assert!(
            matches!(&launch_other, McpLaunch::Single { .. }),
            "--repo pointing at non-estate dir must resolve to Single; got: {launch_other:?}"
        );
    }

    #[test]
    fn resolve_mcp_cwd_precedence() {
        use std::path::{Path, PathBuf};
        // --repo wins over everything.
        assert_eq!(
            resolve_mcp_cwd(
                Some(Path::new("/r")),
                Some("/env"),
                Some(PathBuf::from("/cwd"))
            ),
            Some(PathBuf::from("/r"))
        );
        // CLAUDE_PROJECT_DIR wins over cwd when --repo is absent.
        assert_eq!(
            resolve_mcp_cwd(None, Some("/env"), Some(PathBuf::from("/cwd"))),
            Some(PathBuf::from("/env"))
        );
        // An empty CLAUDE_PROJECT_DIR is ignored; falls through to cwd.
        assert_eq!(
            resolve_mcp_cwd(None, Some(""), Some(PathBuf::from("/cwd"))),
            Some(PathBuf::from("/cwd"))
        );
        // Nothing resolvable returns None (callers choose their own fallback).
        assert_eq!(resolve_mcp_cwd(None, None, None), None);
    }

    /// A directory with a valid `.strata/estate.toml` marker (written by
    /// `cmd_index_workspace`) resolves to `McpLaunch::Estate` with the member
    /// directory as the repo root. This exercises the crossrepo fixture exactly
    /// as the brief specifies.
    #[test]
    fn resolve_mcp_launch_estate_when_marker_present() {
        // Copy the crossrepo fixture to a fresh tempdir.
        let crossrepo_src = fixture("crossrepo");
        let tmp = TempDir::new().expect("tempdir");
        copy_dir_no_strata(&crossrepo_src, tmp.path());

        let manifest = tmp.path().join("strata.workspace.toml");
        assert!(manifest.exists(), "crossrepo fixture must have a manifest");

        // Index the estate — this writes the .strata/estate.toml markers.
        cmd_index_workspace(&manifest, ResolveMode::Auto, false)
            .expect("estate indexing must succeed on the crossrepo fixture");

        let producer_dir = tmp.path().join("repo-producer");
        assert!(
            producer_dir.join(".strata/estate.toml").exists(),
            "estate.toml marker must be written into repo-producer"
        );

        // resolve_mcp_launch on the member repo must return Estate with its root.
        let launch = resolve_mcp_launch(&producer_dir);
        assert!(
            matches!(
                &launch,
                McpLaunch::Estate { repo_root, .. }
                if repo_root == &producer_dir
            ),
            "valid estate marker → Estate {{ repo_root = member_dir }}; got: {launch:?}"
        );
    }
}
