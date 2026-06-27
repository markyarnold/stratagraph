//! Concrete [`strata_mcp::GraphReloader`] implementations for the two graph
//! sources the `strata mcp` server serves: a single DuckDB store (`--db`) and a
//! linked estate (`--workspace <manifest>`).
//!
//! These live in `strata-cli` (not `strata-mcp`) on purpose: the MCP crate is
//! kept load-agnostic — it knows the *trait*, the CLI knows how to load. The
//! server's serial loop calls [`GraphReloader::changed`] before each request
//! (cheap, never opens the db) and, when it reports a change,
//! [`GraphReloader::reload`] to get the fresh graph. A failed reload returns
//! `Err`, the server keeps its current graph, and — because the change signal is
//! only advanced on a *successful* reload — the next `changed()` still fires and
//! the reload is retried (degrade-safe).

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use strata_core::Graph;
use strata_index::IndexStamp;
use strata_mcp::GraphReloader;
use strata_store::{DuckGraphStore, GraphStore};

use crate::load_workspace_graph;

/// The cheap, lock-free change signal for one DuckDB store.
///
/// Prefer the indexer's `.strata/index.stamp` bytes (unique per index run); fall
/// back to the db file's `(mtime, len)` when there is no stamp — so a db indexed
/// *before* this feature still hot-reloads (with the during-write-noise caveat
/// the degrade-safe reload absorbs). Equality means "no change since last load".
#[derive(Debug, Clone, PartialEq, Eq)]
enum DbSignal {
    /// The raw stamp bytes (the canonical signal once the indexer wrote one).
    Stamp(Vec<u8>),
    /// Fallback: `(mtime, len)` of the db file, or `None` if it is absent.
    FileMeta(Option<(SystemTime, u64)>),
}

/// Compute the current [`DbSignal`] for `db`. `strata_dir` is the db's parent
/// (the `.strata/` dir) where the stamp lives. Never opens the db.
fn db_signal(db: &Path, strata_dir: &Path) -> DbSignal {
    if let Some(bytes) = IndexStamp::read(strata_dir) {
        return DbSignal::Stamp(bytes);
    }
    let meta = std::fs::metadata(db)
        .ok()
        .map(|m| (m.modified().unwrap_or(SystemTime::UNIX_EPOCH), m.len()));
    DbSignal::FileMeta(meta)
}

/// `.strata/` directory for a canonical `<repo>/.strata/graph.duckdb` db path,
/// i.e. the db's parent; falls back to the db's parent for any layout.
fn strata_dir_of(db: &Path) -> PathBuf {
    db.parent().map(Path::to_path_buf).unwrap_or_default()
}

/// [`GraphReloader`] for a single DuckDB store (`strata mcp --db`).
pub struct SingleDbReloader {
    db: PathBuf,
    strata_dir: PathBuf,
    /// Signal of the graph currently being served (advanced only on a successful
    /// reload).
    last: DbSignal,
}

impl SingleDbReloader {
    /// Build a reloader whose baseline is the db's *current* on-disk signal —
    /// call this right after the initial load so nothing looks stale until the
    /// next reindex.
    pub fn new(db: &Path) -> Self {
        let strata_dir = strata_dir_of(db);
        let last = db_signal(db, &strata_dir);
        SingleDbReloader {
            db: db.to_path_buf(),
            strata_dir,
            last,
        }
    }
}

impl GraphReloader for SingleDbReloader {
    fn changed(&mut self) -> bool {
        db_signal(&self.db, &self.strata_dir) != self.last
    }

    fn reload(&mut self) -> Result<Graph, String> {
        // Snapshot the signal we are about to load *before* opening, so a write
        // that lands during the open is caught by the next `changed()` rather
        // than being silently folded into this load.
        let sig = db_signal(&self.db, &self.strata_dir);
        let store = DuckGraphStore::open(&self.db).map_err(|e| e.to_string())?;
        let graph = store.load_graph().map_err(|e| e.to_string())?;
        // Advance the baseline ONLY on success → a failed reload retries.
        self.last = sig;
        Ok(graph)
    }
}

/// [`GraphReloader`] for a linked estate (`strata mcp --workspace <manifest>`).
///
/// The signal is the manifest's own `(mtime, len)` plus each repo's
/// [`DbSignal`]: a change to the manifest *or* to any repo's index flips it.
/// `reload()` re-links the estate via [`load_workspace_graph`] (per-repo failures
/// already degrade per the existing estate policy; an all-failed load is `Err`).
pub struct WorkspaceReloader {
    manifest: PathBuf,
    last: EstateSignal,
}

/// The combined estate signal: the manifest file meta + per-repo db signals (in
/// manifest order). Any difference means "reload the estate".
#[derive(Debug, Clone, PartialEq, Eq)]
struct EstateSignal {
    manifest_meta: Option<(SystemTime, u64)>,
    repos: Vec<DbSignal>,
}

/// Compute the estate signal for `manifest`. Best-effort: a manifest that fails
/// to parse yields an empty repo list (the manifest-meta change still triggers a
/// reload attempt, which surfaces the parse error through `reload()`).
fn estate_signal(manifest: &Path) -> EstateSignal {
    let manifest_meta = std::fs::metadata(manifest)
        .ok()
        .map(|m| (m.modified().unwrap_or(SystemTime::UNIX_EPOCH), m.len()));

    let mut repos = Vec::new();
    if let Ok(parsed) = strata_index::WorkspaceManifest::parse_file(manifest) {
        let manifest_dir = manifest.parent().unwrap_or(Path::new("."));
        for repo in &parsed.repos {
            let db = manifest_dir
                .join(&repo.path)
                .join(".strata")
                .join("graph.duckdb");
            let strata_dir = strata_dir_of(&db);
            repos.push(db_signal(&db, &strata_dir));
        }
    }
    EstateSignal {
        manifest_meta,
        repos,
    }
}

impl WorkspaceReloader {
    /// Build a reloader baselined to the estate's current on-disk signal.
    pub fn new(manifest: &Path) -> Self {
        let last = estate_signal(manifest);
        WorkspaceReloader {
            manifest: manifest.to_path_buf(),
            last,
        }
    }
}

impl GraphReloader for WorkspaceReloader {
    fn changed(&mut self) -> bool {
        estate_signal(&self.manifest) != self.last
    }

    fn reload(&mut self) -> Result<Graph, String> {
        let sig = estate_signal(&self.manifest);
        let (graph, _results) = load_workspace_graph(&self.manifest).map_err(|e| e.to_string())?;
        self.last = sig;
        Ok(graph)
    }
}
