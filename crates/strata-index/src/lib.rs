//! strata-index: the cross-file code-plane graph orchestrator.
//!
//! [`index_repo`] walks a TS/JS repository (honouring `.gitignore`),
//! content-hashes each file, and incrementally builds the graph by reusing
//! the persisted [`strata_store::ParseCacheEntry`] for any file whose content
//! hash has not changed since the last index.  Only changed (or new) files are
//! re-parsed with Tree-sitter.  The graph is always assembled from the **full**
//! current set of `AnalyzedFile`s via the pure [`assemble_graph`], so the
//! result is identical to a full rebuild (`incremental == full` invariant).
//!
//! The graph-building logic ([`assemble_graph`], [`build_graph`]) is a pure,
//! deterministic function of the analysed-file set and is unit-testable without
//! any IO.

mod build;
mod changes;
mod contract;
mod data;
mod differential;
mod estate;
pub mod estate_marker;
mod fs;
mod infra;
mod rename;
mod resolve_mode;
mod scip_merge;
mod stamp;
mod tsconfig;

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use strata_core::{AnalyzedFile, LanguageAnalyzer};
use strata_lang_cs::{analyze as analyze_cs, CsAnalyzer};
use strata_lang_py::{analyze as analyze_py, PyAnalyzer};
use strata_lang_rust::{analyze as analyze_rust, RustAnalyzer};
use strata_lang_ts::{analyze, ResolveOptions, TsAnalyzer};
use strata_store::{GraphStore, ParseCacheEntry};

use crate::resolve_mode::resolve_scip;

pub use build::{
    assemble_graph, assemble_graph_with_scip, assemble_with_coverage, build_graph, HeuristicClass,
    ResolutionCoverage,
};
// The §4.1 heuristic-band calibration constants, re-exported so the band-invariant
// guardrail (`tests/confidence_bands.rs`) asserts the PRODUCTION values, not a
// drifting local copy. Each is the measured-or-capped grade for one heuristic class.
pub use build::{CONF_BARE_MULTI, CONF_BARE_SINGLE, CONF_THIS_METHOD, CONF_UNKNOWN_RECEIVER};
// The contract producer-link band constants (Inferred single / Ambiguous multi),
// re-exported for the same band-guardrail purpose.
pub use contract::{CONF_PRODUCES_MULTI, CONF_PRODUCES_SINGLE};
// The data-plane fact constants (DDL + explicit-ORM, both Extracted 0.95).
pub use data::{CONF_DATA_FACT, CONF_ORM_EXPLICIT};
// The infra-plane band constants (Extracted refs/runs/produces + Inferred refs/produces).
pub use changes::{
    blast_for_file, blast_for_file_in_repo, detect_changes, detect_changes_in_repo, AffectedNode,
    BlastReport, BlastSymbol, ChangeKind, ChangeReport, ChangeScope, ChangedSymbol, ChangesError,
    FileChange, Plane, Risk, RiskLevel, LOW_MAX, MEDIUM_MAX,
};
pub use contract::{assemble_graph_with_contracts, build_contract_plane};
pub use data::{
    assemble_graph_with_data, build_data_plane, CodeOrmFile, CodeSqlFile, DataLinkCoverage,
};
pub use differential::{
    accuracy_report, resolve_differential, resolve_differential_graph, AccuracyReport, Band,
    BandMetrics, ClassMetrics, SiteOutcome, ALL_BANDS, ALL_CLASSES,
};
pub use estate::{
    index_estate, index_estate_with_options, link_estate, load_estate, EstateError,
    EstateLinkCoverage, EstateStats, RepoEntry, RepoIndexResult, TierCounts, WorkspaceManifest,
    WorkspaceMeta,
};
pub use fs::{BTreeMapModuleFs, FsModuleFs};
pub use infra::{
    assemble_graph_with_infra, build_infra_plane, build_terragrunt_plane, InfraLinkCoverage,
    TerragruntCoverage,
};
pub use infra::{
    CONF_PRODUCES_EXTRACTED, CONF_PRODUCES_INFERRED, CONF_REF_INFERRED, CONF_REF_RESOURCE,
    CONF_RUNS,
};
pub use rename::{
    rename, Candidate, Edit, RenameError, RenameOptions, RenameOutcome, DEF_SITE_CONFIDENCE,
};
pub use resolve_mode::{IndexOptions, ResolveMode};
pub use stamp::{IndexStamp, STAMP_FILE};
// Re-export the Python plane assembler so hermetic three-plane tests (and any
// in-memory caller) can build the `py`-tagged code plane the same way
// `index_impl` does, without depending on `strata-lang-py` directly.
pub use strata_lang_py::{assemble_python, PyLinkCoverage};
// Likewise re-export the C# plane assembler (Slice 11) so a mixed ts+py+cs test
// can assemble the `cs`-tagged plane without a direct `strata-lang-cs` dep.
pub use strata_lang_cs::{assemble_csharp, CsLinkCoverage};
// Likewise re-export the Rust plane assembler (Slice 21) so a mixed
// ts+py+cs+rust test can assemble the `rust`-tagged plane without a direct
// `strata-lang-rust` dep.
pub use strata_lang_rust::{assemble_rust, RustLinkCoverage};

/// Summary counts returned by [`index_repo`].
#[derive(Debug, Default, Clone, PartialEq)]
pub struct IndexStats {
    pub files_indexed: usize,
    pub nodes: usize,
    pub edges: usize,
    /// Number of files that were re-parsed (content hash changed or first index).
    pub files_parsed: usize,
    /// Number of files whose `AnalyzedFile` was reused from the parse cache.
    pub files_reused: usize,
    /// The precise-resolution mode that was requested for this index.
    pub resolution_mode: ResolveMode,
    /// `true` when an `Auto` index fell back to the heuristic because SCIP was
    /// unavailable or failed (R1). Always `false` for `Off`, and for a
    /// successful `On`/`Auto` run.
    pub degraded: bool,
    /// Total call/import-name sites considered (resolution coverage, spec A5).
    pub sites_total: usize,
    /// Sites resolved precisely by SCIP.
    pub sites_resolved: usize,
    /// Sites that fell back to a non-ambiguous heuristic edge.
    pub sites_heuristic: usize,
    /// Sites that fell back to an ambiguous heuristic edge.
    pub sites_ambiguous: usize,
    /// Infrastructure-plane link coverage: detected templates, resources, and the
    /// resolver/Lambda linking tallies (Slice 5, M2). A repo with no CFN/SAM
    /// templates reports all-zero (additive). Now also fed by Terraform `.tf`/plan
    /// resources (Track D1) — they flow through the SAME `build_infra_plane`, so
    /// `templates_detected`/`resources_total` count TF configs/resources too.
    pub infra_link: InfraLinkCoverage,
    /// Terragrunt structural coverage (Track D1, M2): detected `terragrunt.hcl`
    /// units and how many of their `dependency` config-paths resolved to a known
    /// same-repo unit. A repo with no Terragrunt reports all-zero (additive).
    pub terragrunt: TerragruntCoverage,
    /// Data-plane link coverage (Slice 16, D3): detected `.sql` schema files, the
    /// `Table`/`Column` nodes built, and how many foreign keys resolved to a
    /// declared column. A repo with no `.sql` schema files reports all-zero
    /// (additive). Fed by `build_data_plane`.
    pub data_link: DataLinkCoverage,
    /// Per-schema parse-failure diagnostics (`<path>: parse error: …`), one per
    /// `.sql` file that carries the SQL DDL textual signal but could not be parsed
    /// (a malformed/truncated schema). The human-readable companion to
    /// [`DataLinkCoverage::schemas_failed`]; the CLI prints each as a
    /// `[data] FAILED …` line so a skipped schema is never silent. Capped at
    /// [`MAX_INFRA_DIAGNOSTICS`] like the infra diagnostics.
    pub data_diagnostics: Vec<String>,
    /// Per-template parse-failure diagnostics (`<path>: parse error: …`), one per
    /// file that carries the CFN textual signal but could not be parsed (a
    /// malformed/truncated template, surfaced via `CfnSamAdapter::detect_kind`).
    /// These are the human-readable companion to
    /// [`InfraLinkCoverage::templates_failed`];
    /// the CLI prints each as an `[infra] FAILED …` line so a skipped template is
    /// never silent. Capped at [`MAX_INFRA_DIAGNOSTICS`] (with a final
    /// `… and N more` line) so a pathological repo cannot flood the output.
    pub infra_diagnostics: Vec<String>,
}

/// The cap on the number of per-template diagnostic lines carried in
/// [`IndexStats::infra_diagnostics`] (and printed by the CLI). Beyond this, a
/// single trailing `… and N more infra template failure(s)` line summarises the
/// remainder — the count in [`InfraLinkCoverage::templates_failed`] is always
/// exact regardless.
pub const MAX_INFRA_DIAGNOSTICS: usize = 20;

/// Errors that can occur while indexing a repository from disk.
#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    #[error("io error at {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("store error: {0}")]
    Store(#[from] strata_store::StoreError),
    /// Precise resolution was *required* (`--resolve on`) but `scip-typescript`
    /// could not produce a usable index (R5).
    #[error("precise resolution required but failed: {0}")]
    Scip(#[from] strata_scip::ScipError),
}

/// Read every TS/JS file under `repo_path` (honouring `.gitignore`), build the
/// graph incrementally, and persist the graph, file-hash map, and parse cache
/// through `store`.
///
/// **Incremental strategy:**
/// 1. Load the previously persisted file-hash map and parse cache from `store`.
/// 2. Walk current files; compute each file's blake3 content hash.
/// 3. For each current file:
///    - If a cache entry exists with a matching hash → **reuse** (`files_reused += 1`).
///    - Otherwise → re-parse with Tree-sitter (`files_parsed += 1`).
/// 4. Build the current `{path → AnalyzedFile}` map from (reused ∪ freshly-parsed).
///    Files no longer present in the repo are *not* carried over.
/// 5. `assemble_graph(current_map, ...)` — same pure function, same result as
///    a full rebuild would produce.
/// 6. Persist: graph, current hashes, current parse cache.
///
/// Resolution defaults to [`IndexOptions::default`] (`Auto`, no network install)
/// — so precise SCIP resolution kicks in automatically when `typescript` is
/// already installed for the repo, and otherwise degrades cleanly to the
/// slice-1 heuristic. Use [`index_repo_with_options`] to choose the mode or to
/// permit a `scip-typescript` install.
pub fn index_repo(repo_path: &Path, store: &mut dyn GraphStore) -> Result<IndexStats, IndexError> {
    index_repo_with_options(repo_path, store, &IndexOptions::default())
}

/// [`index_repo`] with an explicit [`IndexOptions`] (resolution mode + whether a
/// `scip-typescript` install may run). Honours the `auto`/`on`/`off` switch:
/// `On` returns [`IndexError::Scip`] when SCIP cannot run (R5); `Auto` degrades
/// to the heuristic (R1); `Off` never runs SCIP. The graph is always assembled
/// from the full current `AnalyzedFile` set, preserving the
/// `incremental == full` invariant for the parse cache.
pub fn index_repo_with_options(
    repo_path: &Path,
    store: &mut dyn GraphStore,
    options: &IndexOptions,
) -> Result<IndexStats, IndexError> {
    let repo_name = repo_name_of(repo_path);
    index_impl(repo_path, &repo_name, store, options)
}

/// [`index_repo_with_options`] but with an **explicit** `repo_name` that
/// overrides the directory basename. This is the additive helper used by
/// [`estate::index_estate`] so the manifest `name` qualifies all UIDs.
/// The public `index_repo`/`index_repo_with_options` entry points are unchanged.
///
/// Also used by `cmd_index` (strata-cli) when the repo carries a valid
/// `.strata/estate.toml` marker, so a plain `strata index <member>` reindexes
/// with the estate-qualified name rather than the directory basename.
pub fn index_repo_named(
    repo_path: &Path,
    repo_name: &str,
    store: &mut dyn GraphStore,
    options: &IndexOptions,
) -> Result<IndexStats, IndexError> {
    index_impl(repo_path, repo_name, store, options)
}

/// Shared implementation for all public indexing entry points.
///
/// Walks `repo_path`, applies the incremental cache strategy, runs SCIP if
/// requested, assembles the graph, and persists the result through `store`.
/// Both `index_repo_with_options` (which derives `repo_name` from the dir
/// basename) and `index_repo_named` (which accepts an explicit name) delegate
/// here so there is exactly one copy of the build logic.
fn index_impl(
    repo_path: &Path,
    repo_name: &str,
    store: &mut dyn GraphStore,
    options: &IndexOptions,
) -> Result<IndexStats, IndexError> {
    // One pre-scan finds committed third-party dependency *bundles*: a `*.dist-info`
    // metadata directory's `RECORD` lists the exact FILES a wheel install wrote next
    // to it. Those files (resolved to real paths) are pruned from every plane below
    // by-file — never by directory name — so vendored code never inflates the graph
    // (or the `rename` tool's "implicated files" set), while a co-located first-party
    // file the RECORD does not list always survives (no first-party loss).
    // `--include-vendored` (carried on `options`) disables detection — the bundle is
    // indexed like first-party code. `.strataignore` still applies regardless: it is
    // the walker's own exclude list, not part of this auto-detection.
    let vendored = Arc::new(if options.include_vendored {
        HashSet::new()
    } else {
        discover_vendored_paths(repo_path)?
    });

    let files = collect_sources(repo_path, Arc::clone(&vendored))?;
    // Python sources are collected separately (their own extension + skip-dirs)
    // and linked in their own resolution world; they never reach SCIP or the TS
    // resolver. They DO share the parse cache and hash map (disjoint path keys).
    let py_files = collect_python_sources(repo_path, Arc::clone(&vendored))?;
    // C# sources, likewise collected separately (their own extension + skip-dirs:
    // bin/obj/packages/.vs) and linked in their own `cs` resolution world. Same
    // cache/hash machinery, disjoint path keys; never reach SCIP/TS/Python.
    let cs_files = collect_csharp_sources(repo_path, Arc::clone(&vendored))?;
    // Rust sources (`.rs`), likewise collected separately (their own extension +
    // skip-dirs — critically `target/`, the huge Rust build-output dir) and linked
    // in their own `rust` resolution world. Same cache/hash machinery, disjoint path
    // keys; never reach SCIP/TS/Python/C#.
    let rust_files = collect_rust_sources(repo_path, Arc::clone(&vendored))?;
    let mut current_hashes = hash_files(&files);
    let opts = load_tsconfig(repo_path)?;

    // Load previously persisted cache state (empty maps on first run).
    let old_cache = store.load_parse_cache()?;

    // Decide for each current file: reuse or parse.
    let mut new_cache: BTreeMap<String, ParseCacheEntry> = BTreeMap::new();
    let mut analyzed_map: BTreeMap<String, AnalyzedFile> = BTreeMap::new();
    let mut files_parsed: usize = 0;
    let mut files_reused: usize = 0;

    for (path, source) in &files {
        let current_hash = current_hashes
            .get(path)
            .expect("hash_files covers all files");

        if let Some(entry) = old_cache.get(path) {
            if &entry.hash == current_hash {
                // Cache hit: reuse AnalyzedFile without re-parsing.
                analyzed_map.insert(path.clone(), entry.analyzed.clone());
                new_cache.insert(
                    path.clone(),
                    ParseCacheEntry {
                        hash: current_hash.clone(),
                        analyzed: entry.analyzed.clone(),
                    },
                );
                files_reused += 1;
                continue;
            }
        }

        // Cache miss (new file or changed content): parse.
        let analyzed = analyze(path, source);
        new_cache.insert(
            path.clone(),
            ParseCacheEntry {
                hash: current_hash.clone(),
                analyzed: analyzed.clone(),
            },
        );
        analyzed_map.insert(path.clone(), analyzed);
        files_parsed += 1;
    }
    // Files not in `files` are naturally excluded from both maps — deleted
    // files are dropped without any explicit cleanup step.

    // ── Python sources (`.py`) → the Python analyzer + its own resolution world. ──
    //
    // Each `.py` file becomes an `AnalyzedFile` via `strata-lang-py` (NOT the TS
    // analyzer or SCIP). They use the SAME incremental parse-cache + hash machinery
    // (path keys are disjoint from the TS set), then `assemble_python` adds the
    // `py`-tagged plane to the graph below. The `AnalyzedFile` shape is identical,
    // so no schema bump is needed (a `.py` entry is just a new cache key).
    let mut py_analyzed_map: BTreeMap<String, AnalyzedFile> = BTreeMap::new();
    let py_hashes = hash_files(&py_files);
    for (path, source) in &py_files {
        let current_hash = py_hashes.get(path).expect("hash_files covers all py files");
        if let Some(entry) = old_cache.get(path) {
            if &entry.hash == current_hash {
                py_analyzed_map.insert(path.clone(), entry.analyzed.clone());
                new_cache.insert(
                    path.clone(),
                    ParseCacheEntry {
                        hash: current_hash.clone(),
                        analyzed: entry.analyzed.clone(),
                    },
                );
                files_reused += 1;
                continue;
            }
        }
        let analyzed = analyze_py(path, source);
        new_cache.insert(
            path.clone(),
            ParseCacheEntry {
                hash: current_hash.clone(),
                analyzed: analyzed.clone(),
            },
        );
        py_analyzed_map.insert(path.clone(), analyzed);
        files_parsed += 1;
    }
    // Persist the Python hashes too, so an unchanged `.py` is reused next index.
    for (path, hash) in py_hashes {
        current_hashes.insert(path, hash);
    }

    // ── C# sources (`.cs`) → the C# analyzer + its own `cs` resolution world. ──
    //
    // Identical machinery to the Python block: each `.cs` file becomes an
    // `AnalyzedFile` via `strata-lang-cs` (NOT the TS analyzer or SCIP), sharing the
    // incremental parse-cache + hash map (path keys disjoint from TS/Python). Then
    // `assemble_csharp` adds the `cs`-tagged plane to the graph below. The
    // `AnalyzedFile` shape is identical, so no schema bump (a `.cs` entry is just a
    // new cache key).
    let mut cs_analyzed_map: BTreeMap<String, AnalyzedFile> = BTreeMap::new();
    let cs_hashes = hash_files(&cs_files);
    for (path, source) in &cs_files {
        let current_hash = cs_hashes.get(path).expect("hash_files covers all cs files");
        if let Some(entry) = old_cache.get(path) {
            if &entry.hash == current_hash {
                cs_analyzed_map.insert(path.clone(), entry.analyzed.clone());
                new_cache.insert(
                    path.clone(),
                    ParseCacheEntry {
                        hash: current_hash.clone(),
                        analyzed: entry.analyzed.clone(),
                    },
                );
                files_reused += 1;
                continue;
            }
        }
        let analyzed = analyze_cs(path, source);
        new_cache.insert(
            path.clone(),
            ParseCacheEntry {
                hash: current_hash.clone(),
                analyzed: analyzed.clone(),
            },
        );
        cs_analyzed_map.insert(path.clone(), analyzed);
        files_parsed += 1;
    }
    // Persist the C# hashes too, so an unchanged `.cs` is reused next index.
    for (path, hash) in cs_hashes {
        current_hashes.insert(path, hash);
    }

    // ── Rust sources (`.rs`) → the Rust analyzer + its own `rust` resolution world. ──
    //
    // Identical machinery to the Python/C# blocks: each `.rs` file becomes an
    // `AnalyzedFile` via `strata-lang-rust` (NOT the TS analyzer or SCIP), sharing
    // the incremental parse-cache + hash map (path keys disjoint from TS/Python/C#).
    // Then `assemble_rust` adds the `rust`-tagged plane to the graph below. The
    // `AnalyzedFile` shape is identical, so no schema bump (a `.rs` entry is just a
    // new cache key).
    let mut rust_analyzed_map: BTreeMap<String, AnalyzedFile> = BTreeMap::new();
    let rust_hashes = hash_files(&rust_files);
    for (path, source) in &rust_files {
        let current_hash = rust_hashes
            .get(path)
            .expect("hash_files covers all rust files");
        if let Some(entry) = old_cache.get(path) {
            if &entry.hash == current_hash {
                rust_analyzed_map.insert(path.clone(), entry.analyzed.clone());
                new_cache.insert(
                    path.clone(),
                    ParseCacheEntry {
                        hash: current_hash.clone(),
                        analyzed: entry.analyzed.clone(),
                    },
                );
                files_reused += 1;
                continue;
            }
        }
        let analyzed = analyze_rust(path, source);
        new_cache.insert(
            path.clone(),
            ParseCacheEntry {
                hash: current_hash.clone(),
                analyzed: analyzed.clone(),
            },
        );
        rust_analyzed_map.insert(path.clone(), analyzed);
        files_parsed += 1;
    }
    // Persist the Rust hashes too, so an unchanged `.rs` is reused next index.
    for (path, hash) in rust_hashes {
        current_hashes.insert(path, hash);
    }

    // ── GraphQL operation documents (`.graphql`/`.gql`) → the doc analyzer. ──
    //
    // A `.graphql` file that `GraphqlAdapter::detects` calls a *schema* is a spec
    // (handled by `extract_repo_operations` below) and produces NO AnalyzedFile.
    // Every other `.graphql`/`.gql` file is an executable operation document: it
    // becomes a one-`GqlDocument` AnalyzedFile keyed by its repo-relative path, so
    // `assemble_graph` gives it a Module node and the cache/incremental machinery
    // applies (brief §2). These are NOT fed to the TS analyzer or to SCIP.
    let graphql_docs = collect_graphql_docs(repo_path, Arc::clone(&vendored))?;
    let graphql_hashes = hash_files(&graphql_docs);
    for (path, content) in &graphql_docs {
        // A schema contributes operations, not a consumer doc — skip it here.
        if is_graphql_schema(path, content) {
            continue;
        }
        let current_hash = graphql_hashes
            .get(path)
            .expect("hash_files covers all graphql docs");
        if let Some(entry) = old_cache.get(path) {
            if &entry.hash == current_hash {
                analyzed_map.insert(path.clone(), entry.analyzed.clone());
                new_cache.insert(
                    path.clone(),
                    ParseCacheEntry {
                        hash: current_hash.clone(),
                        analyzed: entry.analyzed.clone(),
                    },
                );
                files_reused += 1;
                continue;
            }
        }
        let analyzed = doc_analyze(content);
        new_cache.insert(
            path.clone(),
            ParseCacheEntry {
                hash: current_hash.clone(),
                analyzed: analyzed.clone(),
            },
        );
        analyzed_map.insert(path.clone(), analyzed);
        files_parsed += 1;
    }
    // Persist the doc hashes too, so an unchanged doc is reused next index. (A
    // doc that became a spec, or whose content changed, is handled by its hash.)
    for (path, hash) in graphql_hashes {
        current_hashes.insert(path, hash);
    }

    // ── Precise resolution (mode-gated, cached). `On` propagates a failure. ──
    // Only TS/JS `files` reach SCIP; GraphQL docs are never passed to it.
    let scip_outcome = resolve_scip(repo_path, &files, options)?;

    let (mut graph, coverage) = assemble_with_coverage(
        &analyzed_map,
        repo_name,
        &opts,
        scip_outcome.resolver.as_ref(),
        &files,
    );

    // ── Python plane: add the `py`-tagged Module/symbol nodes + band-disciplined
    // call/import edges to the SAME graph, linking within Python's own resolution
    // world (no cross-language edges this slice). ──
    assemble_python(&mut graph, repo_name, &py_analyzed_map);

    // ── C# plane: likewise add the `cs`-tagged Module/symbol nodes +
    // band-disciplined call/using edges, linking within C#'s own resolution world
    // (no cross-language edges this slice; the `cs` UID tag keeps it disjoint from
    // the ts/py planes in a mixed repo). ──
    assemble_csharp(&mut graph, repo_name, &cs_analyzed_map);

    // ── Rust plane: likewise add the `rust`-tagged Module/symbol nodes +
    // band-disciplined call/use edges, linking within Rust's own resolution world
    // (no cross-language edges this slice; the `rust` UID tag keeps it disjoint from
    // the ts/py/cs planes in a mixed repo). ──
    assemble_rust(&mut graph, repo_name, &rust_analyzed_map);

    // The combined analyzed-file set (TS/JS + GraphQL docs + Python + C# + Rust). The
    // infra plane's `Runs` handler match is the one place that must see Python (and
    // potentially other) module paths — a Lambda whose handler resolves to a `.py`
    // file links to its Python `Module` node (the EARNED Python flip). **C# and Rust
    // are included for completeness of the code graph, but neither earns a `Runs`
    // edge this slice:** a C# Lambda `Handler` is an `Assembly::Namespace.Type::Method`
    // string (not a file path), and a Rust (cargo-lambda) handler maps to a Cargo
    // *binary name* (from `Cargo.toml`'s `[[bin]]`/`package.name`), not a `.rs` file
    // path. So neither `cs` nor `rs` is in `HANDLER_EXTS`, and such a handler stays
    // `lambdas_handler_unresolved` (resolving it needs csproj/assembly or Cargo
    // bin-name mapping, deferred — see `infra.rs`). The contract plane is fed this
    // combined map too: **Python** now contributes producers/consumers (Flask/
    // FastAPI/Django routes, `requests`/`httpx` calls, `gql` documents, and
    // Graphene/Strawberry/Ariadne resolvers) that link exactly like TS/JS; C# and
    // Rust emit no contract signals yet, so they add nothing.
    let combined_analyzed: BTreeMap<String, AnalyzedFile> = analyzed_map
        .iter()
        .chain(py_analyzed_map.iter())
        .chain(cs_analyzed_map.iter())
        .chain(rust_analyzed_map.iter())
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    // ── Contract plane: detect specs, extract operations, link producers. ──
    // A spec that fails to parse is skipped with whatever diagnostic the adapter
    // gives; other specs and the whole code graph are unaffected (R2).
    let operations = extract_repo_operations(repo_path, Arc::clone(&vendored))?;
    contract::build_contract_plane(&mut graph, repo_name, &combined_analyzed, &operations);

    // ── Infra plane: detect CFN/SAM templates, build nodes/wiring/Runs + the
    // money link. Runs AFTER the contract plane so the `GraphqlField` nodes the
    // money link points at already exist. A malformed template is skipped (R2) but
    // its (path, error) is recorded so the skip is VISIBLE, not silent. ──
    let (templates, mut infra_diagnostics) =
        extract_repo_templates(repo_path, Arc::clone(&vendored))?;
    // `combined_analyzed` (TS + Python) is the existence oracle for the `Runs`
    // handler match, so a Lambda with a Python handler now links to its `.py`
    // Module node instead of counting as unresolved (the EARNED flip).
    let mut infra_link =
        infra::build_infra_plane(&mut graph, repo_name, &templates, &combined_analyzed);
    // `build_infra_plane` only sees the templates that parsed; the failures are
    // counted here from the diagnostics collected during extraction.
    infra_link.templates_failed = infra_diagnostics.len();

    // ── Terragrunt structural plane (Track D1, M2): detect `terragrunt.hcl`
    // units and wire the unit→unit `dependency` edges (structural config_path
    // only — NO function/output evaluation). A malformed unit is skipped but its
    // (path, error) is recorded alongside the infra diagnostics so the skip is
    // visible. ──
    let (units, mut tg_diagnostics) = extract_repo_terragrunt(repo_path, Arc::clone(&vendored))?;
    let terragrunt = infra::build_terragrunt_plane(&mut graph, repo_name, &units);
    infra_diagnostics.append(&mut tg_diagnostics);
    let infra_diagnostics = cap_diagnostics(infra_diagnostics);

    // ── Data plane (Slice 16, D3): detect `.sql` schema files, build the
    // Table/Column nodes + HasColumn/ForeignKey edges, AND (M2) the code→table
    // `Reads`/`Writes` edges from each code file's SQL string literals. It runs
    // LAST — AFTER the code planes — so the code symbol/module nodes a `Reads`/
    // `Writes` edge targets already exist. A malformed `.sql` is skipped (R2) but
    // its (path, error) is recorded so the skip is VISIBLE, not silent. ──
    let (schemas, data_diagnostics) = extract_repo_schemas(repo_path, Arc::clone(&vendored))?;
    // The code SQL candidates, tagged with the language plane (`ts`/`py`/`cs`/`rust`)
    // each file's code nodes were built under, so the data plane reconstructs the
    // right enclosing code-node UID. A file with no SQL literals contributes an empty
    // candidate slice (no edges), so a SQL-free repo is unaffected.
    let code_sql = collect_code_sql(
        &analyzed_map,
        &py_analyzed_map,
        &cs_analyzed_map,
        &rust_analyzed_map,
    );
    // The code ORM hints (explicit-table-name model classes), tagged with the language
    // plane each file's code nodes were built under — the M2b model→table `MapsTo`
    // input (Slice 25). A file with no ORM models contributes an empty slice (no
    // edges), so an ORM-free repo is unaffected.
    let code_orm = collect_code_orm(
        &analyzed_map,
        &py_analyzed_map,
        &cs_analyzed_map,
        &rust_analyzed_map,
    );
    let mut data_link =
        data::build_data_plane(&mut graph, repo_name, &schemas, &code_sql, &code_orm);
    // `build_data_plane` only sees the schemas that parsed; the failures are counted
    // here from the diagnostics collected during extraction (mirrors infra).
    data_link.schemas_failed = data_diagnostics.len();
    let data_diagnostics = cap_diagnostics(data_diagnostics);

    let stats = IndexStats {
        // Includes TS/JS sources, the GraphQL operation documents that became
        // AnalyzedFiles (schemas, which contribute operations not docs, are not
        // counted here), the Python sources, the C# sources, AND the Rust sources.
        files_indexed: analyzed_map.len()
            + py_analyzed_map.len()
            + cs_analyzed_map.len()
            + rust_analyzed_map.len(),
        nodes: graph.node_count(),
        edges: graph.edge_count(),
        files_parsed,
        files_reused,
        resolution_mode: options.resolve_mode,
        degraded: scip_outcome.degraded,
        sites_total: coverage.sites_total,
        sites_resolved: coverage.sites_resolved,
        sites_heuristic: coverage.sites_heuristic,
        sites_ambiguous: coverage.sites_ambiguous,
        infra_link,
        terragrunt,
        infra_diagnostics,
        data_link,
        data_diagnostics,
    };

    store.save_graph(&graph)?;
    store.save_file_hashes(&current_hashes)?;
    store.save_parse_cache(&new_cache)?;

    // Hot-reload change signal (Track E3): write the stamp LAST, after every
    // persist above has returned, so a reader keying off the stamp only learns
    // of the new graph once it is fully written. The stamp is a sidecar file in
    // `<repo>/.strata/` — it does not touch the db connection `store` still
    // holds; the reader's own db open is fail-fast under a writer's lock
    // (DuckDB returns a conflicting-lock error rather than blocking), so a
    // reader that races a still-in-flight reindex degrades safely. A stamp-write
    // failure must not fail an otherwise-successful index (the served graph just
    // keeps using its previous staleness signal), so it is logged, not
    // propagated.
    let strata_dir = repo_path.join(".strata");
    if let Err(e) = stamp::IndexStamp::new(stats.nodes, stats.edges).write(&strata_dir) {
        eprintln!(
            "[index] warning: could not write hot-reload stamp {}: {e}",
            strata_dir.join(stamp::STAMP_FILE).display()
        );
    }

    Ok(stats)
}

/// Walk `repo_path` (gitignore-aware) and collect TS/JS source files into a map
/// of `/`-normalized, repo-relative path -> source text.
fn collect_sources(
    repo_path: &Path,
    vendored: Arc<HashSet<PathBuf>>,
) -> Result<BTreeMap<String, String>, IndexError> {
    let extensions = TsAnalyzer.extensions();
    let mut files = BTreeMap::new();

    // `require_git(false)` makes `.gitignore` files take effect even when the
    // repo is not (yet) a git checkout — the brief's fixture is a plain temp
    // dir. `.git` is always skipped regardless. `.strataignore` (gitignore syntax)
    // is an additional user-controlled exclude list; `vendored` is a per-FILE prune
    // of the exact files a dist-info RECORD installed (see `discover_vendored_paths`),
    // and `*.dist-info` metadata dirs are pruned wholesale by name (they carry no
    // first-party source). Other directories are descended so a name-colliding
    // first-party file the RECORD does not list is still reached and indexed.
    for entry in ignore::WalkBuilder::new(repo_path)
        .require_git(false)
        .add_custom_ignore_filename(".strataignore")
        .filter_entry(move |e| !is_dist_info_dir(e) && !vendored.contains(e.path()))
        .build()
    {
        let entry = entry.map_err(|e| IndexError::Io {
            path: repo_path.display().to_string(),
            source: e
                .into_io_error()
                .unwrap_or_else(|| std::io::Error::other("walk error")),
        })?;
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let path = entry.path();
        if !has_supported_extension(path, extensions) {
            continue;
        }
        let Some(rel) = relative_key(repo_path, path) else {
            continue;
        };
        let source = std::fs::read_to_string(path).map_err(|e| IndexError::Io {
            path: path.display().to_string(),
            source: e,
        })?;
        files.insert(rel, source);
    }

    Ok(files)
}

/// Directory names that are NEVER walked for Python sources, regardless of
/// `.gitignore`. These are virtualenv / dependency / cache roots whose `.py`
/// files are not first-party code (belt-and-suspenders beyond gitignore, the
/// same spirit as excluding `node_modules` for the TS plane). A pragmatic,
/// name-based skip: any path component equal to one of these prunes the subtree.
const PY_SKIP_DIRS: [&str; 5] = [
    "__pycache__",
    "venv",
    ".venv",
    "site-packages",
    "node_modules",
];

/// Walk `repo_path` (gitignore-aware) and collect Python source files into a map
/// of `/`-normalized, repo-relative path -> source text.
///
/// Mirrors [`collect_sources`] but for `.py`/`.pyi` and with the extra
/// [`PY_SKIP_DIRS`] pruning: a `.py` under `__pycache__/`, `venv/`, `.venv/`,
/// `site-packages/`, or `node_modules/` is excluded even when no `.gitignore`
/// covers it. `*.pyc` (compiled bytecode) is excluded for free by the
/// extension filter. The Python analyzer's extension set is the source of truth
/// for which extensions count.
fn collect_python_sources(
    repo_path: &Path,
    vendored: Arc<HashSet<PathBuf>>,
) -> Result<BTreeMap<String, String>, IndexError> {
    collect_lang_sources(repo_path, PyAnalyzer.extensions(), &PY_SKIP_DIRS, vendored)
}

/// Directory names that are NEVER walked for C# sources, regardless of
/// `.gitignore`: the .NET build-output and dependency roots (`bin/`, `obj/`,
/// `packages/`) and the Visual Studio metadata dir (`.vs/`). Their `.cs` files are
/// generated/third-party, not first-party code — belt-and-suspenders beyond
/// gitignore, the same spirit as excluding `node_modules`/`__pycache__`.
const CS_SKIP_DIRS: [&str; 4] = ["bin", "obj", "packages", ".vs"];

/// Walk `repo_path` (gitignore-aware) and collect C# source files into a map of
/// `/`-normalized, repo-relative path -> source text.
///
/// Mirrors [`collect_python_sources`] but for `.cs`/`.csx` and with the
/// [`CS_SKIP_DIRS`] pruning: a `.cs` under `bin/`, `obj/`, `packages/`, or `.vs/`
/// is excluded even when no `.gitignore` covers it (the runtime-decoy guard).
fn collect_csharp_sources(
    repo_path: &Path,
    vendored: Arc<HashSet<PathBuf>>,
) -> Result<BTreeMap<String, String>, IndexError> {
    collect_lang_sources(repo_path, CsAnalyzer.extensions(), &CS_SKIP_DIRS, vendored)
}

/// Directory names that are NEVER walked for Rust sources, regardless of
/// `.gitignore`. **`target/` is CRITICAL** — it is Cargo's build-output dir, which
/// in any real Rust repo (including StrataGraph's own 12-crate workspace) holds an
/// enormous tree of compiled `.rs` artifacts (build scripts' `OUT_DIR` output,
/// dependency source copies). Indexing it would balloon the graph with non-
/// first-party code; pruning it by name (belt-and-suspenders beyond gitignore, the
/// same spirit as `node_modules`/`bin`/`obj`) keeps a self-index tractable.
const RS_SKIP_DIRS: [&str; 1] = ["target"];

/// Walk `repo_path` (gitignore-aware) and collect Rust source files into a map of
/// `/`-normalized, repo-relative path -> source text.
///
/// Mirrors [`collect_csharp_sources`] but for `.rs` and with the [`RS_SKIP_DIRS`]
/// pruning: a `.rs` under `target/` is excluded even when no `.gitignore` covers it
/// (the runtime-decoy guard — the huge-build-dir prune that makes self-indexing
/// viable).
fn collect_rust_sources(
    repo_path: &Path,
    vendored: Arc<HashSet<PathBuf>>,
) -> Result<BTreeMap<String, String>, IndexError> {
    collect_lang_sources(
        repo_path,
        RustAnalyzer.extensions(),
        &RS_SKIP_DIRS,
        vendored,
    )
}

/// Shared gitignore-aware walker for a non-TS language plane: collect files with
/// one of `extensions`, pruning any directory whose name is in `skip_dirs`
/// (belt-and-suspenders beyond `.gitignore`). Both [`collect_python_sources`] and
/// [`collect_csharp_sources`] delegate here so the skip-dir pruning is identical.
fn collect_lang_sources(
    repo_path: &Path,
    extensions: &[&str],
    // `'static` because `WalkBuilder::filter_entry` requires a `'static` closure;
    // the callers pass the `'static` `PY_SKIP_DIRS` / `CS_SKIP_DIRS` consts.
    skip_dirs: &'static [&'static str],
    // Detected dependency-bundle directories (absolute paths) to prune in addition
    // to the name-based `skip_dirs`; see `discover_vendored_paths`. An `Arc` so the
    // `'static` `filter_entry` closure can own a handle to it.
    vendored: Arc<HashSet<PathBuf>>,
) -> Result<BTreeMap<String, String>, IndexError> {
    let mut files = BTreeMap::new();

    for entry in ignore::WalkBuilder::new(repo_path)
        .require_git(false)
        .add_custom_ignore_filename(".strataignore")
        // Prune any directory whose name is a skip-dir, every `*.dist-info` metadata
        // dir wholesale (it holds no first-party source), and — per FILE — any path
        // a dist-info RECORD lists as installed (see `discover_vendored_paths`). A
        // non-skip, non-dist-info directory is DESCENDED so a name-colliding
        // first-party file the RECORD does not list is reached and indexed (the
        // never-lose-first-party guarantee), not pruned by directory name.
        .filter_entry(move |e| {
            if is_dist_info_dir(e) || vendored.contains(e.path()) {
                return false;
            }
            let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if is_dir {
                if let Some(name) = e.file_name().to_str() {
                    return !skip_dirs.contains(&name);
                }
            }
            true
        })
        .build()
    {
        let entry = entry.map_err(|e| IndexError::Io {
            path: repo_path.display().to_string(),
            source: e
                .into_io_error()
                .unwrap_or_else(|| std::io::Error::other("walk error")),
        })?;
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let path = entry.path();
        if !has_supported_extension(path, extensions) {
            continue;
        }
        let Some(rel) = relative_key(repo_path, path) else {
            continue;
        };
        let source = std::fs::read_to_string(path).map_err(|e| IndexError::Io {
            path: path.display().to_string(),
            source: e,
        })?;
        files.insert(rel, source);
    }

    Ok(files)
}

/// Whether a walked entry is a `*.dist-info` directory — a wheel-install metadata
/// dir that carries only `RECORD`/`METADATA`/etc., never first-party source. The
/// collectors prune it wholesale (stop descent) by this name check, so its dozens
/// of metadata files never need to be enumerated into the vendored set. Cheap, and
/// safe because a first-party tree never legitimately ships a `*.dist-info` dir
/// (unlike `*.egg-info`, which is why only `.dist-info` is the vendoring marker).
fn is_dist_info_dir(entry: &ignore::DirEntry) -> bool {
    entry.file_type().map(|t| t.is_dir()).unwrap_or(false)
        && entry
            .file_name()
            .to_str()
            .is_some_and(|n| n.ends_with(".dist-info"))
}

/// Pre-scan `repo_path` for committed third-party dependency *bundles* and return
/// the exact **set of FILE paths** to prune from every plane — never a directory
/// name. Each path is `repo_path`-rooted (the form the collectors' walkers yield),
/// so a `vendored.contains(entry.path())` lookup in `filter_entry` matches with no
/// canonicalization (path-consistency: the set and the lookup share one form).
///
/// The signal is a `*.dist-info` directory — the metadata a wheel install (`pip
/// install -t .`, the AWS Lambda bundling anti-pattern) writes next to the package
/// directories it installed. Its `RECORD` (always present) lists the exact files the
/// install wrote, so each is resolved to `parent.join(entry)` and added **only when
/// it exists as a real file**. That installed-file precision is what closes the
/// first-party-loss bound: a co-located first-party `utils/foo.py` that no `RECORD`
/// lists is NOT in the set, so it survives even when a vendored `utils/__init__.py`
/// (a generic top-level the wheel did install) name-collides with it in the same
/// parent — the never-lose-first-party guarantee. (The whole `*.dist-info` dir is
/// pruned too, but by the collectors' name check, not by enumerating its files.)
///
/// A missing/unreadable `RECORD`, or an out-of-tree entry (absolute `/…`, or `../`
/// script), prunes **nothing extra** — only the dist-info dir itself (handled by the
/// name check), never a guessed-at sibling. That conservative fallback means a
/// vendored file absent from its `RECORD` (rare) is not pruned: inflation, never
/// first-party loss.
///
/// Only `*.dist-info` is treated as a vendoring marker; `*.egg-info` is deliberately
/// ignored, because a first-party source tree legitimately contains its *own*
/// `*.egg-info` build metadata next to first-party code. The `.strataignore` escape
/// valve covers the rare legacy-egg case.
///
/// Gitignore- and `.strataignore`-aware, and it does not descend into the name-based
/// skip dirs — so an already-excluded `site-packages/` is never re-scanned.
fn discover_vendored_paths(repo_path: &Path) -> Result<HashSet<PathBuf>, IndexError> {
    let mut vendored = HashSet::new();

    for entry in ignore::WalkBuilder::new(repo_path)
        .require_git(false)
        .add_custom_ignore_filename(".strataignore")
        .filter_entry(|e| {
            // The scan only needs directory structure; never descend into `.git` or
            // the dependency / build-output roots the collectors already prune.
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                if let Some(name) = e.file_name().to_str() {
                    return name != ".git"
                        && !PY_SKIP_DIRS.contains(&name)
                        && !CS_SKIP_DIRS.contains(&name)
                        && !RS_SKIP_DIRS.contains(&name);
                }
            }
            true
        })
        .build()
    {
        let entry = entry.map_err(|e| IndexError::Io {
            path: repo_path.display().to_string(),
            source: e
                .into_io_error()
                .unwrap_or_else(|| std::io::Error::other("walk error")),
        })?;
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let dir = entry.path();
        let is_dist_info = dir
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.ends_with(".dist-info"));
        if !is_dist_info {
            continue;
        }
        let Some(parent) = dir.parent() else {
            continue;
        };
        // Each `RECORD` entry → the installed FILE path, added only when it really
        // exists. Resolving to a concrete file (not a directory name) is the precise
        // prune: a same-named first-party file the RECORD does not list is left out,
        // so it survives. A missing/unreadable RECORD adds nothing here — only the
        // dist-info dir is pruned (by the collectors' name check), never a sibling.
        for rel in recorded_files(dir) {
            let installed = parent.join(rel);
            if installed.is_file() {
                vendored.insert(installed);
            }
        }
    }

    Ok(vendored)
}

/// The repo-relative file paths a `*.dist-info` recorded as installed, read from its
/// `RECORD` (the wheel install manifest — always present). Each is the first CSV
/// field (`path,hash,size`), so it names one installed file under the dist-info's
/// parent. `top_level.txt` is intentionally NOT used: it lists *top-level package
/// names*, not files, and pruning by those names is the imprecise directory-name
/// rule this fix removes. Out-of-tree entries (absolute, or `../` scripts) are
/// ignored. A missing or unreadable `RECORD` yields an empty set — the conservative
/// fallback prunes only the dist-info dir itself, never a guessed-at sibling.
fn recorded_files(dist_info: &Path) -> Vec<String> {
    let mut files = Vec::new();

    // `RECORD`: CSV `path,hash,size`; the first field is one installed file path.
    if let Ok(text) = std::fs::read_to_string(dist_info.join("RECORD")) {
        for line in text.lines() {
            let Some(path) = line.split(',').next() else {
                continue;
            };
            let path = path.trim();
            if path.is_empty() || path.starts_with('/') || path.starts_with("..") {
                continue;
            }
            files.push(path.to_string());
        }
    }

    files
}

/// Detect contract spec files under `repo_path` and extract their operations
/// into one flat, deterministically-ordered vec.
///
/// Walks the repo (gitignore-aware) for candidate files, runs each adapter's
/// cheap `detects`, and `extract`s the ones that look like specs:
/// - OpenAPI/Swagger over `.json`/`.yaml`/`.yml` → `ApiOperation`s.
/// - GraphQL SDL over `.graphql`/`.gql` → `GraphqlField`s. A `.graphql` file that
///   is an *operation document* (not a schema) is NOT a spec — `detects` gates it
///   out, and it is instead routed to the consumer doc analyzer (brief §2).
/// - protobuf/gRPC over `.proto` → `ApiOperation`s (one per `rpc`). A `.proto`
///   with only `message`s/`enum`s declares no rpcs and so contributes nothing
///   (honest: detected as proto, zero operations).
///
/// A spec that fails to parse is **skipped** (graceful degradation, R2) — the
/// rest of the estate/repo still indexes. Spec paths are repo-relative and
/// `/`-normalized so the resulting operation UIDs are stable. OpenAPI specs are
/// emitted first (in path order), then GraphQL schemas, then gRPC services — a
/// fixed order so the resulting node/edge set is deterministic.
pub(crate) fn extract_repo_operations(
    repo_path: &Path,
    // Detected dependency-bundle files to prune (Fix B); see `discover_vendored_paths`.
    // An empty set re-includes everything (`--include-vendored`, or the estate's
    // already-indexed repos where pruning happened at index time).
    vendored: Arc<HashSet<PathBuf>>,
) -> Result<Vec<strata_contract::OperationDef>, IndexError> {
    use strata_contract::{ContractAdapter, GraphqlAdapter, OpenApiAdapter, ProtoAdapter};

    let mut ops = Vec::new();

    // ── OpenAPI/Swagger (.json/.yaml/.yml). ──
    const OPENAPI_EXTS: [&str; 3] = ["json", "yaml", "yml"];
    let openapi = OpenApiAdapter;
    for (rel, content) in collect_spec_candidates(repo_path, &OPENAPI_EXTS, Arc::clone(&vendored))?
    {
        if !openapi.detects(&rel, &content) {
            continue;
        }
        match openapi.extract(&rel, &content) {
            Ok(mut extracted) => ops.append(&mut extracted),
            // R2: a malformed spec is skipped; the code graph is intact.
            Err(_e) => continue,
        }
    }

    // ── GraphQL SDL (.graphql/.gql). Only *schemas* are specs; operation
    // documents are gated out by `detects` and handled as consumer docs. ──
    const GRAPHQL_EXTS: [&str; 2] = ["graphql", "gql"];
    let graphql = GraphqlAdapter;
    for (rel, content) in collect_spec_candidates(repo_path, &GRAPHQL_EXTS, Arc::clone(&vendored))?
    {
        if !graphql.detects(&rel, &content) {
            continue;
        }
        match graphql.extract(&rel, &content) {
            Ok(mut extracted) => ops.append(&mut extracted),
            // R2: a malformed schema is skipped; the code graph is intact.
            Err(_e) => continue,
        }
    }

    // ── protobuf/gRPC (.proto). One operation per `rpc`; a `.proto` of only
    // messages/enums detects as proto but yields nothing (honest, R1). ──
    const PROTO_EXTS: [&str; 1] = ["proto"];
    let grpc = ProtoAdapter;
    for (rel, content) in collect_spec_candidates(repo_path, &PROTO_EXTS, Arc::clone(&vendored))? {
        if !grpc.detects(&rel, &content) {
            continue;
        }
        match grpc.extract(&rel, &content) {
            Ok(mut extracted) => ops.append(&mut extracted),
            // R2: a malformed .proto is skipped; the code graph is intact.
            Err(_e) => continue,
        }
    }

    Ok(ops)
}

/// Detect CloudFormation/SAM templates under `repo_path` and extract them into one
/// path-ordered vec of [`InfraTemplate`](strata_infra::InfraTemplate)s.
///
/// Walks the repo (gitignore-aware) for `.yaml`/`.yml`/`.json`/`.template`
/// candidates, runs [`CfnSamAdapter::detects`](strata_infra::CfnSamAdapter), and
/// `extract`s the ones that look like templates. A template that fails to parse is
/// **skipped** (graceful degradation, R2) — the code/contract planes are intact.
///
/// **Cross-plane precedence (brief §integration).** A file the contract plane
/// already claims (an OpenAPI/Swagger spec) is excluded *first* so a `.yaml`/
/// `.json` can never be double-claimed. In practice the two `detects` are mutually
/// exclusive — a CFN template (top-level `Resources` map of `AWS::…` types) has no
/// `openapi`/`swagger`/`paths` root, and an OpenAPI doc has no `Resources` map —
/// but the explicit exclusion makes the precedence a guarantee, not a coincidence.
/// (GraphQL SDL lives in `.graphql`/`.gql`, a disjoint extension set, so it needs
/// no exclusion.)
pub(crate) fn extract_repo_templates(
    repo_path: &Path,
    vendored: Arc<HashSet<PathBuf>>,
) -> Result<(Vec<strata_infra::InfraTemplate>, Vec<String>), IndexError> {
    use strata_contract::{ContractAdapter, OpenApiAdapter};
    use strata_infra::{
        dedup_plan_over_hcl, extract_plan, is_plan_json, CfnDetection, CfnSamAdapter, IacAdapter,
        TerraformAdapter,
    };

    const TEMPLATE_EXTS: [&str; 4] = ["yaml", "yml", "json", "template"];
    let cfn = CfnSamAdapter;
    let openapi = OpenApiAdapter;

    let mut templates = Vec::new();
    // Terraform plan-JSON is the PREFERRED source (resolved over raw); collected
    // separately so it can dedup-supersede the HCL-parsed `.tf` resources by
    // address below.
    let mut plan_templates = Vec::new();
    let mut diagnostics = Vec::new();
    for (rel, content) in collect_spec_candidates(repo_path, &TEMPLATE_EXTS, Arc::clone(&vendored))?
    {
        // Precedence: a contract-plane (OpenAPI) spec is claimed there, never by
        // infra. A CFN template is not OpenAPI anyway, so this only matters
        // defensively.
        if openapi.detects(&rel, &content) {
            continue;
        }
        // A Terraform plan/state JSON (`terraform show -json`) is the resolved
        // source: route it to the plan set FIRST (a plan has no `Resources` map, so
        // CFN's `detect_kind` would call it `NotCfn` and drop it).
        if is_plan_json(&content) {
            match extract_plan(&rel, &content) {
                Ok(t) => plan_templates.push(t),
                Err(e) => diagnostics.push(format!("{rel}: {e}")),
            }
            continue;
        }
        // `detect_kind` (not the boolean `detects`) so a file that *looks* like a
        // CFN template but won't parse is surfaced as a failure rather than
        // silently skipped — the exact regression that lost the 8k-line template.
        match cfn.detect_kind(&content) {
            CfnDetection::Template => match cfn.extract(&rel, &content) {
                Ok(template) => templates.push(template),
                // Defensive: `Template` means it parsed in `detect_kind`, so
                // `extract` re-parsing should also succeed; record any surprise.
                Err(e) => diagnostics.push(format!("{rel}: {e}")),
            },
            // R2: a malformed template is skipped (the rest of the repo still
            // indexes), but NO LONGER silently — the (path, error) is recorded so
            // the CLI prints it and coverage counts it (`templates_failed`).
            CfnDetection::Malformed(msg) => diagnostics.push(format!("{rel}: parse error: {msg}")),
            // Not a CFN template at all — skip without noise.
            CfnDetection::NotCfn => continue,
        }
    }

    // ── Terraform / OpenTofu `.tf`/`.tofu` configs (Track D1). Walked with the
    // `.terraform/` skip (downloaded modules — the TF analogue of node_modules), so
    // a vendored module's resources never pollute the graph. Each detected config is
    // extracted by the `TerraformAdapter` and flows through the SAME path as CFN. ──
    let tf = TerraformAdapter;
    let mut hcl_templates = Vec::new();
    for (rel, content) in collect_terraform_sources(repo_path, vendored)? {
        if !tf.detects(&rel, &content) {
            continue; // a `.tf` with no resource/data/module block (e.g. variables.tf only).
        }
        match tf.extract(&rel, &content) {
            Ok(t) => hcl_templates.push(t),
            // R2: a malformed `.tf` is skipped but surfaced (path + error), like CFN.
            Err(e) => diagnostics.push(format!("{rel}: {e}")),
        }
    }

    // Plan-JSON resources supersede HCL-parsed ones by address (resolved over raw);
    // HCL configs with no plan counterpart keep their resources. The merged TF set
    // joins the CFN templates in one vec for `build_infra_plane`.
    templates.extend(dedup_plan_over_hcl(hcl_templates, plan_templates));

    Ok((templates, diagnostics))
}

/// Directory names NEVER walked for Terraform sources, regardless of `.gitignore`:
/// `.terraform/` holds provider plugins and DOWNLOADED modules (the TF analogue of
/// `node_modules`), whose `.tf` files are third-party, not first-party config.
const TF_SKIP_DIRS: [&str; 1] = [".terraform"];

/// The Terraform / OpenTofu source extensions routed to the [`TerraformAdapter`].
/// `.tf` is HCL config; `.tofu` is the OpenTofu equivalent. (`.tf.json` — the JSON
/// HCL variant — is intentionally not handled here; it is rare and overlaps the
/// plan-JSON detection space.)
const TF_EXTS: [&str; 2] = ["tf", "tofu"];

/// Walk `repo_path` (gitignore-aware) collecting `.tf`/`.tofu` files into a sorted
/// map of repo-relative path → text, pruning [`TF_SKIP_DIRS`]. Reuses the shared
/// language-source walker so the `.terraform/` pruning is identical in spirit to
/// the Python/C# skip-dir guards.
fn collect_terraform_sources(
    repo_path: &Path,
    vendored: Arc<HashSet<PathBuf>>,
) -> Result<BTreeMap<String, String>, IndexError> {
    collect_lang_sources(repo_path, &TF_EXTS, &TF_SKIP_DIRS, vendored)
}

/// Detect `terragrunt.hcl` units under `repo_path` and extract them structurally
/// into a path-ordered vec of [`TerragruntUnit`](strata_infra::TerragruntUnit)s,
/// plus per-unit parse diagnostics. Walks the repo (gitignore-aware) for `.hcl`
/// files, pruning [`TF_SKIP_DIRS`] (`.terraform/`), and keeps only those whose
/// basename is `terragrunt.hcl`. A unit that fails to parse is **skipped** (R2)
/// with its `(path, error)` recorded — never silently dropped.
pub(crate) fn extract_repo_terragrunt(
    repo_path: &Path,
    vendored: Arc<HashSet<PathBuf>>,
) -> Result<(Vec<strata_infra::TerragruntUnit>, Vec<String>), IndexError> {
    use strata_infra::{extract_unit, is_terragrunt_file};

    const HCL_EXTS: [&str; 1] = ["hcl"];
    let mut units = Vec::new();
    let mut diagnostics = Vec::new();
    for (rel, content) in collect_lang_sources(repo_path, &HCL_EXTS, &TF_SKIP_DIRS, vendored)? {
        if !is_terragrunt_file(&rel) {
            continue; // a non-unit `.hcl` (common.hcl, an include) — not a unit file.
        }
        match extract_unit(&rel, &content) {
            Ok(u) => units.push(u),
            Err(e) => diagnostics.push(format!("{rel}: {e}")),
        }
    }
    Ok((units, diagnostics))
}

/// Directory names NEVER walked for SQL schema sources, regardless of `.gitignore`.
/// These are dependency / build-output / virtualenv roots whose `.sql` files (a
/// vendored migration shipped inside a package, a fixture under `node_modules`) are
/// not first-party schema — belt-and-suspenders beyond gitignore, the same spirit
/// as the per-language skip lists. (`bin`/`obj` are .NET's; `target` is Rust's; the
/// rest mirror the Python/TF guards.)
const SQL_SKIP_DIRS: [&str; 9] = [
    "node_modules",
    "__pycache__",
    "venv",
    ".venv",
    "site-packages",
    ".terraform",
    "bin",
    "obj",
    "target",
];

/// The SQL schema file extensions routed to the [`SqlSchemaAdapter`].
const SQL_EXTS: [&str; 1] = ["sql"];

/// Detect SQL DDL/migration files under `repo_path` and extract them into one
/// path-ordered vec of [`SchemaModel`](strata_data::SchemaModel)s, plus per-file
/// parse diagnostics.
///
/// Walks the repo (gitignore-aware) for `.sql` files, pruning [`SQL_SKIP_DIRS`] and
/// vendored dependency bundles, runs [`SqlSchemaAdapter::detects`](strata_data), and
/// `extract`s the ones that declare ≥1 table. A `.sql` that declares no table (a
/// query-only file) is skipped silently. A `.sql` that carries the DDL textual
/// signal but **fails to parse** is **skipped** (graceful degradation, R2) but NO
/// LONGER silently — its `(path, error)` is recorded so the CLI prints it and
/// coverage counts it (`schemas_failed`), the infra `templates_failed` precedent.
pub(crate) fn extract_repo_schemas(
    repo_path: &Path,
    vendored: Arc<HashSet<PathBuf>>,
) -> Result<(Vec<strata_data::SchemaModel>, Vec<String>), IndexError> {
    use strata_data::{looks_like_ddl, SchemaAdapter, SqlSchemaAdapter};

    let adapter = SqlSchemaAdapter;
    let mut schemas = Vec::new();
    let mut diagnostics = Vec::new();
    for (rel, content) in collect_lang_sources(repo_path, &SQL_EXTS, &SQL_SKIP_DIRS, vendored)? {
        match adapter.extract(&rel, &content) {
            // A schema file that declares ≥1 table joins the set; a query-only
            // `.sql` (0 tables) is skipped without noise.
            Ok(model) if !model.tables.is_empty() => schemas.push(model),
            Ok(_) => continue,
            // A parse failure is surfaced ONLY when the file looked like DDL — a
            // non-SQL `.sql` of prose is skipped silently (no false alarm).
            Err(e) => {
                if looks_like_ddl(&content) {
                    diagnostics.push(format!("{rel}: parse error: {e}"));
                }
            }
        }
    }
    Ok((schemas, diagnostics))
}

/// Build the data plane's code→table input: one [`CodeSqlFile`] per source file,
/// tagged with the language plane (`ts`/`py`/`cs`) its code nodes were built under,
/// carrying that file's captured SQL candidates.
///
/// The TS `analyzed_map` also holds `.graphql`/`.gql` operation documents — those
/// have no `sql_candidates`, so they contribute an empty slice (no edges). The
/// borrows are valid because the three maps outlive the `build_data_plane` call.
/// A repo with no SQL literals yields all-empty slices, so the data plane adds no
/// `Reads`/`Writes` edges (additive).
fn collect_code_sql<'a>(
    ts: &'a BTreeMap<String, AnalyzedFile>,
    py: &'a BTreeMap<String, AnalyzedFile>,
    cs: &'a BTreeMap<String, AnalyzedFile>,
    rust: &'a BTreeMap<String, AnalyzedFile>,
) -> Vec<data::CodeSqlFile<'a>> {
    let mut out = Vec::new();
    for (lang, map) in [("ts", ts), ("py", py), ("cs", cs), ("rust", rust)] {
        for (path, file) in map {
            // Skip files with no SQL — keeps the slice small and the intent clear.
            if file.sql_candidates.is_empty() {
                continue;
            }
            out.push(data::CodeSqlFile {
                lang,
                path,
                candidates: &file.sql_candidates,
            });
        }
    }
    out
}

/// Build the data plane's ORM model→table input: one [`CodeOrmFile`](data::CodeOrmFile)
/// per source file, tagged with the language plane (`ts`/`py`/`cs`/`rust`) its code
/// nodes were built under, carrying that file's captured ORM model hints (Slice 25,
/// D3, M2b). The data-plane analogue of [`collect_code_sql`]. A repo whose models
/// declare no explicit table names yields all-empty slices, so the plane adds no
/// `MapsTo` edges (additive). The borrows are valid because the maps outlive the
/// `build_data_plane` call.
fn collect_code_orm<'a>(
    ts: &'a BTreeMap<String, AnalyzedFile>,
    py: &'a BTreeMap<String, AnalyzedFile>,
    cs: &'a BTreeMap<String, AnalyzedFile>,
    rust: &'a BTreeMap<String, AnalyzedFile>,
) -> Vec<data::CodeOrmFile<'a>> {
    let mut out = Vec::new();
    for (lang, map) in [("ts", ts), ("py", py), ("cs", cs), ("rust", rust)] {
        for (path, file) in map {
            // Skip files with no ORM models — keeps the slice small and intent clear.
            if file.orm_models.is_empty() {
                continue;
            }
            out.push(data::CodeOrmFile {
                lang,
                path,
                hints: &file.orm_models,
            });
        }
    }
    out
}

/// Cap a per-template diagnostics vec at [`MAX_INFRA_DIAGNOSTICS`], appending a
/// single `… and N more …` summary line when truncated. The exact failure count
/// always lives in [`InfraLinkCoverage::templates_failed`](strata_infra) /
/// [`IndexStats::infra_link`]; this only bounds the printed text.
fn cap_diagnostics(mut diagnostics: Vec<String>) -> Vec<String> {
    if diagnostics.len() > MAX_INFRA_DIAGNOSTICS {
        let extra = diagnostics.len() - MAX_INFRA_DIAGNOSTICS;
        diagnostics.truncate(MAX_INFRA_DIAGNOSTICS);
        diagnostics.push(format!("… and {extra} more infra template failure(s)"));
    }
    diagnostics
}

/// Walk `repo_path` (gitignore-aware) collecting candidate spec files by the
/// given extension set into a sorted map of repo-relative path → text. Cheap
/// extension filtering keeps us from reading every file; the caller's adapter
/// `detects` then decides which candidates are real specs.
///
/// Vendored-aware: a spec file a dist-info RECORD lists as installed (a CFN/SAM
/// template or an OpenAPI/GraphQL/proto spec committed inside a vendored bundle) is
/// pruned per-FILE, and `*.dist-info` dirs wholesale — the SAME `vendored` guard the
/// code collectors apply, so all planes prune vendored bundles identically (by the
/// dist-info RECORD's exact file set). An empty `vendored` set (`--include-vendored`,
/// or the estate's already-indexed repos) prunes nothing, transparently re-including
/// everything.
fn collect_spec_candidates(
    repo_path: &Path,
    exts: &[&str],
    vendored: Arc<HashSet<PathBuf>>,
) -> Result<BTreeMap<String, String>, IndexError> {
    let mut specs = BTreeMap::new();

    for entry in ignore::WalkBuilder::new(repo_path)
        .require_git(false)
        .filter_entry(move |e| !is_dist_info_dir(e) && !vendored.contains(e.path()))
        .build()
    {
        let entry = entry.map_err(|e| IndexError::Io {
            path: repo_path.display().to_string(),
            source: e
                .into_io_error()
                .unwrap_or_else(|| std::io::Error::other("walk error")),
        })?;
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let path = entry.path();
        if !has_supported_extension(path, exts) {
            continue;
        }
        let Some(rel) = relative_key(repo_path, path) else {
            continue;
        };
        let content = std::fs::read_to_string(path).map_err(|e| IndexError::Io {
            path: path.display().to_string(),
            source: e,
        })?;
        specs.insert(rel, content);
    }

    Ok(specs)
}

/// The GraphQL document file extensions routed to the doc analyzer.
const GRAPHQL_DOC_EXTS: [&str; 2] = ["graphql", "gql"];

/// Walk `repo_path` (gitignore-aware) collecting `.graphql`/`.gql` files into a
/// sorted map of repo-relative path → text. Both schemas and operation documents
/// are collected here; the caller's [`is_graphql_schema`] gate separates them.
/// Vendored-aware (Fix B): a `.graphql`/`.gql` inside a vendored bundle is pruned,
/// so a vendored schema/operation doc never becomes a node.
fn collect_graphql_docs(
    repo_path: &Path,
    vendored: Arc<HashSet<PathBuf>>,
) -> Result<BTreeMap<String, String>, IndexError> {
    collect_spec_candidates(repo_path, &GRAPHQL_DOC_EXTS, vendored)
}

/// Whether a `.graphql`/`.gql` file is a *schema* (SDL), as opposed to an
/// executable operation document. Delegates to the adapter's content-based
/// `detects` — a schema is a spec (handled by [`extract_repo_operations`]); a
/// non-schema is an operation document for the doc analyzer.
fn is_graphql_schema(path: &str, content: &str) -> bool {
    use strata_contract::{ContractAdapter, GraphqlAdapter};
    GraphqlAdapter.detects(path, content)
}

/// Analyze a `.graphql`/`.gql` *operation document* into an [`AnalyzedFile`].
///
/// The lightweight doc analyzer (NOT the TS analyzer): the file's whole content
/// is captured as one module-top-level [`GqlDocument`] (always `interpolation_free`
/// — a standalone document has no host-language interpolations). Everything else
/// is empty, so the assembled graph gives the file a `Module` node and the gql
/// document drives a `CONSUMES` edge from that module (brief §2). A malformed or
/// comment-only document still yields this record; the *linker* parses it and
/// benignly produces no links on a parse failure (R2).
fn doc_analyze(content: &str) -> AnalyzedFile {
    AnalyzedFile {
        gql_documents: vec![strata_core::GqlDocument {
            text: content.to_string(),
            interpolation_free: true,
            // A `.graphql`/`.gql` operation file is explicit GraphQL by extension:
            // a parse failure is an honest miss (counted in `unparsed_documents`).
            tagged: true,
            enclosing_fqn: String::new(),
            span: strata_core::Span::default(),
        }],
        ..AnalyzedFile::default()
    }
}

/// blake3 hex content hash of every file (parallel structure to `files`).
fn hash_files(files: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    files
        .iter()
        .map(|(path, source)| {
            (
                path.clone(),
                blake3::hash(source.as_bytes()).to_hex().to_string(),
            )
        })
        .collect()
}

/// Load `<repo>/tsconfig.json` into [`ResolveOptions`]; default if absent.
fn load_tsconfig(repo_path: &Path) -> Result<ResolveOptions, IndexError> {
    let tsconfig_path = repo_path.join("tsconfig.json");
    match std::fs::read_to_string(&tsconfig_path) {
        Ok(contents) => Ok(tsconfig::parse_tsconfig(&contents)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ResolveOptions::default()),
        Err(e) => Err(IndexError::Io {
            path: tsconfig_path.display().to_string(),
            source: e,
        }),
    }
}

/// The code-plane language tag for a path's extension, matching the `language`
/// component of the symbol's UID (`ts`/`py`/`cs`), or `None` for a path that no
/// code analyzer handles. The single source of truth for "is this a code file,
/// and which analyzer/grammar owns it" — used by both `detect_changes`
/// (plane routing) and `rename` (grammar selection).
pub(crate) fn code_language_of(path: &str) -> Option<&'static str> {
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    if TsAnalyzer.extensions().contains(&ext.as_str()) {
        Some("ts")
    } else if PyAnalyzer.extensions().contains(&ext.as_str()) {
        Some("py")
    } else if CsAnalyzer.extensions().contains(&ext.as_str()) {
        Some("cs")
    } else if RustAnalyzer.extensions().contains(&ext.as_str()) {
        Some("rust")
    } else {
        None
    }
}

/// Analyze a code file's content with the analyzer its extension routes to (the
/// same routing `index_impl` applies per language plane). A non-code path yields
/// an empty [`AnalyzedFile`] (no analyzer claims it). Used by `detect_changes` to
/// re-derive the symbol set of old/new file content.
pub(crate) fn analyze_for_path(path: &str, content: &str) -> AnalyzedFile {
    match code_language_of(path) {
        Some("ts") => analyze(path, content),
        Some("py") => analyze_py(path, content),
        Some("cs") => analyze_cs(path, content),
        Some("rust") => analyze_rust(path, content),
        _ => AnalyzedFile::default(),
    }
}

/// Whether `path`'s extension is one the analyzer supports.
fn has_supported_extension(path: &Path, extensions: &[&str]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|ext| extensions.contains(&ext))
        .unwrap_or(false)
}

/// The repo-relative, `/`-normalized key for `path` under `repo_path`.
fn relative_key(repo_path: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(repo_path).ok()?;
    let mut out = String::new();
    for (i, component) in rel.components().enumerate() {
        let part = component.as_os_str().to_str()?;
        if i > 0 {
            out.push('/');
        }
        out.push_str(part);
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// The repo name used for the `Repo` node and `Uid` package: the final path
/// component, falling back to `"repo"` for unusual paths (e.g. `/`).
///
/// The path is **canonicalized first** (falling back to the raw path if that
/// fails — e.g. the dir does not exist) so a relative invocation yields the real
/// directory name: `strata index .` in `/x/y/myrepo` derives `myrepo`, not the
/// literal `.` (whose `file_name()` is `None` → the useless `"repo"` fallback).
fn repo_name_of(repo_path: &Path) -> String {
    let canonical = std::fs::canonicalize(repo_path).unwrap_or_else(|_| repo_path.to_path_buf());
    canonical
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("repo")
        .to_string()
}

/// The graph context resolved for a repo root: a standalone single-repo db, or
/// membership in an estate (with the manifest path and this repo's
/// estate-qualified name).
///
/// Produced by [`resolve_context`]; consumed by every command/hook that needs
/// to know whether it is inside an estate.
#[derive(Debug, Clone, PartialEq)]
pub enum IndexContext {
    /// A standalone repo: the graph lives at `<repo>/.strata/graph.duckdb`.
    Single { db: PathBuf },
    /// An estate member: the estate manifest and this repo's `name` within it.
    Estate {
        manifest: PathBuf,
        repo: String,
        repo_root: PathBuf,
    },
}

/// Resolve whether `repo_root` is a standalone repo or an estate member by
/// reading and **validating** the `.strata/estate.toml` marker.
///
/// Validation: the marker must be present AND its `manifest` path must parse
/// as a valid [`WorkspaceManifest`] AND the manifest must still list a repo
/// entry whose `name` equals `marker.repo` and whose resolved path
/// (`manifest_dir.join(repo.path).canonicalize()`) equals
/// `repo_root.canonicalize()`. Any failure in that chain — missing marker,
/// stale/deleted manifest, repo no longer listed — silently degrades to
/// [`IndexContext::Single`]. Never panics.
pub fn resolve_context(repo_root: &Path) -> IndexContext {
    let strata = repo_root.join(".strata");
    let single = IndexContext::Single {
        db: strata.join("graph.duckdb"),
    };

    let Some(marker) = estate_marker::read_marker(&strata) else {
        return single;
    };
    let Ok(manifest) = estate::WorkspaceManifest::parse_file(&marker.manifest) else {
        return single;
    };
    let Some(manifest_dir) = marker.manifest.parent() else {
        return single;
    };
    let want = repo_root.canonicalize().ok();
    let listed = manifest
        .repos
        .iter()
        .any(|r| r.name == marker.repo && manifest_dir.join(&r.path).canonicalize().ok() == want);
    if !listed {
        return single;
    }
    IndexContext::Estate {
        manifest: marker.manifest,
        repo: marker.repo,
        repo_root: repo_root
            .canonicalize()
            .unwrap_or_else(|_| repo_root.to_path_buf()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── dogfood fix 1 — test 4: repo_name canonicalization ───────────────────
    //
    // `strata index .` in `/x/y/myrepo` must derive repo_name `myrepo`, not the
    // literal `.` (whose `file_name()` is `None`, which fell back to the useless
    // `"repo"`). A path ending in `.` is the exact shape the CLI passes; it has no
    // `file_name()` until canonicalized to the real directory.

    #[test]
    fn repo_name_of_canonicalizes_traversal_path_to_real_basename() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // `<tmp>/myrepo` is the real repo; `<tmp>/myrepo/inner/..` points back at it
        // but its RAW `file_name()` is `..` — only canonicalization recovers
        // `myrepo`. This pins that canonicalize (not the raw basename) is consulted.
        let repo_dir = tmp.path().join("myrepo");
        std::fs::create_dir(&repo_dir).expect("create myrepo dir");
        std::fs::create_dir(repo_dir.join("inner")).expect("create inner dir");

        let traversal = repo_dir.join("inner").join("..");
        // A path ending in `..` has NO `file_name()` (Rust skips the ParentDir
        // component), so the raw path would hit the useless `"repo"` fallback —
        // exactly the `.`-invocation bug. Only canonicalization recovers `myrepo`.
        assert_eq!(
            traversal.file_name(),
            None,
            "a `..`-suffixed path has no file_name (test premise: raw basename is useless)"
        );
        assert_eq!(
            repo_name_of(&traversal),
            "myrepo",
            "canonicalization must recover the real directory basename, not the `repo` fallback"
        );
    }

    #[test]
    fn repo_name_of_handles_plain_relative_dot_via_cwd() {
        // The literal `strata index .` case: with cwd set to a known directory,
        // `repo_name_of(".")` canonicalizes through the cwd to that dir's basename.
        // Serialized against the other cwd-free tests by only mutating cwd here.
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo_dir = tmp.path().join("myrepo");
        std::fs::create_dir(&repo_dir).expect("create myrepo dir");
        // Canonicalize the expected basename too: on macOS a tempdir lives under a
        // `/var → /private/var` symlink, but the LAST component (`myrepo`) is stable.
        let prev = std::env::current_dir().ok();
        std::env::set_current_dir(&repo_dir).expect("chdir into myrepo");
        let name = repo_name_of(Path::new("."));
        // Restore cwd before asserting so a failure cannot leave a poisoned cwd.
        if let Some(prev) = prev {
            let _ = std::env::set_current_dir(prev);
        }
        assert_eq!(
            name, "myrepo",
            "`repo_name_of(\".\")` must resolve through cwd to the real basename"
        );
    }
}
