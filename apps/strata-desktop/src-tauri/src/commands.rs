//! The three backend operations behind the desktop commands, written as **thin,
//! testable functions** that never touch Tauri state or panic. The
//! `#[tauri::command]` wrappers in `lib.rs` are one-liners over these; the unit
//! tests here pin the contracts without launching a window.
//!
//! Design rules (from the slice spec):
//! * `tool` **delegates** to [`strata_mcp::call_tool`] — the GUI shares the one
//!   dispatch path with the MCP server and CLI, so it can never give a different
//!   answer.
//! * `open` handles both a DuckDB graph file and a `strata.workspace.toml`
//!   estate manifest; per-repo load outcomes are surfaced in [`OpenInfo`], and a
//!   bad repo is reported (`ok: false`) rather than crashing the load.
//! * Every fallible path returns `Err(String)` (the Tauri command error
//!   contract).

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;
use strata_core::Graph;
use strata_index::{index_estate, index_repo, link_estate, ResolveMode, WorkspaceManifest};
use strata_store::{DuckGraphStore, GraphStore};

/// Marker prefix on the "no StrataGraph index found" error returned by [`open_path`]
/// for a directory that has neither an index nor a workspace manifest.
///
/// The UI keys its **Index Now** affordance off this prefix *structurally* — it
/// never regex-matches the human folder name in the message body. The message
/// after the prefix stays human-readable and actionable (it still names the dir
/// and mentions `strata index`), so the CLI-style guidance is intact for anyone
/// reading the raw string.
pub const NO_INDEX_PREFIX: &str = "NO_INDEX::";

/// What was opened, remembered so [`reindex`](crate) can rebuild it the same way
/// the CLI would. Set by [`open_path`]:
/// * a directory that resolves to a `.strata/graph.duckdb` (with or without an
///   index already present) → [`OpenedSource::Repo`] of that directory;
/// * a `strata.workspace.toml` (directory-root or file) → [`OpenedSource::Estate`]
///   of the manifest path;
/// * any other file (a bare `.duckdb`) → [`OpenedSource::GraphFile`] of that file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenedSource {
    /// A project directory the CLI would `strata index <dir>` into
    /// `<dir>/.strata/graph.duckdb`.
    Repo(PathBuf),
    /// A bare DuckDB graph file opened directly (no inferable project root unless
    /// it is the conventional `<root>/.strata/graph.duckdb`).
    GraphFile(PathBuf),
    /// A `strata.workspace.toml` estate manifest.
    Estate(PathBuf),
}

impl OpenedSource {
    /// The repository working directory this source implies, when one is
    /// inferable: an opened project dir IS the root; a conventional
    /// `<root>/.strata/graph.duckdb` infers `<root>`; a bare graph file has no
    /// root, and an estate has MANY roots (the repo-scoped filesystem tools get
    /// an honest `None` and refuse with their clear message rather than guessing
    /// a member).
    pub fn repo_root(&self) -> Option<PathBuf> {
        match self {
            OpenedSource::Repo(dir) => Some(dir.clone()),
            OpenedSource::GraphFile(file) => repo_root_of_graph_file(file),
            OpenedSource::Estate(_) => None,
        }
    }
}

/// Per-repo load outcome inside an estate open (mirrors
/// `strata_index::RepoIndexResult`, trimmed to what the UI shows).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RepoStatus {
    pub name: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Summary of a freshly-loaded graph, returned by `open` and kept in state so the
/// UI header can show what is loaded.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct OpenInfo {
    /// Human-readable description of what was opened (the path, plus a `db` /
    /// `workspace` qualifier).
    pub source: String,
    /// The engine build id that produced this answer (strata_core::ENGINE_ID) —
    /// shown in the UI so a stale running app is identifiable at a glance.
    pub engine: String,
    pub nodes: usize,
    pub edges: usize,
    /// Per-repo statuses. For a single DuckDB file this is one synthetic entry
    /// (`ok: true`) so the UI can render uniformly; for an estate it is one per
    /// manifest repo, bad repos included with `ok: false`.
    pub repos: Vec<RepoStatus>,
}

/// Whether `path` is a workspace manifest (handled via the estate loader) rather
/// than a DuckDB graph file. Keyed on the `.toml` extension — the spec's two
/// inputs are a `.duckdb` path and a `.toml` manifest.
fn is_workspace_manifest(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("toml"))
        .unwrap_or(false)
}

/// Load a graph from either a DuckDB file or a workspace manifest, returning the
/// graph plus its [`OpenInfo`].
///
/// This is the pure core of the `open` command — no Tauri, no global state — so
/// it is exercised directly by the tests. Errors (missing file, unreadable
/// store, malformed manifest) come back as `Err(String)`. An estate with a
/// single broken repo still succeeds: the bad repo is recorded `ok: false` and
/// the other repos load.
///
/// ## Routing order
/// 1. **Directory** — resolved in priority order:
///    - `<dir>/strata.workspace.toml` exists → open as estate ([`OpenedSource::Estate`]).
///    - `<dir>/.strata/graph.duckdb` exists → open as duckdb, but the descriptor is
///      [`OpenedSource::Repo`] of the *directory* (so a reindex re-runs `index_repo`).
///    - neither → `Err` carrying the [`NO_INDEX_PREFIX`] marker (UI offers Index Now).
/// 2. **File** — `.toml` → estate; anything else → duckdb
///    ([`OpenedSource::GraphFile`], existing behaviour).
///
/// The third tuple element is the [`OpenedSource`] descriptor the caller stashes
/// in state so `reindex` can rebuild exactly what was opened.
pub fn open_path(path: &str) -> Result<(Graph, OpenInfo, OpenedSource), String> {
    let p = Path::new(path);

    if p.is_dir() {
        // 1a. Workspace manifest at the directory root.
        let workspace_toml = p.join("strata.workspace.toml");
        if workspace_toml.exists() {
            return open_workspace(&workspace_toml);
        }

        // 1b. StrataGraph index nested inside the directory. The descriptor is the
        // DIRECTORY (Repo), not the db file, so a reindex re-runs `index_repo`
        // over the project exactly as `strata index <dir>` would.
        let index_db = p.join(".strata").join("graph.duckdb");
        if index_db.exists() {
            let (graph, info) = open_duckdb(&index_db)?;
            return Ok((graph, info, OpenedSource::Repo(p.to_path_buf())));
        }

        // 1c. Neither found — return an actionable error, tagged with the
        // structured marker so the UI can offer Index Now without parsing the
        // dir name out of the prose.
        return Err(format!(
            "{NO_INDEX_PREFIX}No StrataGraph index found in {}. Run `strata index <dir>` to create one.",
            p.display()
        ));
    }

    // 2. File path: keep current behaviour.
    if is_workspace_manifest(p) {
        open_workspace(p)
    } else {
        let (graph, info) = open_duckdb(p)?;
        Ok((graph, info, OpenedSource::GraphFile(p.to_path_buf())))
    }
}

/// Open a single `.strata/graph.duckdb` (or any DuckDB graph file) into memory.
fn open_duckdb(path: &Path) -> Result<(Graph, OpenInfo), String> {
    // A non-existent path would otherwise have DuckDB create an empty database
    // and silently "succeed" with a zero-node graph — surface it as an error.
    if !path.exists() {
        return Err(format!("graph file not found: {}", path.display()));
    }
    let store = DuckGraphStore::open(path).map_err(|e| format!("open store: {e}"))?;
    let graph = store.load_graph().map_err(|e| format!("load graph: {e}"))?;
    let info = OpenInfo {
        engine: strata_core::ENGINE_ID.to_string(),
        source: format!("db: {}", path.display()),
        nodes: graph.node_count(),
        edges: graph.edge_count(),
        repos: vec![RepoStatus {
            name: path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("graph")
                .to_string(),
            ok: true,
            error: None,
        }],
    };
    Ok((graph, info))
}

/// Open a `strata.workspace.toml` estate: parse + validate the manifest, then
/// `link_estate` to union every repo's persisted graph (deduping contract
/// operations and adding cross-repo consumer links). Per-repo outcomes — bad
/// repos included — are surfaced in [`OpenInfo::repos`].
fn open_workspace(path: &Path) -> Result<(Graph, OpenInfo, OpenedSource), String> {
    let manifest =
        WorkspaceManifest::parse_file(path).map_err(|e| format!("parse manifest: {e}"))?;
    // Repo paths in the manifest are relative to the manifest's directory.
    let manifest_dir = path.parent().unwrap_or_else(|| Path::new("."));

    let (graph, _coverage, results) = link_estate(&manifest, manifest_dir);
    let repos: Vec<RepoStatus> = results
        .into_iter()
        .map(|r| RepoStatus {
            name: r.name,
            ok: r.ok,
            error: r.error,
        })
        .collect();

    let info = OpenInfo {
        engine: strata_core::ENGINE_ID.to_string(),
        source: format!(
            "workspace: {} ({})",
            manifest.workspace.name,
            path.display()
        ),
        nodes: graph.node_count(),
        edges: graph.edge_count(),
        repos,
    };
    Ok((graph, info, OpenedSource::Estate(path.to_path_buf())))
}

/// Rebuild the graph for a previously-opened [`OpenedSource`] **exactly the way
/// the CLI's `index` command does**, then reload it via [`open_path`] and return
/// the fresh `(Graph, OpenInfo, OpenedSource)`.
///
/// One behaviour, two front doors — this mirrors `strata-cli`'s
/// `cmd_index`/`cmd_index_workspace` (`crates/strata-cli/src/lib.rs`) precisely:
/// * **`Repo(dir)`** → `cmd_index(dir, dir/.strata/graph.duckdb)`: create the
///   `.strata/` parent, `DuckGraphStore::open(db)`, `index_repo(dir, &mut store)`
///   (the same `index_repo` the CLI calls, so `IndexOptions::default()` =
///   `ResolveMode::Auto`, no install). Then `open_path(dir)` reloads it (and the
///   descriptor stays `Repo(dir)`).
/// * **`Estate(manifest)`** → `cmd_index_workspace(manifest, ResolveMode::Auto)`:
///   `index_estate(&manifest, &manifest_dir, Auto)` re-indexes **each** repo and
///   *continues on per-repo failure* (R2) — a broken repo never aborts the batch;
///   the per-repo ok/err shows up in the reloaded [`OpenInfo::repos`]. Then
///   `open_path(manifest)` reloads (via `link_estate`, the same as a fresh open).
/// * **`GraphFile(f)`** → if `f` is a conventional `<root>/.strata/graph.duckdb`,
///   reindex it as `Repo(root)` (we *can* infer the project root); otherwise a
///   bare graph file elsewhere has no inferable root → an honest, actionable
///   `Err` telling the user to open the project folder instead.
///
/// This is a **pure** function — no Tauri state, no global mutex, no `spawn_*` —
/// so the tests drive it directly. The blocking CPU work lives here; the command
/// wrapper in `lib.rs` is responsible for running it off the UI thread and for
/// the single-flight guard + atomic state swap.
///
/// The schema-version parse cache needs **no** special handling on reindex: a
/// stale cache entry (mismatched content hash) is simply re-parsed by
/// `index_repo`'s incremental walk, and an unchanged file is reused — the
/// `incremental == full` invariant means the rebuilt graph is identical to a
/// from-scratch index regardless of cache state.
pub fn reindex_source(source: &OpenedSource) -> Result<(Graph, OpenInfo, OpenedSource), String> {
    match source {
        OpenedSource::Repo(dir) => {
            let db = dir.join(".strata").join("graph.duckdb");
            index_repo_to_db(dir, &db)?;
            open_path(
                dir.to_str()
                    .ok_or_else(|| format!("project path is not valid UTF-8: {}", dir.display()))?,
            )
        }
        OpenedSource::Estate(manifest) => {
            // Mirror `cmd_index_workspace`: parse the manifest, then
            // `index_estate(..., ResolveMode::Auto)` — which indexes each repo and
            // records (never propagates) per-repo failures. We deliberately ignore
            // the returned `EstateStats` here: the authoritative per-repo ok/err
            // the UI shows comes from the subsequent `open_path` reload (its
            // `link_estate` carries the same `RepoIndexResult`s).
            let parsed = WorkspaceManifest::parse_file(manifest)
                .map_err(|e| format!("parse manifest: {e}"))?;
            let _stats = index_estate(&parsed, manifest, ResolveMode::Auto);
            open_path(manifest.to_str().ok_or_else(|| {
                format!("manifest path is not valid UTF-8: {}", manifest.display())
            })?)
        }
        OpenedSource::GraphFile(file) => {
            // A bare graph file only has an inferable project root when it is the
            // conventional `<root>/.strata/graph.duckdb`. Then reindex as the repo
            // root; otherwise refuse with an actionable message.
            if let Some(root) = repo_root_of_graph_file(file) {
                let db = root.join(".strata").join("graph.duckdb");
                index_repo_to_db(&root, &db)?;
                open_path(root.to_str().ok_or_else(|| {
                    format!("project path is not valid UTF-8: {}", root.display())
                })?)
            } else {
                Err(format!(
                    "Can't infer the project root from a bare graph file ({}) — open the project folder instead.",
                    file.display()
                ))
            }
        }
    }
}

/// Index `repo` into `db`, mirroring the CLI's `cmd_index`: ensure the db's
/// parent (`.strata/`) exists, open the DuckDB store, and run the **same**
/// [`index_repo`] the CLI calls (so resolution defaults — `Auto`, no install —
/// are byte-for-byte identical). The store is dropped at the end of this scope,
/// so no lock is held on the duckdb file between this build and the reload that
/// follows.
fn index_repo_to_db(repo: &Path, db: &Path) -> Result<(), String> {
    if let Some(parent) = db.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
        }
    }
    let mut store = DuckGraphStore::open(db).map_err(|e| format!("open store: {e}"))?;
    index_repo(repo, &mut store).map_err(|e| format!("index repo: {e}"))?;
    Ok(())
}

/// If `file` is a conventional `<root>/.strata/graph.duckdb`, return `<root>`;
/// otherwise `None` (the project root cannot be inferred from a bare graph file).
///
/// The check is purely on the path *shape* (`…/.strata/graph.duckdb`), so it is
/// deterministic and does not touch the filesystem.
fn repo_root_of_graph_file(file: &Path) -> Option<PathBuf> {
    if file.file_name().and_then(|n| n.to_str()) != Some("graph.duckdb") {
        return None;
    }
    let strata_dir = file.parent()?;
    if strata_dir.file_name().and_then(|n| n.to_str()) != Some(".strata") {
        return None;
    }
    Some(strata_dir.parent()?.to_path_buf())
}

/// Delegate a tool call to the shared MCP dispatch.
///
/// This is the whole point of the `tool` command: the GUI reuses
/// [`strata_mcp::call_tool_ctx`] verbatim, so its answers are byte-for-byte the
/// MCP server's. `repo_root` (from [`OpenedSource::repo_root`]) rides in the
/// [`ToolCtx`](strata_mcp::ToolCtx) so the filesystem-touching tools
/// (`detect_changes`, `rename`) work when the opened source implies a project
/// root — previously the desktop always passed the ctx-less default and those
/// tools could only ever error. A `None` root keeps their clear refusal. The
/// only other adaptation is mapping [`strata_mcp::ToolError`] to the
/// `Err(String)` the Tauri boundary wants.
pub fn run_tool(
    graph: &Graph,
    repo_root: Option<PathBuf>,
    name: &str,
    args: &Value,
) -> Result<Value, String> {
    let ctx = strata_mcp::ToolCtx { repo_root };
    strata_mcp::call_tool_ctx(graph, &ctx, name, args).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;
    use strata_index::index_repo;
    use strata_store::DuckGraphStore;
    use tempfile::TempDir;

    /// Path to a strata-index test fixture (we reuse the committed
    /// `monolith_graphql` repo so the desktop `open` test indexes a real graph,
    /// mirroring `strata-index/tests/graphql_linking.rs`).
    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("..")
            .join("crates")
            .join("strata-index")
            .join("tests")
            .join("fixtures")
            .join(name)
    }

    #[test]
    fn opened_source_repo_root_covers_all_variants() {
        assert_eq!(
            OpenedSource::Repo(PathBuf::from("/w/proj")).repo_root(),
            Some(PathBuf::from("/w/proj")),
            "an opened project dir IS the repo root"
        );
        assert_eq!(
            OpenedSource::GraphFile(PathBuf::from("/w/proj/.strata/graph.duckdb")).repo_root(),
            Some(PathBuf::from("/w/proj")),
            "a conventional graph file infers its project root"
        );
        assert_eq!(
            OpenedSource::GraphFile(PathBuf::from("/tmp/some.duckdb")).repo_root(),
            None,
            "a bare graph file has no inferable root — honest None"
        );
        assert_eq!(
            OpenedSource::Estate(PathBuf::from("/w/est/strata.workspace.toml")).repo_root(),
            None,
            "an estate has MANY roots; the repo-scoped fs tools get an honest None"
        );
    }

    #[test]
    fn run_tool_threads_repo_root_to_filesystem_tools() {
        let (_tmp, db) = indexed_monolith();
        let (graph, _info, _src) = open_path(db.to_str().expect("utf8 path")).expect("open");

        // Without a root the fs-touching tools refuse with the clear message.
        let err = run_tool(&graph, None, "detect_changes", &json!({}))
            .expect_err("detect_changes without a root must error");
        assert!(
            err.contains("needs a repo root"),
            "ctx-less call keeps the clear refusal, got: {err}"
        );

        // With a root the ctx REACHES the tool: it proceeds past the root check
        // and fails on git instead (the tempdir is not a repository) — proving
        // the desktop no longer drops the root on the floor.
        let non_git = TempDir::new().expect("tempdir");
        let err = run_tool(
            &graph,
            Some(non_git.path().to_path_buf()),
            "detect_changes",
            &json!({}),
        )
        .expect_err("detect_changes in a non-git dir must error");
        assert!(
            !err.contains("needs a repo root"),
            "the root was threaded through, got: {err}"
        );
    }

    /// Index the `monolith_graphql` fixture into a fresh DuckDB store inside a
    /// tempdir and return `(tempdir, db_path)`. The tempdir must outlive the db.
    fn indexed_monolith() -> (TempDir, PathBuf) {
        let tmp = TempDir::new().expect("tempdir");
        let db_path = tmp.path().join("graph.duckdb");
        let mut store = DuckGraphStore::open(&db_path).expect("open store");
        let repo = fixture("monolith_graphql");
        assert!(repo.exists(), "fixture must exist at {}", repo.display());
        let stats = index_repo(&repo, &mut store).expect("index fixture");
        assert!(stats.nodes > 0, "fixture indexed to a non-empty graph");
        (tmp, db_path)
    }

    #[test]
    fn open_duckdb_loads_indexed_fixture() {
        let (_tmp, db_path) = indexed_monolith();
        let (graph, info, src) = open_path(db_path.to_str().unwrap()).expect("open db");
        assert!(graph.node_count() > 0);
        assert_eq!(info.nodes, graph.node_count());
        assert_eq!(info.edges, graph.edge_count());
        assert!(info.source.starts_with("db:"));
        assert_eq!(info.repos.len(), 1);
        assert!(info.repos[0].ok);
        // A bare `.duckdb` file (not under a `.strata/` named exactly that) opens
        // as a GraphFile descriptor.
        assert_eq!(src, OpenedSource::GraphFile(db_path.clone()));
    }

    #[test]
    fn open_missing_db_is_error_not_empty_graph() {
        let err = open_path("/no/such/graph.duckdb").unwrap_err();
        assert!(err.contains("not found"), "missing db must error: {err}");
    }

    #[test]
    fn open_then_tool_roundtrips_query_context_impact() {
        let (_tmp, db_path) = indexed_monolith();
        let (graph, _info, _src) = open_path(db_path.to_str().unwrap()).expect("open db");

        // query: the fixture defines a `getUser` resolver handler — find it.
        let q = run_tool(&graph, None, "query", &json!({ "text": "getUser" })).expect("query ok");
        let matches = q["matches"].as_array().expect("matches array");
        assert!(
            matches
                .iter()
                .any(|m| m["name"].as_str() == Some("getUser")),
            "query(getUser) must surface the getUser handler: {matches:?}"
        );

        // context: the same symbol resolves to a node with the context buckets.
        let ctx = run_tool(&graph, None, "context", &json!({ "symbol": "getUser" }));
        // `getUser` may be ambiguous (handler + maybe others) — either a node
        // payload or an ambiguous-candidates payload is a valid round-trip; what
        // matters is it does not error and shares call_tool's shape.
        let ctx = ctx.expect("context ok");
        assert!(
            ctx.get("node").is_some() || ctx.get("ambiguous").is_some(),
            "context must return either a node or an ambiguous payload: {ctx}"
        );

        // impact: the GraphqlField `Query.getUser` has dependents (its consumer +
        // producer) — impact on it must not error and must carry an `affected`
        // array (the shared call_tool contract).
        let imp = run_tool(
            &graph,
            None,
            "impact",
            &json!({ "symbol": "Query.getUser" }),
        )
        .expect("impact ok");
        assert!(
            imp["affected"].is_array(),
            "impact must return an affected array: {imp}"
        );
    }

    #[test]
    fn tool_error_becomes_err_string() {
        let (_tmp, db_path) = indexed_monolith();
        let (graph, _info, _src) = open_path(db_path.to_str().unwrap()).expect("open db");
        // Unknown symbol → NotFound → Err(String), never a panic.
        let err = run_tool(
            &graph,
            None,
            "impact",
            &json!({ "symbol": "definitely_absent_zzz" }),
        )
        .unwrap_err();
        assert!(
            err.to_lowercase().contains("not found"),
            "unknown symbol maps to a not-found error string: {err}"
        );
        // Unknown tool name → BadArgs → Err(String).
        let err2 = run_tool(&graph, None, "frobnicate", &json!({})).unwrap_err();
        assert!(!err2.is_empty(), "unknown tool errors as a string");
    }

    // ── Estate open: a manifest with one good repo and one bad repo. ──
    //
    // The good repo is the indexed `monolith_graphql`; the bad repo points at a
    // path with no `.strata/graph.duckdb`. `open` must SUCCEED, loading the good
    // repo's graph and surfacing the bad repo as `ok: false` — never a crash.

    #[test]
    fn open_workspace_surfaces_bad_repo_without_crashing() {
        let tmp = TempDir::new().expect("tempdir");

        // Good repo: copy the fixture into the workspace and index it in place so
        // its `.strata/graph.duckdb` exists where the manifest points.
        let good_repo = tmp.path().join("good");
        copy_dir(&fixture("monolith_graphql"), &good_repo).expect("copy fixture");
        let strata_dir = good_repo.join(".strata");
        std::fs::create_dir_all(&strata_dir).expect("mk .strata");
        let mut store =
            DuckGraphStore::open(&strata_dir.join("graph.duckdb")).expect("open good store");
        index_repo(&good_repo, &mut store).expect("index good repo");

        // Bad repo: a directory that exists but was never indexed (no store).
        let bad_repo = tmp.path().join("bad");
        std::fs::create_dir_all(&bad_repo).expect("mk bad repo");

        // Manifest listing both.
        let manifest = tmp.path().join("strata.workspace.toml");
        std::fs::write(
            &manifest,
            r#"
[workspace]
name = "estate"

[[repos]]
name = "good"
path = "good"

[[repos]]
name = "bad"
path = "bad"
"#,
        )
        .expect("write manifest");

        let (graph, info, src) = open_path(manifest.to_str().unwrap()).expect("open workspace");
        assert!(
            graph.node_count() > 0,
            "the good repo's graph must load even though `bad` failed"
        );
        assert!(info.source.starts_with("workspace:"));
        assert_eq!(src, OpenedSource::Estate(manifest.clone()));

        let good = info
            .repos
            .iter()
            .find(|r| r.name == "good")
            .expect("good repo status");
        assert!(good.ok, "good repo loads");

        let bad = info
            .repos
            .iter()
            .find(|r| r.name == "bad")
            .expect("bad repo status");
        assert!(!bad.ok, "bad repo surfaced as not-ok");
        assert!(bad.error.is_some(), "bad repo carries an error message");
    }

    // ── Directory routing: open_path on a directory. ──────────────────────────
    //
    // Three cases per the plan:
    //   1. dir containing .strata/graph.duckdb → loads the graph (nodes > 0).
    //   2. dir containing strata.workspace.toml → opens the estate.
    //   3. dir with neither → Err whose message contains the dir path and
    //      "strata index" (the actionable text).

    #[test]
    fn open_path_dir_with_index_loads_graph() {
        let tmp = TempDir::new().expect("tempdir");
        // Mirror the indexed_monolith pattern but place the db at
        // <tmp>/.strata/graph.duckdb, which is the standard location
        // open_path resolves when given a project directory.
        let strata_dir = tmp.path().join(".strata");
        std::fs::create_dir_all(&strata_dir).expect("mk .strata");
        let db_path = strata_dir.join("graph.duckdb");
        let mut store = DuckGraphStore::open(&db_path).expect("open store");
        let repo = fixture("monolith_graphql");
        assert!(repo.exists(), "fixture must exist at {}", repo.display());
        let stats = index_repo(&repo, &mut store).expect("index fixture");
        assert!(stats.nodes > 0, "fixture must have nodes");
        drop(store);

        // Pass the PROJECT DIRECTORY (not the db path).
        let dir = tmp.path().to_str().unwrap().to_string();
        let (graph, info, src) = open_path(&dir).expect("open dir with .strata/graph.duckdb");
        assert!(graph.node_count() > 0, "graph must have nodes");
        assert_eq!(info.nodes, graph.node_count());
        assert!(
            info.source.contains(".strata") || info.source.contains("db:"),
            "source should reference the resolved db: {}",
            info.source
        );
        // The descriptor is the DIRECTORY (Repo), not the db file, so a reindex
        // re-runs index_repo over the project.
        assert_eq!(src, OpenedSource::Repo(PathBuf::from(&dir)));
    }

    #[test]
    fn open_path_dir_with_workspace_opens_estate() {
        let tmp = TempDir::new().expect("tempdir");

        // Good repo: copy the fixture and index it in place.
        let good_repo = tmp.path().join("good");
        copy_dir(&fixture("monolith_graphql"), &good_repo).expect("copy fixture");
        let strata_dir = good_repo.join(".strata");
        std::fs::create_dir_all(&strata_dir).expect("mk .strata");
        let mut store =
            DuckGraphStore::open(&strata_dir.join("graph.duckdb")).expect("open good store");
        index_repo(&good_repo, &mut store).expect("index good repo");
        drop(store);

        // Write a strata.workspace.toml in the project root (next to the good repo).
        let manifest = tmp.path().join("strata.workspace.toml");
        std::fs::write(
            &manifest,
            r#"
[workspace]
name = "dir-estate"

[[repos]]
name = "good"
path = "good"
"#,
        )
        .expect("write manifest");

        // Pass the DIRECTORY that contains strata.workspace.toml.
        let dir = tmp.path().to_str().unwrap().to_string();
        let (graph, info, src) = open_path(&dir).expect("open dir with strata.workspace.toml");
        assert!(
            graph.node_count() > 0,
            "estate must load the good repo's graph"
        );
        assert!(
            info.source.starts_with("workspace:"),
            "source must say workspace: {}",
            info.source
        );
        // Opening a dir whose manifest lives at <dir>/strata.workspace.toml yields
        // an Estate descriptor pointing at that manifest file.
        assert_eq!(src, OpenedSource::Estate(manifest.clone()));
    }

    #[test]
    fn open_path_dir_with_neither_is_actionable_error() {
        let tmp = TempDir::new().expect("tempdir");
        // The tempdir is empty — no workspace.toml, no .strata/graph.duckdb.
        let dir = tmp.path().to_str().unwrap().to_string();
        let err = open_path(&dir).unwrap_err();
        // Structured marker so the UI can offer Index Now without regex-matching
        // the folder name out of the prose.
        assert!(
            err.starts_with(NO_INDEX_PREFIX),
            "no-index error must carry the structured marker prefix: {err}"
        );
        assert!(err.contains(&dir), "error must name the dir: {err}");
        assert!(
            err.to_lowercase().contains("strata index"),
            "error must mention 'strata index' for actionability: {err}"
        );
    }

    #[test]
    fn open_malformed_manifest_is_error() {
        let tmp = TempDir::new().expect("tempdir");
        let manifest = tmp.path().join("strata.workspace.toml");
        // Missing the required `[workspace] name` → parse/validate failure.
        std::fs::write(&manifest, "this = is = not valid toml ===").expect("write");
        let err = open_path(manifest.to_str().unwrap()).unwrap_err();
        assert!(
            err.contains("parse manifest"),
            "malformed manifest errors: {err}"
        );
    }

    // ════════════════════════════════════════════════════════════════════════
    // reindex tests (the plain `reindex_source` fn — no GUI, no Tauri state).
    // The async command wrapper + single-flight guard are tested in `lib.rs`.
    // ════════════════════════════════════════════════════════════════════════

    /// Write a tiny TS file with one exported function `name` into `dir`.
    fn write_ts_fn(dir: &Path, file: &str, name: &str) {
        std::fs::write(
            dir.join(file),
            format!("export function {name}() {{ return 1; }}\n"),
        )
        .expect("write ts file");
    }

    /// Whether `query` over a freshly-loaded graph at `db`/`source` finds a node
    /// named `name` (the user-visible "the new symbol is present" check).
    fn query_finds(graph: &Graph, name: &str) -> bool {
        let res = run_tool(graph, None, "query", &json!({ "text": name })).expect("query ok");
        res["matches"]
            .as_array()
            .map(|ms| ms.iter().any(|m| m["name"].as_str() == Some(name)))
            .unwrap_or(false)
    }

    // ── Group 1: reindex a Repo picks up a newly-added source file. ──
    //
    // Index a fresh tempdir repo, open it (Repo descriptor), add a NEW source
    // file, reindex → the new symbol appears (node count grows AND query finds
    // it). This is the core "run a reindex from the app" payoff.
    #[test]
    fn reindex_repo_picks_up_new_symbol() {
        let tmp = TempDir::new().expect("tempdir");
        let repo = tmp.path();
        write_ts_fn(repo, "alpha.ts", "alphaFn");

        // First index via the same path the CLI uses, then open the DIRECTORY so
        // the descriptor is Repo(dir).
        let db = repo.join(".strata").join("graph.duckdb");
        index_repo_to_db(repo, &db).expect("initial index");
        let (graph_before, info_before, src) =
            open_path(repo.to_str().unwrap()).expect("open repo dir");
        assert_eq!(src, OpenedSource::Repo(repo.to_path_buf()));
        assert!(
            query_finds(&graph_before, "alphaFn"),
            "alphaFn present pre-add"
        );
        assert!(
            !query_finds(&graph_before, "betaFn"),
            "betaFn absent before it is written"
        );
        let nodes_before = info_before.nodes;

        // Add a brand-new source file with a new symbol, then reindex via the
        // remembered descriptor.
        write_ts_fn(repo, "beta.ts", "betaFn");
        let (graph_after, info_after, src_after) =
            reindex_source(&src).expect("reindex repo after add");

        // The descriptor is stable across a reindex.
        assert_eq!(src_after, OpenedSource::Repo(repo.to_path_buf()));
        // New symbol is present: node count grew AND query finds it.
        assert!(
            info_after.nodes > nodes_before,
            "node count must grow after adding a file: {} !> {}",
            info_after.nodes,
            nodes_before
        );
        assert!(
            query_finds(&graph_after, "betaFn"),
            "the newly-added betaFn symbol must be present after reindex"
        );
        // And the original symbol survives the rebuild.
        assert!(
            query_finds(&graph_after, "alphaFn"),
            "alphaFn survives reindex"
        );
    }

    // ── Group 2: estate reindex continues past a broken repo. ──
    //
    // A 2-repo manifest; after the first index one repo is made un-indexable
    // (its path is replaced by a *file* so `index_estate` records a failure for
    // it). Reindex → the good repo still rebuilds and the reloaded `repos[]`
    // carries ok:false for the broken repo (never an aborted batch).
    #[test]
    fn reindex_estate_continues_past_broken_repo() {
        let tmp = TempDir::new().expect("tempdir");

        // Good repo: a real TS source dir.
        let good = tmp.path().join("good");
        std::fs::create_dir_all(&good).expect("mk good");
        write_ts_fn(&good, "g.ts", "goodFn");

        // Bad repo: starts as a real dir so the FIRST open succeeds for both …
        let bad = tmp.path().join("bad");
        std::fs::create_dir_all(&bad).expect("mk bad");
        write_ts_fn(&bad, "b.ts", "badFn");

        let manifest = tmp.path().join("strata.workspace.toml");
        std::fs::write(
            &manifest,
            r#"
[workspace]
name = "estate"

[[repos]]
name = "good"
path = "good"

[[repos]]
name = "bad"
path = "bad"
"#,
        )
        .expect("write manifest");

        // Index the estate once (both repos ok), then open it.
        let parsed = WorkspaceManifest::parse_file(&manifest).expect("parse");
        let _ = index_estate(&parsed, &manifest, ResolveMode::Auto);
        let (_g, info0, src) = open_path(manifest.to_str().unwrap()).expect("open estate");
        assert_eq!(src, OpenedSource::Estate(manifest.clone()));
        assert!(
            info0.repos.iter().all(|r| r.ok),
            "both repos load on the first open"
        );

        // Break the `bad` repo: replace its directory with a regular FILE so
        // `index_estate` records a failure (repo path is not a directory) and the
        // reload's `link_estate` reports ok:false — but `good` is untouched.
        std::fs::remove_dir_all(&bad).expect("rm bad dir");
        std::fs::write(&bad, "not a directory").expect("write bad as file");
        // Add a new symbol to the GOOD repo so we can prove it still reindexed.
        write_ts_fn(&good, "g2.ts", "goodFn2");

        let (graph, info, _src) = reindex_source(&src).expect("estate reindex must not abort");

        let good_status = info
            .repos
            .iter()
            .find(|r| r.name == "good")
            .expect("good repo status present");
        assert!(good_status.ok, "good repo still reindexes: {good_status:?}");
        let bad_status = info
            .repos
            .iter()
            .find(|r| r.name == "bad")
            .expect("bad repo status present");
        assert!(
            !bad_status.ok,
            "broken repo surfaced as ok:false, not an aborted batch: {bad_status:?}"
        );
        assert!(
            bad_status.error.is_some(),
            "broken repo carries an error message"
        );
        // The good repo's NEW symbol is present → it genuinely re-indexed.
        assert!(
            query_finds(&graph, "goodFn2"),
            "the good repo's freshly-added symbol must be present after estate reindex"
        );
    }

    // ── Group 3: graph-file source routing. ──
    //
    // (a) A GraphFile whose path is the conventional <root>/.strata/graph.duckdb
    //     reindexes as the repo root (picks up a new symbol).
    // (b) A bare graph file ELSEWHERE has no inferable root → an actionable Err
    //     that names the file and says to open the project folder.
    #[test]
    fn reindex_graph_file_under_dot_strata_behaves_as_repo() {
        let tmp = TempDir::new().expect("tempdir");
        let repo = tmp.path();
        write_ts_fn(repo, "alpha.ts", "alphaFn");
        let db = repo.join(".strata").join("graph.duckdb");
        index_repo_to_db(repo, &db).expect("initial index");

        // Open the DB FILE directly → GraphFile descriptor pointing at the
        // conventional <root>/.strata/graph.duckdb.
        let (_g, _i, src) = open_path(db.to_str().unwrap()).expect("open db file");
        assert_eq!(src, OpenedSource::GraphFile(db.clone()));

        // Add a new symbol and reindex via the graph-file descriptor: it must
        // infer the repo root and rebuild like a Repo reindex.
        write_ts_fn(repo, "beta.ts", "betaFn");
        let (graph, _info, src_after) =
            reindex_source(&src).expect("graph-file-under-.strata reindexes as repo");
        // Reload routes a directory back to a Repo descriptor (open_path(dir)).
        assert_eq!(src_after, OpenedSource::Repo(repo.to_path_buf()));
        assert!(
            query_finds(&graph, "betaFn"),
            "reindex via the .strata graph file must pick up the new symbol"
        );
    }

    #[test]
    fn reindex_bare_graph_file_elsewhere_is_actionable_error() {
        let tmp = TempDir::new().expect("tempdir");
        // A graph file NOT named `<root>/.strata/graph.duckdb` (a bare file the
        // user opened from some other location).
        let bare = tmp.path().join("exported.duckdb");
        // The file need not even be a valid store — routing fails on the PATH
        // shape before any indexing is attempted.
        let src = OpenedSource::GraphFile(bare.clone());
        let err = reindex_source(&src).unwrap_err();
        assert!(
            err.contains("open the project folder"),
            "bare graph file reindex must point the user at the project folder: {err}"
        );
        assert!(
            err.contains(&bare.display().to_string()),
            "error names the offending file: {err}"
        );
    }

    /// Minimal recursive directory copy for the estate test (the fixture is a
    /// handful of small files — no need for a crate dependency).
    fn copy_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dst)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            let from = entry.path();
            let to = dst.join(entry.file_name());
            if entry.file_type()?.is_dir() {
                copy_dir(&from, &to)?;
            } else {
                std::fs::copy(&from, &to)?;
            }
        }
        Ok(())
    }
}
