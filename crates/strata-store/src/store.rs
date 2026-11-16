use std::collections::BTreeMap;
use std::path::Path;

use duckdb::Connection;
use strata_core::{
    graph::Direction,
    model::{Confidence, Edge, EdgeKind, Node, NodeKind, Provenance, Span},
    AnalyzedFile, Graph, Uid,
};

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("duckdb error: {0}")]
    Db(String),
    #[error("data integrity error: {0}")]
    Integrity(String),
}

impl From<duckdb::Error> for StoreError {
    fn from(e: duckdb::Error) -> Self {
        StoreError::Db(e.to_string())
    }
}

// ---------------------------------------------------------------------------
// Parse cache entry
// ---------------------------------------------------------------------------

/// One row of the parse cache: the blake3 hex hash of a file and the
/// `AnalyzedFile` that was produced when the file had that hash.
#[derive(Debug, Clone, PartialEq)]
pub struct ParseCacheEntry {
    pub hash: String,
    pub analyzed: AnalyzedFile,
}

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

pub trait GraphStore {
    /// Replace the persisted graph with `graph` (full save; idempotent).
    fn save_graph(&mut self, graph: &Graph) -> Result<(), StoreError>;
    /// Load the full persisted graph into memory.
    fn load_graph(&self) -> Result<Graph, StoreError>;
    /// Replace the persisted file→content-hash map.
    fn save_file_hashes(&mut self, hashes: &BTreeMap<String, String>) -> Result<(), StoreError>;
    /// Load the persisted file→content-hash map (empty if none).
    fn load_file_hashes(&self) -> Result<BTreeMap<String, String>, StoreError>;
    /// Full replace of the parse cache (atomic, single transaction).
    fn save_parse_cache(
        &mut self,
        entries: &BTreeMap<String, ParseCacheEntry>,
    ) -> Result<(), StoreError>;
    /// Load the full parse cache; returns an empty map if nothing was saved yet.
    fn load_parse_cache(&self) -> Result<BTreeMap<String, ParseCacheEntry>, StoreError>;
}

// ---------------------------------------------------------------------------
// Helpers: enum ↔ TEXT (via serde_json unit-variant serialisation)
// ---------------------------------------------------------------------------

/// Serialise a serde_json::Value to the bare variant name (no JSON quotes).
/// Call as: `enum_to_text(serde_json::to_value(&kind)?)`
fn strip_json_quotes(json: &str) -> String {
    json.trim_matches('"').to_owned()
}

/// Serialise a unit enum variant to its bare name (no JSON quotes).
/// Uses serde_json::Value so we avoid a direct `serde` dep in Cargo.toml.
fn node_kind_to_text(v: &strata_core::NodeKind) -> Result<String, StoreError> {
    let json = serde_json::to_string(v)
        .map_err(|e| StoreError::Integrity(format!("serialize NodeKind: {e}")))?;
    Ok(strip_json_quotes(&json))
}

fn edge_kind_to_text(v: &strata_core::EdgeKind) -> Result<String, StoreError> {
    let json = serde_json::to_string(v)
        .map_err(|e| StoreError::Integrity(format!("serialize EdgeKind: {e}")))?;
    Ok(strip_json_quotes(&json))
}

fn provenance_to_text(v: &strata_core::Provenance) -> Result<String, StoreError> {
    let json = serde_json::to_string(v)
        .map_err(|e| StoreError::Integrity(format!("serialize Provenance: {e}")))?;
    Ok(strip_json_quotes(&json))
}

fn text_to_node_kind(text: &str) -> Result<NodeKind, StoreError> {
    let json = serde_json::to_string(text)
        .map_err(|e| StoreError::Integrity(format!("encode NodeKind {:?}: {e}", text)))?;
    serde_json::from_str(&json)
        .map_err(|e| StoreError::Integrity(format!("parse NodeKind {:?}: {e}", text)))
}

fn text_to_edge_kind(text: &str) -> Result<EdgeKind, StoreError> {
    let json = serde_json::to_string(text)
        .map_err(|e| StoreError::Integrity(format!("encode EdgeKind {:?}: {e}", text)))?;
    serde_json::from_str(&json)
        .map_err(|e| StoreError::Integrity(format!("parse EdgeKind {:?}: {e}", text)))
}

fn text_to_provenance(text: &str) -> Result<Provenance, StoreError> {
    let json = serde_json::to_string(text)
        .map_err(|e| StoreError::Integrity(format!("encode Provenance {:?}: {e}", text)))?;
    serde_json::from_str(&json)
        .map_err(|e| StoreError::Integrity(format!("parse Provenance {:?}: {e}", text)))
}

// ---------------------------------------------------------------------------
// Schema
// ---------------------------------------------------------------------------

const SCHEMA_SQL: &str = "
CREATE TABLE IF NOT EXISTS nodes (
    uid          TEXT    PRIMARY KEY,
    kind         TEXT    NOT NULL,
    name         TEXT    NOT NULL,
    fqn          TEXT    NOT NULL,
    path         TEXT    NOT NULL,
    start_line   INTEGER,
    start_col    INTEGER,
    end_line     INTEGER,
    end_col      INTEGER,
    provenance   TEXT    NOT NULL,
    confidence   REAL    NOT NULL
);
CREATE TABLE IF NOT EXISTS edges (
    src          TEXT NOT NULL,
    dst          TEXT NOT NULL,
    kind         TEXT NOT NULL,
    provenance   TEXT NOT NULL,
    confidence   REAL NOT NULL
);
CREATE TABLE IF NOT EXISTS file_hashes (
    path  TEXT PRIMARY KEY,
    hash  TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS parse_cache (
    path           TEXT    PRIMARY KEY,
    hash           TEXT    NOT NULL,
    analyzed       TEXT    NOT NULL,
    schema_version INTEGER NOT NULL
);
";

// ---------------------------------------------------------------------------
// DuckGraphStore
// ---------------------------------------------------------------------------

/// Map a DuckDB open failure onto an actionable [`StoreError`].
///
/// The one case worth translating is a graph written by a build with a newer
/// DuckDB: DuckDB refuses it with a message about storage version numbers
/// ("Trying to read a database file with version number 999, but we can only
/// read versions between 64 and 68"), which is accurate and useless — it names
/// neither the cause nor the remedy. Since the graph is derived data, deleting
/// and re-indexing is always safe, so say that.
///
/// Every other error is passed through unchanged: guessing at unrelated IO or
/// permission failures would be worse than DuckDB's own wording.
fn explain_open_error(path: &Path, e: duckdb::Error) -> StoreError {
    let text = e.to_string();
    let is_version_mismatch = text.contains("version number")
        && (text.contains("can only read versions") || text.contains("newer version"));
    if !is_version_mismatch {
        return StoreError::from(e);
    }
    StoreError::Db(format!(
        "{} was written by a newer build of strata and cannot be read by this one. \
         The graph is derived data, so it is safe to rebuild: delete it and re-index \
         (`strata index <repo>`, or `rm {} && strata index <repo>`). \
         Underlying error: {text}",
        path.display(),
        path.display()
    ))
}

pub struct DuckGraphStore {
    conn: Connection,
}

impl DuckGraphStore {
    fn init(conn: Connection) -> Result<Self, StoreError> {
        conn.execute_batch(SCHEMA_SQL)?;
        Ok(DuckGraphStore { conn })
    }

    /// Open (creating if needed) a DuckDB database at `path`; ensures schema exists.
    ///
    /// A graph written by a build carrying a NEWER DuckDB than this one cannot
    /// be read — DuckDB's storage format is forward-incompatible by design, and
    /// its own error names only storage version numbers, which tell the reader
    /// nothing about what to do. The graph is derived data, so the remedy is
    /// always the same and always safe: re-index. [`explain_open_error`] turns
    /// that case into an instruction.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let conn = Connection::open(path).map_err(|e| explain_open_error(path, e))?;
        Self::init(conn)
    }

    /// Open an in-memory DuckDB database; ensures schema exists.
    pub fn open_in_memory() -> Result<Self, StoreError> {
        let conn = Connection::open_in_memory()?;
        Self::init(conn)
    }
}

impl GraphStore for DuckGraphStore {
    fn save_graph(&mut self, graph: &Graph) -> Result<(), StoreError> {
        // Full replace inside a single transaction: if any insert fails the
        // DELETE is rolled back automatically when `tx` is dropped.
        let tx = self.conn.transaction()?;

        tx.execute_batch("DELETE FROM nodes; DELETE FROM edges;")?;

        // --- Insert nodes ---
        {
            let mut stmt = tx.prepare(
                "INSERT INTO nodes \
                 (uid, kind, name, fqn, path, start_line, start_col, end_line, end_col, provenance, confidence) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )?;
            for node in graph.nodes() {
                let kind = node_kind_to_text(&node.kind)?;
                let prov = provenance_to_text(&node.provenance)?;
                stmt.execute(duckdb::params![
                    node.uid.as_str(),
                    kind,
                    node.name.as_str(),
                    node.fqn.as_str(),
                    node.path.as_str(),
                    node.span.start_line,
                    node.span.start_col,
                    node.span.end_line,
                    node.span.end_col,
                    prov,
                    f64::from(node.confidence.value()),
                ])?;
            }
        }

        // --- Insert edges ---
        // Graph has no public edges() iterator; collect edges via outgoing neighbors.
        // Each edge is stored exactly once in out_adj[src], so iterating outgoing
        // neighbors across all nodes yields every edge exactly once.
        {
            let mut stmt = tx.prepare(
                "INSERT INTO edges (src, dst, kind, provenance, confidence) VALUES (?, ?, ?, ?, ?)",
            )?;
            let node_uids: Vec<Uid> = graph.nodes().map(|n| n.uid.clone()).collect();
            for uid in &node_uids {
                for (edge, _target) in graph.neighbors(uid, Direction::Outgoing, &[]) {
                    let kind = edge_kind_to_text(&edge.kind)?;
                    let prov = provenance_to_text(&edge.provenance)?;
                    stmt.execute(duckdb::params![
                        edge.src.as_str(),
                        edge.dst.as_str(),
                        kind,
                        prov,
                        f64::from(edge.confidence.value()),
                    ])?;
                }
            }
        }

        tx.commit()?;
        Ok(())
    }

    fn load_graph(&self) -> Result<Graph, StoreError> {
        let mut graph = Graph::new();

        // --- Load nodes ---
        {
            let mut stmt = self.conn.prepare(
                "SELECT uid, kind, name, fqn, path, \
                        start_line, start_col, end_line, end_col, \
                        provenance, confidence \
                 FROM nodes",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?, // uid
                    row.get::<_, String>(1)?, // kind
                    row.get::<_, String>(2)?, // name
                    row.get::<_, String>(3)?, // fqn
                    row.get::<_, String>(4)?, // path
                    row.get::<_, u32>(5)?,    // start_line
                    row.get::<_, u32>(6)?,    // start_col
                    row.get::<_, u32>(7)?,    // end_line
                    row.get::<_, u32>(8)?,    // end_col
                    row.get::<_, String>(9)?, // provenance
                    row.get::<_, f64>(10)?,   // confidence
                ))
            })?;

            for row in rows {
                let (
                    uid_s,
                    kind_s,
                    name,
                    fqn,
                    path,
                    start_line,
                    start_col,
                    end_line,
                    end_col,
                    prov_s,
                    conf_v,
                ) = row.map_err(StoreError::from)?;

                let kind: NodeKind = text_to_node_kind(&kind_s)?;
                let provenance: Provenance = text_to_provenance(&prov_s)?;
                let node = Node {
                    uid: Uid(uid_s),
                    kind,
                    name,
                    fqn,
                    path,
                    span: Span {
                        start_line,
                        start_col,
                        end_line,
                        end_col,
                    },
                    provenance,
                    confidence: Confidence::new(conf_v as f32),
                };
                graph.add_node(node);
            }
        }

        // --- Load edges ---
        {
            let mut stmt = self
                .conn
                .prepare("SELECT src, dst, kind, provenance, confidence FROM edges")?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?, // src
                    row.get::<_, String>(1)?, // dst
                    row.get::<_, String>(2)?, // kind
                    row.get::<_, String>(3)?, // provenance
                    row.get::<_, f64>(4)?,    // confidence
                ))
            })?;

            for row in rows {
                let (src_s, dst_s, kind_s, prov_s, conf_v) = row.map_err(StoreError::from)?;
                let kind: EdgeKind = text_to_edge_kind(&kind_s)?;
                let provenance: Provenance = text_to_provenance(&prov_s)?;
                let edge = Edge {
                    src: Uid(src_s),
                    dst: Uid(dst_s),
                    kind,
                    provenance,
                    confidence: Confidence::new(conf_v as f32),
                };
                graph.add_edge(edge);
            }
        }

        Ok(graph)
    }

    fn save_file_hashes(&mut self, hashes: &BTreeMap<String, String>) -> Result<(), StoreError> {
        // Full replace inside a single transaction: if any insert fails the
        // DELETE is rolled back automatically when `tx` is dropped.
        let tx = self.conn.transaction()?;

        tx.execute_batch("DELETE FROM file_hashes;")?;

        {
            let mut stmt = tx.prepare("INSERT INTO file_hashes (path, hash) VALUES (?, ?)")?;
            for (path, hash) in hashes {
                stmt.execute(duckdb::params![path, hash])?;
            }
        }

        tx.commit()?;
        Ok(())
    }

    fn load_file_hashes(&self) -> Result<BTreeMap<String, String>, StoreError> {
        let mut map = BTreeMap::new();
        let mut stmt = self.conn.prepare("SELECT path, hash FROM file_hashes")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (path, hash) = row.map_err(StoreError::from)?;
            map.insert(path, hash);
        }
        Ok(map)
    }

    fn save_parse_cache(
        &mut self,
        entries: &BTreeMap<String, ParseCacheEntry>,
    ) -> Result<(), StoreError> {
        // Full replace inside a single transaction (mirrors save_file_hashes).
        let tx = self.conn.transaction()?;

        tx.execute_batch("DELETE FROM parse_cache;")?;

        {
            let mut stmt = tx.prepare(
                "INSERT INTO parse_cache (path, hash, analyzed, schema_version) VALUES (?, ?, ?, ?)",
            )?;
            for (path, entry) in entries {
                let analyzed_json = serde_json::to_string(&entry.analyzed).map_err(|e| {
                    StoreError::Integrity(format!("serialize AnalyzedFile for {path}: {e}"))
                })?;
                stmt.execute(duckdb::params![
                    path,
                    entry.hash,
                    analyzed_json,
                    strata_core::ANALYZER_SCHEMA_VERSION
                ])?;
            }
        }

        tx.commit()?;
        Ok(())
    }

    fn load_parse_cache(&self) -> Result<BTreeMap<String, ParseCacheEntry>, StoreError> {
        let mut map = BTreeMap::new();
        let mut stmt = self
            .conn
            .prepare("SELECT path, hash, analyzed FROM parse_cache WHERE schema_version = ?")?;
        let rows = stmt.query_map(
            duckdb::params![strata_core::ANALYZER_SCHEMA_VERSION],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )?;
        for row in rows {
            let (path, hash, analyzed_json) = row.map_err(StoreError::from)?;
            let analyzed: AnalyzedFile = serde_json::from_str(&analyzed_json).map_err(|e| {
                StoreError::Integrity(format!("parse AnalyzedFile for {path}: {e}"))
            })?;
            map.insert(path, ParseCacheEntry { hash, analyzed });
        }
        Ok(map)
    }
}

#[cfg(test)]
mod open_error_tests {
    use super::*;

    /// A graph written by a newer build must produce an instruction, not a
    /// storage-version number. DuckDB's own wording is accurate and useless.
    #[test]
    fn a_newer_graph_explains_how_to_recover() {
        let e = duckdb::Error::DuckDBFailure(
            duckdb::ffi::Error::new(1),
            Some(
                "IO Error: Trying to read a database file with version number 999, \
                 but we can only read versions between 64 and 68."
                    .to_string(),
            ),
        );
        let msg = explain_open_error(Path::new("/repo/.strata/graph.duckdb"), e).to_string();
        assert!(
            msg.contains("written by a newer build"),
            "must name the cause: {msg}"
        );
        assert!(msg.contains("strata index"), "must name the remedy: {msg}");
        assert!(
            msg.contains("/repo/.strata/graph.duckdb"),
            "must name the file: {msg}"
        );
        assert!(
            msg.contains("version number 999"),
            "must keep the underlying error for support: {msg}"
        );
    }

    /// Unrelated failures pass through untouched — guessing at an IO or
    /// permission error would be worse than DuckDB's own wording.
    #[test]
    fn unrelated_open_errors_are_not_reworded() {
        let e = duckdb::Error::DuckDBFailure(
            duckdb::ffi::Error::new(1),
            Some("IO Error: Permission denied".to_string()),
        );
        let msg = explain_open_error(Path::new("/repo/x.duckdb"), e).to_string();
        assert!(msg.contains("Permission denied"), "{msg}");
        assert!(
            !msg.contains("strata index"),
            "must not offer a remedy that does not apply: {msg}"
        );
    }
}
