//! StrataGraph desktop backend: managed graph state + the three Tauri commands.
//!
//! The window opens a code graph (a `.strata/graph.duckdb` file or a
//! `strata.workspace.toml` estate) into in-memory [`strata_core::Graph`] state,
//! then answers `query`/`context`/`impact` over it via the **shared**
//! [`strata_mcp::call_tool`] dispatch (so the GUI never disagrees with the
//! MCP/CLI) plus a GUI-specific `subgraph` neighbourhood feed for the M2
//! renderer.
//!
//! The actual logic lives in [`commands`] and [`subgraph`] as plain, testable
//! functions; the `#[tauri::command]` wrappers below are thin glue over the
//! managed state. The window itself cannot be driven in the build harness, so
//! correctness is pinned by the unit tests in those modules.

mod commands;
mod subgraph;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::Value;
use strata_core::Graph;
use tauri::{Manager, State};

pub use commands::{OpenInfo, OpenedSource, RepoStatus};
pub use subgraph::{plane_of, SubgraphDto, SubgraphEdge, SubgraphNode, MAX_DEPTH, MAX_NODES};

/// The currently-loaded graph plus the summary returned at open time and the
/// [`OpenedSource`] descriptor needed to rebuild it. `None` until the user opens
/// something.
struct LoadedGraph {
    graph: Graph,
    #[allow(dead_code)] // retained for parity with the open response / future UI use.
    source: String,
    #[allow(dead_code)]
    summary: OpenInfo,
    /// What was opened, so `reindex` can rebuild it the same way the CLI would.
    source_kind: OpenedSource,
}

/// Single-flight guard for indexing: a dedicated [`AtomicBool`] that is `true`
/// while a `reindex`/`index_path` runs. A second concurrent attempt sees the
/// flag set and is rejected with [`INDEXING_BUSY`] — it does **not** wait.
///
/// Acquisition returns an [`IndexInProgress`] RAII token; the flag is cleared in
/// its `Drop`, so it is released on **every** exit path of the command —
/// including early `?` errors and panics — and can never leak the "busy" state.
#[derive(Default)]
struct IndexFlag {
    running: Arc<AtomicBool>,
}

/// The error returned when an index is already running (single-flight reject).
const INDEXING_BUSY: &str = "Indexing is already running.";

/// The error returned when `reindex` is called with nothing loaded.
const NOTHING_LOADED: &str = "Open a project first.";

impl IndexFlag {
    /// Try to begin indexing. `Ok(token)` when we won the flag (released on the
    /// token's drop); `Err(INDEXING_BUSY)` when another index already holds it.
    ///
    /// Uses `compare_exchange` so the test-and-set is atomic: exactly one caller
    /// can transition `false → true`.
    fn try_begin(&self) -> Result<IndexInProgress, String> {
        match self
            .running
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => Ok(IndexInProgress {
                flag: Arc::clone(&self.running),
            }),
            Err(_) => Err(INDEXING_BUSY.to_string()),
        }
    }
}

/// RAII token proving an index is in flight. Dropping it clears the
/// [`IndexFlag`] — the release happens on success, on `?`-propagated error, and
/// on panic alike, so the single-flight flag cannot be leaked by an error path.
struct IndexInProgress {
    flag: Arc<AtomicBool>,
}

impl Drop for IndexInProgress {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::Release);
    }
}

/// Managed Tauri state: an optional loaded graph behind a mutex, plus the
/// indexing single-flight flag. A command that needs the graph locks it and
/// errors cleanly if nothing is open.
#[derive(Default)]
struct AppState {
    loaded: Mutex<Option<LoadedGraph>>,
    indexing: IndexFlag,
}

impl AppState {
    /// Run `f` against the loaded graph, or return a uniform "open a graph first"
    /// error when nothing is loaded. Centralises the lock + not-loaded check the
    /// `tool`/`subgraph` commands share.
    fn with_graph<T>(&self, f: impl FnOnce(&Graph) -> Result<T, String>) -> Result<T, String> {
        let guard = self
            .loaded
            .lock()
            .map_err(|_| "internal: graph state lock poisoned".to_string())?;
        match guard.as_ref() {
            Some(loaded) => f(&loaded.graph),
            None => Err("no graph loaded — open a graph or workspace first".to_string()),
        }
    }

    /// Briefly lock the state and **clone** the current [`OpenedSource`], or
    /// return [`NOTHING_LOADED`] when nothing is open. The clone lets the caller
    /// drop the std mutex *before* the blocking reindex — the lock is never held
    /// across the CPU work or an `.await` (the non-negotiable concurrency rule).
    fn clone_source(&self) -> Result<OpenedSource, String> {
        let guard = self
            .loaded
            .lock()
            .map_err(|_| "internal: graph state lock poisoned".to_string())?;
        match guard.as_ref() {
            Some(loaded) => Ok(loaded.source_kind.clone()),
            None => Err(NOTHING_LOADED.to_string()),
        }
    }

    /// Atomically swap in a freshly-built graph at the end of a reindex: lock,
    /// replace the whole `LoadedGraph`, drop. The old graph stays queryable until
    /// this instant, and the lock is held only for the pointer swap.
    fn swap_in(
        &self,
        graph: Graph,
        info: OpenInfo,
        source_kind: OpenedSource,
    ) -> Result<(), String> {
        let mut guard = self
            .loaded
            .lock()
            .map_err(|_| "internal: graph state lock poisoned".to_string())?;
        *guard = Some(LoadedGraph {
            graph,
            source: info.source.clone(),
            summary: info,
            source_kind,
        });
        Ok(())
    }
}

/// Open a `.duckdb` graph file or a `strata.workspace.toml` estate, replacing any
/// currently-loaded graph. Returns the [`OpenInfo`] summary (also stashed in
/// state for later reference).
#[tauri::command]
fn open(path: String, state: State<'_, AppState>) -> Result<OpenInfo, String> {
    let (graph, info, source_kind) = commands::open_path(&path)?;
    state.swap_in(graph, info.clone(), source_kind)?;
    Ok(info)
}

/// Rebuild the currently-loaded graph the way the CLI's `index` command does,
/// then atomically swap in the fresh graph and return the new [`OpenInfo`].
///
/// **Concurrency discipline (non-negotiable):** the `std::sync::Mutex` is *never*
/// held across the blocking work. We (1) briefly lock to clone the descriptor and
/// drop the guard, (2) acquire the single-flight [`IndexInProgress`] token, (3)
/// run the index + reload on a blocking thread via
/// [`tauri::async_runtime::spawn_blocking`], (4) briefly lock again to swap. A
/// second concurrent `reindex`/`index_path` is rejected with [`INDEXING_BUSY`]
/// via the RAII guard (released on every error path, never manually reset).
/// During the rebuild the OLD graph stays loaded and queryable; the swap is
/// atomic at the very end.
#[tauri::command]
async fn reindex(state: State<'_, AppState>) -> Result<OpenInfo, String> {
    // (1) Brief lock → clone descriptor → drop guard (errors if nothing loaded).
    let source = state.clone_source()?;
    // (2) Single-flight: claim the flag (released on the token's drop).
    let _guard = state.indexing.try_begin()?;
    // (3) Blocking index + reload OFF the async/UI thread; old graph stays live.
    let (graph, info, source_kind) = run_reindex_blocking(source).await?;
    // (4) Brief lock → atomic swap → drop.
    state.swap_in(graph, info.clone(), source_kind)?;
    Ok(info)
    // `_guard` drops here → single-flight flag cleared.
}

/// Index a folder that has no index yet (the open-project dead end), then open
/// it — same machinery and single-flight guard as [`reindex`], for an explicit
/// path rather than the remembered source.
///
/// `path` is treated as a [`OpenedSource::Repo`]: it is indexed into
/// `<path>/.strata/graph.duckdb` and loaded, exactly as `strata index <path>`
/// then opening the folder would.
#[tauri::command]
async fn index_path(path: String, state: State<'_, AppState>) -> Result<OpenInfo, String> {
    // Single-flight applies here too: an Index Now while a reindex runs is
    // rejected rather than racing two writers at the same db.
    let _guard = state.indexing.try_begin()?;
    let source = OpenedSource::Repo(std::path::PathBuf::from(&path));
    let (graph, info, source_kind) = run_reindex_blocking(source).await?;
    state.swap_in(graph, info.clone(), source_kind)?;
    Ok(info)
}

/// Run [`commands::reindex_source`] on a blocking thread (it is CPU-bound: a
/// full repo walk + parse + persist). Kept as a free async fn so both commands
/// share the one off-thread hop and so the `spawn_blocking` join error is mapped
/// uniformly.
async fn run_reindex_blocking(
    source: OpenedSource,
) -> Result<(Graph, OpenInfo, OpenedSource), String> {
    tauri::async_runtime::spawn_blocking(move || commands::reindex_source(&source))
        .await
        .map_err(|e| format!("indexing task failed to run: {e}"))?
}

/// Run a `query` / `context` / `impact` tool over the loaded graph, delegating to
/// the shared [`strata_mcp::call_tool`] dispatch.
#[tauri::command]
fn tool(name: String, args: Value, state: State<'_, AppState>) -> Result<Value, String> {
    state.with_graph(|graph| commands::run_tool(graph, &name, &args))
}

/// Bounded both-directions neighbourhood of `uid` for the renderer (depth ≤
/// [`MAX_DEPTH`], ≤ [`MAX_NODES`] nodes, optional edge-kind and plane filters).
#[tauri::command]
fn subgraph(
    uid: String,
    depth: u32,
    kinds: Option<Vec<String>>,
    planes: Option<Vec<String>>,
    state: State<'_, AppState>,
) -> Result<SubgraphDto, String> {
    state.with_graph(|graph| subgraph::compute_subgraph(graph, &uid, depth, &kinds, &planes))
}

/// Build and run the Tauri application. Kept as a library entry point (called by
/// `main.rs`) so the rest of the crate stays unit-testable.
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            app.manage(AppState::default());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            open, tool, subgraph, reindex, index_path
        ])
        .run(tauri::generate_context!())
        .expect("error while running the StrataGraph desktop application");
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Group 4: reindex with nothing loaded → "Open a project first." ──
    //
    // The async command early-returns this via `clone_source` before touching the
    // single-flight flag or spawning anything; we pin the state helper directly
    // (the async wrapper is a thin shell over it).
    #[test]
    fn clone_source_with_nothing_loaded_is_open_first_error() {
        let state = AppState::default();
        let err = state.clone_source().unwrap_err();
        assert_eq!(err, NOTHING_LOADED);
        assert_eq!(err, "Open a project first.");
    }

    #[test]
    fn clone_source_returns_the_remembered_descriptor() {
        let state = AppState::default();
        let src = OpenedSource::Repo(std::path::PathBuf::from("/some/project"));
        // Seed state with an empty graph + this descriptor.
        state
            .swap_in(
                Graph::new(),
                OpenInfo {
                    engine: strata_core::ENGINE_ID.to_string(),
                    source: "db: x".into(),
                    nodes: 0,
                    edges: 0,
                    repos: vec![],
                },
                src.clone(),
            )
            .expect("swap in");
        assert_eq!(state.clone_source().expect("clone"), src);
    }

    // ── Group 5: single-flight guard. ──
    //
    // (a) While one token is held, a second `try_begin` is rejected with exactly
    //     "Indexing is already running."
    // (b) The flag is an RAII release: dropping the first token lets the next
    //     `try_begin` succeed — proving the busy state cannot leak (the same Drop
    //     fires on a command's early-error path, so an error never wedges it).
    #[test]
    fn single_flight_rejects_a_second_concurrent_index() {
        let flag = IndexFlag::default();
        let first = flag.try_begin().expect("first acquire wins the flag");
        // `IndexInProgress` (the Ok type) is intentionally not `Debug`, so match
        // the second attempt rather than `unwrap_err()`.
        let err = match flag.try_begin() {
            Ok(_) => panic!("a second concurrent index must be rejected"),
            Err(e) => e,
        };
        assert_eq!(err, INDEXING_BUSY);
        assert_eq!(err, "Indexing is already running.");
        // Keep `first` alive across the second attempt (else the test proves
        // nothing); explicit drop documents the release point.
        drop(first);
    }

    #[test]
    fn single_flight_flag_is_released_on_drop_no_leak() {
        let flag = IndexFlag::default();
        {
            let _token = flag.try_begin().expect("acquire");
            // Busy while the token is alive.
            assert!(flag.try_begin().is_err(), "busy while held");
        } // token dropped here (mirrors a command returning — Ok OR Err)
          // Released: the next caller can acquire again. This is the "released on
          // error paths too — a guard, not a manual reset" guarantee.
        flag.try_begin()
            .expect("flag released after the prior token dropped");
    }

    #[test]
    fn swap_in_makes_the_new_graph_queryable_and_atomic() {
        // `swap_in` must replace the WHOLE loaded graph; `with_graph` then sees the
        // new one. (The atomicity is structural: a single mutex guards the Option,
        // and the old value is only replaced under the lock.)
        let state = AppState::default();
        // Nothing loaded yet → with_graph errors.
        assert!(state.with_graph(|_| Ok(())).is_err());
        state
            .swap_in(
                Graph::new(),
                OpenInfo {
                    engine: strata_core::ENGINE_ID.to_string(),
                    source: "db: y".into(),
                    nodes: 0,
                    edges: 0,
                    repos: vec![],
                },
                OpenedSource::GraphFile(std::path::PathBuf::from("/g.duckdb")),
            )
            .expect("swap in");
        // Now loaded → with_graph runs the closure.
        let n = state
            .with_graph(|g| Ok(g.node_count()))
            .expect("graph present after swap");
        assert_eq!(n, 0);
    }
}
