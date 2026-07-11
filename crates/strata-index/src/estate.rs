//! Estate plumbing: workspace manifest parsing and multi-repo indexing/loading.
//!
//! A **workspace manifest** (`strata.workspace.toml`) lists a set of local
//! repositories that together form an *estate*. [`index_estate`] indexes each
//! repo into its own `.strata/graph.duckdb`, using the manifest `name` as that
//! graph's `repo_name` (so node UIDs are repo-qualified and unique across the
//! estate). [`load_estate`] opens every repo store, loads each graph, and unions
//! them into one in-memory estate [`Graph`] for queries.
//!
//! Spec references: R2 (graceful degradation), R3 (determinism).

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;

use strata_contract::{OpIndex, OperationDef};
use strata_core::{AnalyzedFile, Direction, Edge, EdgeKind, Graph, NodeKind, Provenance, Uid};
use strata_store::{DuckGraphStore, GraphStore};

use crate::contract::{canonical_operation_node, canonical_operation_uid, link_consumers_into};
use crate::{extract_repo_operations, index_repo_named, IndexOptions, IndexStats, ResolveMode};

// ── Manifest types ─────────────────────────────────────────────────────────────

/// A parsed and validated workspace manifest (`strata.workspace.toml`).
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct WorkspaceManifest {
    pub workspace: WorkspaceMeta,
    #[serde(default)]
    pub repos: Vec<RepoEntry>,
}

/// The `[workspace]` table in the manifest.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct WorkspaceMeta {
    pub name: String,
}

/// One entry in the `[[repos]]` array.
///
/// `path` is relative to the directory that contains the manifest file.
///
/// `apis` (manifest **v2**, optional) declares the api identities this repo's
/// specs belong to. It is the explicit opt-in that lets two repos share one real
/// API (declare the same `id` in both) — and, conversely, lets one repo host
/// several apis. A v1 manifest (no `apis`) parses unchanged: every operation
/// defaults to `api_id = repo name` (see [`resolve_api_id`]).
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct RepoEntry {
    pub name: String,
    pub path: String,
    #[serde(default)]
    pub apis: Vec<ApiDecl>,
}

/// One `[[repos.apis]]` declaration (manifest v2): an api `id` and the
/// repo-relative spec `spec` that defines it.
///
/// The same `id` declared across repos is the **merge** feature (two repos that
/// share one real API collapse its operations to one canonical node) — so an id
/// is deliberately NOT required to be estate-unique. An `id` must be slug-safe
/// (lowercase ascii alphanumeric + dash) so it composes cleanly into the
/// `{api_id}/{format}` UID discriminator. `spec` is matched against each
/// operation's repo-relative `spec_path`.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct ApiDecl {
    pub id: String,
    pub spec: String,
}

// ── Estate error ───────────────────────────────────────────────────────────────

/// Errors that can occur when working with an estate manifest.
#[derive(Debug, thiserror::Error)]
pub enum EstateError {
    #[error("manifest parse error: {0}")]
    Manifest(String),
    #[error("duplicate repo name in manifest: {0}")]
    DuplicateRepo(String),
    #[error(
        "duplicate repo path in manifest: {0:?} is declared by both {1:?} and {2:?} — two \
         entries indexing one directory would overwrite each other's graph and estate marker \
         (last writer wins)"
    )]
    DuplicateRepoPath(String, String, String),
    #[error("index error: {0}")]
    Index(#[from] crate::IndexError),
    #[error("io error: {0}")]
    Io(String),
}

// ── WorkspaceManifest impl ─────────────────────────────────────────────────────

impl WorkspaceManifest {
    /// Parse a TOML string and validate the manifest.
    ///
    /// Validation rules:
    /// - `workspace.name` must be non-empty.
    /// - Each repo `name` must be non-empty.
    /// - Repo names must be unique.
    pub fn parse_str(toml_text: &str) -> Result<WorkspaceManifest, EstateError> {
        let manifest: WorkspaceManifest =
            toml::from_str(toml_text).map_err(|e| EstateError::Manifest(e.to_string()))?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Parse the manifest from a file at `path` and validate it.
    pub fn parse_file(path: &Path) -> Result<WorkspaceManifest, EstateError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| EstateError::Io(format!("read {}: {e}", path.display())))?;
        Self::parse_str(&text)
    }

    /// Internal validation: non-empty names, unique repo names, unique repo
    /// paths (lexically normalized), slug-safe api ids.
    fn validate(&self) -> Result<(), EstateError> {
        if self.workspace.name.trim().is_empty() {
            return Err(EstateError::Manifest(
                "workspace.name must be non-empty".to_string(),
            ));
        }
        let mut seen: HashSet<String> = HashSet::new();
        // Normalized path → the first repo that declared it. Two entries naming one
        // directory would index into the same `.strata/graph.duckdb` and write the
        // same estate marker (last writer wins), silently losing a declared
        // identity — rejected here at parse time, before any damage. Lexical
        // normalization catches `svc` vs `./svc` vs `x/../svc`; symlink aliasing
        // needs the filesystem and is a documented bound.
        let mut seen_paths: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        for repo in &self.repos {
            if repo.name.trim().is_empty() {
                return Err(EstateError::Manifest(
                    "repo name must be non-empty".to_string(),
                ));
            }
            if !seen.insert(repo.name.clone()) {
                return Err(EstateError::DuplicateRepo(repo.name.clone()));
            }
            let norm = normalize_repo_path(&repo.path);
            if let Some(first) = seen_paths.insert(norm.clone(), repo.name.clone()) {
                return Err(EstateError::DuplicateRepoPath(
                    norm,
                    first,
                    repo.name.clone(),
                ));
            }
            // Manifest v2: each declared api id must be slug-safe so it composes
            // into the `{api_id}/{format}` canonical UID discriminator. The same
            // id across repos is the MERGE feature, so it is NOT required unique.
            for api in &repo.apis {
                if !is_slug_safe(&api.id) {
                    return Err(EstateError::Manifest(format!(
                        "api id {:?} (repo {:?}) must be a non-empty slug: \
                         lowercase ascii letters, digits and dashes only",
                        api.id, repo.name
                    )));
                }
                if api.spec.trim().is_empty() {
                    return Err(EstateError::Manifest(format!(
                        "api {:?} (repo {:?}) must declare a non-empty spec path",
                        api.id, repo.name
                    )));
                }
            }
        }
        Ok(())
    }
}

/// Lexically normalize a manifest repo path for duplicate detection: split on
/// `/` (and `\`), drop `.` and empty components, resolve `..` against preceding
/// components where possible. Pure string arithmetic — no filesystem access, so
/// it is deterministic at parse time; symlink aliasing is a documented bound.
/// An empty path normalizes to `"."` (the manifest dir), so two empty paths
/// still collide.
fn normalize_repo_path(p: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for comp in p.split(['/', '\\']) {
        match comp {
            "" | "." => {}
            ".." => {
                if parts.last().is_some_and(|l| *l != "..") {
                    parts.pop();
                } else {
                    parts.push("..");
                }
            }
            c => parts.push(c),
        }
    }
    if parts.is_empty() {
        ".".to_string()
    } else {
        parts.join("/")
    }
}

/// Whether `s` is a non-empty slug: lowercase ascii alphanumerics and dashes
/// only (no leading/trailing/consecutive-dash restriction — just the character
/// class that keeps an api id clean inside a UID discriminator).
fn is_slug_safe(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Resolve the **api identity** of an operation with repo-relative `spec_path`
/// within a repo whose declared `apis` are `apis` and whose name is `repo_name`.
///
/// Resolution is deliberately boring (B6 fix, two tiers only):
/// 1. **Manifest-declared:** the first `[[repos.apis]]` whose `spec` equals the
///    operation's `spec_path` wins — its `id`. (Multiple apis per repo allowed;
///    declaring the same id in two repos is the cross-repo MERGE opt-in.)
/// 2. **Default:** the repo name. Repos NEVER merge a shared key unless something
///    positively declares the same api id.
///
/// There is intentionally **no spec-intrinsic tier** (no `info.title` heuristic):
/// generic titles ("API", "Backend") are common enough to recreate the very
/// cross-repo collision B6 fixes. Safe-by-construction beats clever — an
/// undeclared spec namespaces to its repo, which can only ever *under*-merge
/// (surfaced honestly as an Ambiguous fan-out), never *over*-merge two unrelated
/// APIs into one confident-wrong node.
fn resolve_api_id<'a>(repo_name: &'a str, apis: &'a [ApiDecl], spec_path: &str) -> &'a str {
    apis.iter()
        .find(|api| api.spec == spec_path)
        .map(|api| api.id.as_str())
        .unwrap_or(repo_name)
}

// ── Result types ───────────────────────────────────────────────────────────────

/// Per-repo outcome from [`index_estate`] or [`load_estate`].
#[derive(Debug, Clone, PartialEq)]
pub struct RepoIndexResult {
    pub name: String,
    pub ok: bool,
    pub stats: Option<IndexStats>,
    pub error: Option<String>,
}

/// Aggregate result from [`index_estate`].
#[derive(Debug, Clone, PartialEq)]
pub struct EstateStats {
    pub estate: String,
    pub repos: Vec<RepoIndexResult>,
    pub total_nodes: usize,
    pub total_edges: usize,
}

// ── index_estate ───────────────────────────────────────────────────────────────

/// Index every repo in the manifest into its own `<repo>/.strata/graph.duckdb`.
///
/// `manifest_path` is the resolved path to the manifest file itself (e.g.
/// `/abs/estate/strata.workspace.toml`). The parent directory is derived from
/// it as the base for repo-relative paths and is recorded in the estate
/// membership marker written into each successfully-indexed repo.
///
/// Each repo is indexed using the manifest `name` as that graph's `repo_name`,
/// so node UIDs are repo-qualified and unique across the estate. A repo that
/// fails to index is recorded as `ok: false` with the error and **skipped** —
/// other repos still index (spec R2). Returns aggregate stats.
pub fn index_estate(
    manifest: &WorkspaceManifest,
    manifest_path: &Path,
    mode: ResolveMode,
) -> EstateStats {
    index_estate_with_options(manifest, manifest_path, mode, false)
}

/// [`index_estate`] with an explicit `include_vendored` switch (the CLI
/// `--include-vendored`). When false (the default), each repo's committed
/// dependency bundles are detected and pruned; when true, they are indexed like
/// first-party code. Workspace indexing never runs a network install
/// (`allow_install` is always false here).
pub fn index_estate_with_options(
    manifest: &WorkspaceManifest,
    manifest_path: &Path,
    mode: ResolveMode,
    include_vendored: bool,
) -> EstateStats {
    let manifest_dir = manifest_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    let mut repos = Vec::new();
    let mut total_nodes: usize = 0;
    let mut total_edges: usize = 0;

    for repo in &manifest.repos {
        let repo_path = manifest_dir.join(&repo.path);

        // Validate the repo path exists before trying to index.
        if !repo_path.exists() {
            repos.push(RepoIndexResult {
                name: repo.name.clone(),
                ok: false,
                stats: None,
                error: Some(format!("repo path does not exist: {}", repo_path.display())),
            });
            continue;
        }

        // Create the .strata/ directory inside the repo.
        let strata_dir = repo_path.join(".strata");
        if let Err(e) = std::fs::create_dir_all(&strata_dir) {
            repos.push(RepoIndexResult {
                name: repo.name.clone(),
                ok: false,
                stats: None,
                error: Some(format!("could not create {}: {e}", strata_dir.display())),
            });
            continue;
        }

        let db_path = strata_dir.join("graph.duckdb");

        let options = IndexOptions {
            resolve_mode: mode,
            allow_install: false,
            include_vendored,
        };

        // Open the store, index, and save — R2: failures are recorded, not propagated.
        let result = (|| -> Result<IndexStats, String> {
            let mut store =
                DuckGraphStore::open(&db_path).map_err(|e| format!("open store: {e}"))?;
            // Use the manifest `name` as the repo_name (additive variant that
            // overrides the directory basename) so UIDs are estate-qualified.
            index_repo_named(&repo_path, &repo.name, &mut store, &options)
                .map_err(|e| e.to_string())
        })();

        match result {
            Ok(stats) => {
                total_nodes += stats.nodes;
                total_edges += stats.edges;
                // Best-effort: write the estate membership marker so commands
                // run from inside this repo can resolve the estate. Never fails
                // the index — `let _ =` swallows any IO error.
                let _ = crate::estate_marker::write_marker(
                    &strata_dir,
                    &crate::estate_marker::EstateMarker {
                        manifest: manifest_path
                            .canonicalize()
                            .unwrap_or_else(|_| manifest_path.to_path_buf()),
                        estate: manifest.workspace.name.clone(),
                        repo: repo.name.clone(),
                    },
                );
                repos.push(RepoIndexResult {
                    name: repo.name.clone(),
                    ok: true,
                    stats: Some(stats),
                    error: None,
                });
            }
            Err(e) => {
                repos.push(RepoIndexResult {
                    name: repo.name.clone(),
                    ok: false,
                    stats: None,
                    error: Some(e),
                });
            }
        }
    }

    EstateStats {
        estate: manifest.workspace.name.clone(),
        repos,
        total_nodes,
        total_edges,
    }
}

// ── load_estate ────────────────────────────────────────────────────────────────

/// Open every repo's store, load its graph, and union them into one estate [`Graph`].
///
/// A repo whose store is missing or unreadable is skipped (recorded in the
/// returned diagnostics). The result is deterministic: nodes and edges are
/// unioned in manifest order (R3).
pub fn load_estate(
    manifest: &WorkspaceManifest,
    manifest_dir: &Path,
) -> (Graph, Vec<RepoIndexResult>) {
    let mut estate = Graph::new();
    let mut results = Vec::new();

    for repo in &manifest.repos {
        let repo_path = manifest_dir.join(&repo.path);
        let db_path = repo_path.join(".strata").join("graph.duckdb");

        if !db_path.exists() {
            results.push(RepoIndexResult {
                name: repo.name.clone(),
                ok: false,
                stats: None,
                error: Some(format!(
                    "store not found at {}; run `strata index` first",
                    db_path.display()
                )),
            });
            continue;
        }

        let graph_result = (|| -> Result<Graph, String> {
            let store = DuckGraphStore::open(&db_path).map_err(|e| format!("open store: {e}"))?;
            store.load_graph().map_err(|e| format!("load graph: {e}"))
        })();

        match graph_result {
            Ok(repo_graph) => {
                let node_count = repo_graph.node_count();
                let edge_count = repo_graph.edge_count();
                union_into(&mut estate, repo_graph);
                results.push(RepoIndexResult {
                    name: repo.name.clone(),
                    ok: true,
                    stats: Some(IndexStats {
                        nodes: node_count,
                        edges: edge_count,
                        ..IndexStats::default()
                    }),
                    error: None,
                });
            }
            Err(e) => {
                results.push(RepoIndexResult {
                    name: repo.name.clone(),
                    ok: false,
                    stats: None,
                    error: Some(e),
                });
            }
        }
    }

    (estate, results)
}

// ── link_estate (dedup-by-key + re-point + cross-repo consumer linking) ──────────

/// Consumer links bucketed by provenance tier (brief §6). Producers are never
/// `Ambiguous`-only here; consumers are never `Extracted`. The shape is fixed by
/// the brief so the report's columns are stable across formats.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TierCounts {
    /// Links at Extracted provenance (consumers: always 0 — a consumer link is
    /// never a fact; the field exists for the report's fixed column shape).
    pub extracted: usize,
    /// Links at Inferred provenance (a unique, confident convention match).
    pub inferred: usize,
    /// Links at Ambiguous provenance (several candidate operations matched).
    pub ambiguous: usize,
}

/// Estate-wide link coverage (R4): the headline numbers the committed
/// `docs/accuracy/openapi-linking.md` report publishes and the CI gate floors.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct EstateLinkCoverage {
    /// Total `PRODUCES` edges in the estate graph (after dedup/re-point).
    pub producers_total: usize,
    /// Total `CONSUMES` edges in the estate graph (after dedup/re-point + the
    /// cross-repo pass). Includes the api fan-out edges (they are real, honestly
    /// graded edges) — read `consumers_ambiguous` to see how many are fan-out.
    pub consumers_total: usize,
    /// Of `consumers_total`, the `CONSUMES` edges that are an **api fan-out**
    /// (B6 fix): a consumer whose matched `(format, key)` is owned by SEVERAL apis
    /// emits one `Ambiguous` 0.35 edge per api. This field keeps the headline
    /// "confidently linked" reading honest — the unique-owner consumer count is
    /// `consumers_total - consumers_ambiguous`, so a fan-out never inflates it.
    /// Equals `by_tier.ambiguous` for consumers (the only source of Ambiguous
    /// `CONSUMES` edges); surfaced separately so the report can state it plainly.
    /// Unique-key estates report **0** (additive).
    pub consumers_ambiguous: usize,
    /// Consumer (`CONSUMES`) edges bucketed by provenance tier.
    pub by_tier: TierCounts,
    /// Outgoing HTTP calls (`fetch`/`axios`) across all repos that produced NO
    /// consumer link — a dynamic URL or an endpoint no operation declares. The
    /// honest "we saw a call we could not link" count (never an invented edge).
    pub unmatched_consumers: usize,
    /// **Tagged** GraphQL documents (`gql`/`graphql` templates and
    /// `.graphql`/`.gql` files) that could NOT be parsed into root fields and so
    /// produced no link: an **interpolated** template (text unreliable) or a
    /// document `parse_operations` rejects (e.g. comment-only / empty). The honest
    /// "we saw a gql document we could not link" count — never a guessed field
    /// (R1/R5). **Untagged** template-constant candidates are NOT counted here: a
    /// parse failure means it was never GraphQL, so it is silently skipped, not an
    /// honest miss. OpenAPI-only estates report 0 (additive).
    pub unparsed_documents: usize,
    /// Root-level fragment spreads / inline fragments across all parsed GraphQL
    /// documents that were seen but **not** expanded into a concrete field (a
    /// fragment's body is opaque here). Counted, never guessed (R1/R5);
    /// surfaced so the report shows the gap. OpenAPI-only estates report 0.
    pub unresolved_root_spreads: usize,
}

/// Open every repo's store, load its graph **and parse cache**, then build one
/// estate [`Graph`] in which operations are deduped across repos by `key` and
/// consumer links span repos (brief §4 — the cross-repo blast-radius payoff).
///
/// Pipeline (deterministic, manifest order throughout; R3):
/// 1. Per repo: load the persisted graph + parse cache; re-extract the spec
///    operations from disk (the authoritative `OperationDef` metadata —
///    method/normalized-path/operationId — the bare `ApiOperation` node does not
///    carry). A repo whose store is missing/unreadable is skipped (R2).
/// 2. Dedup operations by `key`: one **canonical** `ApiOperation` node per key
///    ([`canonical_operation_uid`]); a map from each repo-local op UID to its
///    canonical UID.
/// 3. Union every repo graph into the estate, **dropping** the per-repo op nodes
///    and **re-pointing** every edge endpoint through the canonical map — so all
///    `PRODUCES`/`CONSUMES` edges land on the canonical node. Edges de-duped.
/// 4. Cross-repo consumer linking: run the shared `match_consumer` over each
///    repo's parse-cache consumer signals against the FULL canonical operation
///    set, adding `CONSUMES` edges not already present (catches a consumer repo
///    with NO local spec — the real microservice case). Edges de-duped.
/// 5. Compute [`EstateLinkCoverage`].
pub fn link_estate(
    manifest: &WorkspaceManifest,
    manifest_dir: &Path,
) -> (Graph, EstateLinkCoverage, Vec<RepoIndexResult>) {
    let estate_name = manifest.workspace.name.as_str();
    let mut results = Vec::new();

    // ── Step 1: load each repo's graph + parse cache + re-extract operations. ──
    struct RepoData {
        name: String,
        /// The repo's declared apis (manifest v2 `[[repos.apis]]`); empty for v1.
        apis: Vec<ApiDecl>,
        graph: Graph,
        analyzed: BTreeMap<String, AnalyzedFile>,
        operations: Vec<OperationDef>,
    }
    let mut loaded: Vec<RepoData> = Vec::new();

    for repo in &manifest.repos {
        let repo_path = manifest_dir.join(&repo.path);
        let db_path = repo_path.join(".strata").join("graph.duckdb");

        if !db_path.exists() {
            results.push(RepoIndexResult {
                name: repo.name.clone(),
                ok: false,
                stats: None,
                error: Some(format!(
                    "store not found at {}; run `strata index` first",
                    db_path.display()
                )),
            });
            continue;
        }

        let loaded_repo = (|| -> Result<RepoData, String> {
            let store = DuckGraphStore::open(&db_path).map_err(|e| format!("open store: {e}"))?;
            let graph = store.load_graph().map_err(|e| format!("load graph: {e}"))?;
            // Parse cache → the consumer signals (http_calls + calls) per file.
            let cache = store
                .load_parse_cache()
                .map_err(|e| format!("load parse cache: {e}"))?;
            let analyzed: BTreeMap<String, AnalyzedFile> = cache
                .into_iter()
                .map(|(path, entry)| (path, entry.analyzed))
                .collect();
            // Re-extract operations from disk for full OperationDef fidelity. A
            // broken/absent spec just yields fewer ops (R2) — never a panic.
            //
            // The vendored set is EMPTY here by design: `link_estate` loads repos
            // that were already indexed (each via `index_repo*`, where vendored
            // pruning ran at index time and never wrote vendored nodes/specs to the
            // store). Re-deriving the set from disk would be redundant work, and the
            // graph being linked already excludes vendored paths — so an empty set
            // ("no extra pruning here") is correct, not a gap. A spec that survived
            // indexing is first-party; one that was pruned is absent from the graph
            // these ops link into.
            let operations =
                extract_repo_operations(&repo_path, std::sync::Arc::new(HashSet::new()))
                    .unwrap_or_default();
            Ok(RepoData {
                name: repo.name.clone(),
                apis: repo.apis.clone(),
                graph,
                analyzed,
                operations,
            })
        })();

        match loaded_repo {
            Ok(data) => {
                let node_count = data.graph.node_count();
                let edge_count = data.graph.edge_count();
                results.push(RepoIndexResult {
                    name: repo.name.clone(),
                    ok: true,
                    stats: Some(IndexStats {
                        nodes: node_count,
                        edges: edge_count,
                        ..IndexStats::default()
                    }),
                    error: None,
                });
                loaded.push(data);
            }
            Err(e) => {
                results.push(RepoIndexResult {
                    name: repo.name.clone(),
                    ok: false,
                    stats: None,
                    error: Some(e),
                });
            }
        }
    }

    // ── Step 2: dedup operations by (api_id, format, key) → canonical nodes +
    // re-point map. ──
    //
    // `canonical_ops`: one (api_id, OperationDef) per `(api_id, format, key)`
    // (first occurrence in manifest order wins), for the cross-repo OpIndex and
    // the canonical nodes. `local_to_canonical`: every repo-local op UID → its
    // api-scoped canonical UID, for edge re-pointing.
    //
    // Keying on `(api_id, format, key)` (B6 fix) keeps two UNRELATED apis that
    // share a key on distinct canonical nodes — the api_id defaults to the repo
    // name, so repos never merge unless they positively declare the same api id
    // (the `[[repos.apis]]` opt-in). The format part still keeps a GraphQL field
    // and an OpenAPI op of the same key string apart.
    let mut canonical_ops: Vec<(String, OperationDef)> = Vec::new();
    let mut seen_keys: BTreeSet<(String, strata_contract::ContractFormat, String)> =
        BTreeSet::new();
    let mut local_to_canonical: BTreeMap<Uid, Uid> = BTreeMap::new();

    for repo in &loaded {
        for op in &repo.operations {
            let api_id = resolve_api_id(&repo.name, &repo.apis, &op.spec_path);
            let canonical = canonical_operation_uid(estate_name, api_id, op.format, &op.key);
            let local = crate::contract::operation_uid(&repo.name, op);
            local_to_canonical.insert(local, canonical);
            if seen_keys.insert((api_id.to_string(), op.format, op.key.clone())) {
                canonical_ops.push((api_id.to_string(), op.clone()));
            }
        }
    }

    // ── Step 3: union repos into the estate, dropping per-repo op nodes and
    // re-pointing every edge endpoint to the canonical node. ──
    let mut estate = Graph::new();

    // Canonical operation nodes first (so re-pointed edges land on real nodes).
    // One node per `(api_id, format, key)` — two unrelated apis sharing a key get
    // two nodes.
    for (api_id, op) in &canonical_ops {
        estate.add_node(canonical_operation_node(estate_name, api_id, op));
    }

    // Track every edge we add (by re-pointed src/dst/kind) for de-dup.
    // `HashSet` (not `BTreeSet`): `EdgeKind` is `Hash`+`Eq` but not `Ord`.
    let mut edge_seen: HashSet<(Uid, Uid, EdgeKind)> = HashSet::new();

    for repo in &loaded {
        let node_uids: Vec<Uid> = repo.graph.nodes().map(|n| n.uid.clone()).collect();
        // Nodes: everything EXCEPT the per-repo contract-plane operation nodes
        // (ApiOperation / GraphqlField), which are replaced by the canonical ones
        // already added.
        for node in repo.graph.nodes() {
            if matches!(node.kind, NodeKind::ApiOperation | NodeKind::GraphqlField) {
                continue;
            }
            estate.add_node(node.clone());
        }
        // Edges: re-point both endpoints through the canonical map; de-dup.
        for uid in &node_uids {
            for (edge, _) in repo.graph.neighbors(uid, Direction::Outgoing, &[]) {
                let src = local_to_canonical
                    .get(&edge.src)
                    .cloned()
                    .unwrap_or_else(|| edge.src.clone());
                let dst = local_to_canonical
                    .get(&edge.dst)
                    .cloned()
                    .unwrap_or_else(|| edge.dst.clone());
                if !edge_seen.insert((src.clone(), dst.clone(), edge.kind)) {
                    continue;
                }
                estate.add_edge(Edge {
                    src,
                    dst,
                    kind: edge.kind,
                    provenance: edge.provenance,
                    confidence: edge.confidence,
                });
            }
        }
    }

    // ── Step 4: cross-repo consumer linking against the canonical set. ──
    //
    // Seed the existing-edge set with the CONSUMES edges already in the estate
    // (the re-pointed per-repo links) so we only ADD cross-repo links. The
    // canonical key→uid map points every `(format, key)` at the canonical node(s)
    // that own it — SEVERAL when two unrelated apis share the key, which drives
    // the Ambiguous fan-out in `add_consumer_links` (B6 fix).
    let canonical_op_defs: Vec<OperationDef> =
        canonical_ops.iter().map(|(_, op)| op.clone()).collect();
    let ops = OpIndex::new(&canonical_op_defs);
    let mut canonical_key_to_uid: BTreeMap<(strata_contract::ContractFormat, String), Vec<Uid>> =
        BTreeMap::new();
    for (api_id, op) in &canonical_ops {
        canonical_key_to_uid
            .entry((op.format, op.key.clone()))
            .or_default()
            .push(canonical_operation_uid(
                estate_name,
                api_id,
                op.format,
                &op.key,
            ));
    }
    let mut existing_consumes: BTreeSet<(Uid, Uid)> = BTreeSet::new();
    let estate_node_uids: Vec<Uid> = estate.nodes().map(|n| n.uid.clone()).collect();
    for uid in &estate_node_uids {
        for (edge, _) in estate.neighbors(uid, Direction::Outgoing, &[EdgeKind::Consumes]) {
            existing_consumes.insert((edge.src.clone(), edge.dst.clone()));
        }
    }
    for repo in &loaded {
        link_consumers_into(
            &mut estate,
            &repo.name,
            &repo.analyzed,
            &ops,
            &canonical_key_to_uid,
            &mut existing_consumes,
        );
    }

    // ── Step 5: coverage. ──
    let coverage = compute_coverage(
        &estate,
        &loaded.iter().map(|r| &r.analyzed).collect::<Vec<_>>(),
        &ops,
    );

    (estate, coverage, results)
}

/// Tally [`EstateLinkCoverage`] from the finished estate graph plus the repos'
/// consumer signals. Producer/consumer totals and the consumer tier buckets come
/// from the graph's edges; `unmatched_consumers` counts HTTP calls (the
/// unambiguous consumer signal) that the canonical operation set did not match.
fn compute_coverage(
    estate: &Graph,
    analyzed_sets: &[&BTreeMap<String, AnalyzedFile>],
    ops: &OpIndex,
) -> EstateLinkCoverage {
    let mut cov = EstateLinkCoverage::default();
    for node in estate.nodes() {
        for (edge, _) in estate.neighbors(&node.uid, Direction::Outgoing, &[]) {
            match edge.kind {
                EdgeKind::Produces => cov.producers_total += 1,
                EdgeKind::Consumes => {
                    cov.consumers_total += 1;
                    match edge.provenance {
                        Provenance::Extracted => cov.by_tier.extracted += 1,
                        Provenance::Ambiguous => {
                            cov.by_tier.ambiguous += 1;
                            // An Ambiguous CONSUMES edge is an api fan-out (B6):
                            // surfaced separately so it never inflates the
                            // "confidently linked" reading.
                            cov.consumers_ambiguous += 1;
                        }
                        // Inferred (the unique-match tiers) and anything else
                        // band-respecting count as inferred for the report.
                        _ => cov.by_tier.inferred += 1,
                    }
                }
                _ => {}
            }
        }
    }
    // Unmatched HTTP-call consumer signals: a fetch/axios call whose method+URL
    // matched no operation (dynamic URL or undeclared endpoint).
    for analyzed in analyzed_sets {
        for file in analyzed.values() {
            for http in &file.http_calls {
                if strata_contract::match_consumer(None, Some(http), ops).is_empty() {
                    cov.unmatched_consumers += 1;
                }
            }
            // GraphQL documents — honest accounting split by provenance:
            //
            // - TAGGED (`gql`/`graphql` tag or `.graphql` file): the author
            //   declared it GraphQL, so a miss is honest — an interpolated template
            //   (text unreliable) or one `parse_operations` rejects (comment-only/
            //   empty) is counted *unparsed* (the GraphQL analogue of
            //   `unmatched_consumers`). A tagged doc that parses (even to zero
            //   fields, e.g. a fragment) is not — its root-level fragment spreads
            //   accumulate as `unresolved_root_spreads`.
            // - UNTAGGED (template-constant candidate): parse-gated. It is always
            //   interpolation-free by construction; `parse_operations` Ok → its
            //   spreads accumulate exactly like a tagged doc, but Err → it is
            //   SILENTLY SKIPPED (NOT counted unparsed) — it never claimed to be
            //   GraphQL, so a parse failure is not an honest GraphQL miss.
            for doc in &file.gql_documents {
                if !doc.interpolation_free {
                    // Only tagged templates can be interpolated (an untagged
                    // candidate with substitutions is never emitted); count it.
                    cov.unparsed_documents += 1;
                    continue;
                }
                match strata_contract::parse_operations("doc", &doc.text) {
                    Ok(consumption) => {
                        cov.unresolved_root_spreads += consumption.unresolved_root_spreads;
                    }
                    Err(_) if doc.tagged => cov.unparsed_documents += 1,
                    // Untagged candidate that did not parse: silently skipped.
                    Err(_) => {}
                }
            }
        }
    }
    cov
}

// ── Helpers ────────────────────────────────────────────────────────────────────

/// Merge all nodes and edges from `src` into `dst`.
fn union_into(dst: &mut Graph, src: Graph) {
    // Collect all node uids first so we can iterate edges per uid.
    let node_uids: Vec<_> = src.nodes().map(|n| n.uid.clone()).collect();

    // Add all nodes.
    for node in src.nodes() {
        dst.add_node(node.clone());
    }

    // Collect all unique edges by iterating outgoing neighbours across ALL
    // edge kinds. We use ALL_EDGE_KINDS to avoid missing any variant.
    for uid in &node_uids {
        // Passing an empty kinds slice means "all edge kinds".
        for (edge, _) in src.neighbors(uid, Direction::Outgoing, &[]) {
            dst.add_edge(edge.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn api(id: &str, spec: &str) -> ApiDecl {
        ApiDecl {
            id: id.to_string(),
            spec: spec.to_string(),
        }
    }

    // ── resolve_api_id: the two-tier (declared spec else repo name) resolver. ──

    #[test]
    fn resolve_api_id_defaults_to_repo_name_when_undeclared() {
        // No declared apis → the repo name. This is the safe default that keeps
        // two unrelated repos from ever merging a shared key.
        assert_eq!(
            resolve_api_id("repo-user", &[], "schema.graphql"),
            "repo-user"
        );
        // Declared apis that do NOT match this spec → still the repo name.
        let apis = [api("billing", "billing/openapi.yaml")];
        assert_eq!(
            resolve_api_id("repo-user", &apis, "schema.graphql"),
            "repo-user"
        );
    }

    #[test]
    fn resolve_api_id_uses_declared_id_on_spec_match() {
        // A declared spec that matches the operation's repo-relative spec_path
        // wins — its id. This is the cross-repo MERGE opt-in.
        let apis = [
            api("user", "openapi.yaml"),
            api("admin", "admin/openapi.yaml"),
        ];
        assert_eq!(resolve_api_id("repo-svc", &apis, "openapi.yaml"), "user");
        assert_eq!(
            resolve_api_id("repo-svc", &apis, "admin/openapi.yaml"),
            "admin"
        );
        // A spec_path matching no declaration falls back to the repo name.
        assert_eq!(resolve_api_id("repo-svc", &apis, "other.yaml"), "repo-svc");
    }

    #[test]
    fn resolve_api_id_first_match_wins() {
        // Deterministic: the FIRST declaration whose spec matches wins.
        let apis = [api("first", "openapi.yaml"), api("second", "openapi.yaml")];
        assert_eq!(resolve_api_id("repo", &apis, "openapi.yaml"), "first");
    }

    // ── is_slug_safe: the api-id character-class gate. ──

    #[test]
    fn is_slug_safe_accepts_lowercase_alnum_dash() {
        assert!(is_slug_safe("user"));
        assert!(is_slug_safe("user-service"));
        assert!(is_slug_safe("svc1"));
        assert!(is_slug_safe("a-b-c-2"));
    }

    #[test]
    fn is_slug_safe_rejects_uppercase_space_underscore_empty() {
        assert!(!is_slug_safe(""));
        assert!(!is_slug_safe("User"));
        assert!(!is_slug_safe("user service"));
        assert!(!is_slug_safe("user_svc"));
        assert!(!is_slug_safe("user.svc"));
        assert!(!is_slug_safe("user/svc"));
    }
}
