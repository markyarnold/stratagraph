//! The hot-reload change *stamp*: a tiny sidecar file the indexer writes after a
//! successful index, used by the MCP server as a cheap, race-free "did the
//! on-disk graph change?" signal.
//!
//! Why a sidecar and not the db itself:
//! * The check must be **cheap and lock-free** — the server runs it before every
//!   request. Reading a few-byte file is a `stat` + tiny read; opening the
//!   duckdb file is not (and could contend with an in-flight reindex).
//! * It must be **unique per index run** so a re-index that produced the *same*
//!   node/edge counts still flips the signal (the timestamp guarantees this).
//!
//! Write ordering / race-freedom: [`IndexStamp::write`] is called by the indexer
//! *after* the graph / file-hash / parse-cache saves have returned (see
//! `index_impl`). The stamp is written atomically (temp sibling + `rename`) so a
//! concurrent reader never observes a half-written stamp. A reader keying off the
//! stamp then opens the db to reload; that open is itself fail-fast under a
//! writer's lock (DuckDB returns a "Conflicting lock" error cross-process rather
//! than blocking), so even a reader that races a still-in-flight write degrades
//! safely instead of hanging.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// The stamp file name inside a repo's `.strata/` directory.
pub const STAMP_FILE: &str = "index.stamp";

/// The serialized contents of `<repo>/.strata/index.stamp`.
///
/// Persisted as a single-line JSON object. The fields together make the bytes
/// **unique per index run**: `indexed_at_nanos` is a fresh wall-clock reading
/// each time, so two indexes that happen to produce identical `nodes`/`edges`
/// still yield different stamp bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexStamp {
    /// The engine identity that produced this index (`strata_core::ENGINE_ID`).
    /// A change here means a differently-versioned engine wrote the graph.
    pub engine_id: String,
    /// Wall-clock nanoseconds since the Unix epoch at stamp-write time. The
    /// freshness component that flips the signal on every run.
    pub indexed_at_nanos: u128,
    /// Node count of the indexed graph (informational + part of the signal).
    pub nodes: usize,
    /// Edge count of the indexed graph (informational + part of the signal).
    pub edges: usize,
}

impl IndexStamp {
    /// Build a stamp for a freshly-indexed graph of `nodes`/`edges`, timestamped
    /// now. `now()` failures (clock before the epoch — not expected) fall back to
    /// `0`, which is still a valid, comparable signal.
    pub fn new(nodes: usize, edges: usize) -> Self {
        let indexed_at_nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        IndexStamp {
            engine_id: strata_core::ENGINE_ID.to_string(),
            indexed_at_nanos,
            nodes,
            edges,
        }
    }

    /// Atomically write this stamp into `strata_dir` (the repo's `.strata/`
    /// directory), creating the directory if needed.
    ///
    /// Atomicity: the JSON is written to a uniquely-named temp sibling in the
    /// same directory, then `rename`d over [`STAMP_FILE`]. `rename` within a
    /// directory is atomic on every platform we target, so a concurrent reader
    /// sees either the old stamp or the new one — never a partial write.
    pub fn write(&self, strata_dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(strata_dir)?;
        let json = serde_json::to_string(self).map_err(std::io::Error::other)?;

        // Unique temp name in the same dir (same filesystem ⇒ rename is atomic).
        // The pid + nanos suffix avoids collisions between concurrent writers.
        let tmp_name = format!(
            ".{STAMP_FILE}.tmp.{}.{}",
            std::process::id(),
            self.indexed_at_nanos
        );
        let tmp_path = strata_dir.join(tmp_name);
        std::fs::write(&tmp_path, json.as_bytes())?;
        match std::fs::rename(&tmp_path, strata_dir.join(STAMP_FILE)) {
            Ok(()) => Ok(()),
            Err(e) => {
                // Best-effort cleanup so a failed rename leaves no temp litter.
                let _ = std::fs::remove_file(&tmp_path);
                Err(e)
            }
        }
    }

    /// Read the raw bytes of `<strata_dir>/index.stamp`, or `None` if it does not
    /// exist (or cannot be read). This is the **change signal** the server
    /// compares across requests — it deliberately returns the raw bytes (not a
    /// parsed struct) so any byte difference counts as "changed", and it **never
    /// opens the duckdb file**.
    pub fn read(strata_dir: &Path) -> Option<Vec<u8>> {
        std::fs::read(strata_dir.join(STAMP_FILE)).ok()
    }
}
