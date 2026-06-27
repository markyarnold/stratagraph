use std::collections::{BTreeMap, HashMap};

use strata_core::{
    model::{Confidence, Edge, EdgeKind, Node, NodeKind, Provenance, Span},
    AnalyzedFile, CallRef, Graph, ImportRef, RawSymbol, Uid, ANALYZER_SCHEMA_VERSION,
};
use strata_store::{DuckGraphStore, GraphStore, ParseCacheEntry};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn make_node(id: &str, kind: NodeKind, provenance: Provenance, confidence: f32) -> Node {
    Node {
        uid: Uid(id.to_string()),
        kind,
        name: id.to_string(),
        fqn: format!("pkg::{id}"),
        path: format!("src/{id}.ts"),
        span: Span {
            start_line: 10,
            start_col: 4,
            end_line: 20,
            end_col: 1,
        },
        provenance,
        confidence: Confidence::new(confidence),
    }
}

fn make_edge(
    src: &str,
    dst: &str,
    kind: EdgeKind,
    provenance: Provenance,
    confidence: f32,
) -> Edge {
    Edge {
        src: Uid(src.to_string()),
        dst: Uid(dst.to_string()),
        kind,
        provenance,
        confidence: Confidence::new(confidence),
    }
}

/// Build a test graph with 3 varied nodes and 2 varied edges.
fn build_test_graph() -> Graph {
    let mut g = Graph::new();
    g.add_node(make_node(
        "alpha",
        NodeKind::Function,
        Provenance::Extracted,
        1.0,
    ));
    g.add_node(make_node(
        "beta",
        NodeKind::Class,
        Provenance::Inferred,
        0.75,
    ));
    g.add_node(make_node(
        "gamma",
        NodeKind::Module,
        Provenance::Observed,
        0.5,
    ));
    g.add_edge(make_edge(
        "alpha",
        "beta",
        EdgeKind::Calls,
        Provenance::Resolved,
        0.9,
    ));
    g.add_edge(make_edge(
        "beta",
        "gamma",
        EdgeKind::MemberOf,
        Provenance::Ambiguous,
        0.6,
    ));
    g
}

/// Convert a Graph's nodes into a map keyed by uid for order-independent comparison.
fn node_map(g: &Graph) -> HashMap<String, Node> {
    g.nodes().map(|n| (n.uid.0.clone(), n.clone())).collect()
}

// ---------------------------------------------------------------------------
// Test 1: Empty load
// ---------------------------------------------------------------------------

#[test]
fn empty_load() {
    let store = DuckGraphStore::open_in_memory().expect("open_in_memory failed");
    let g = store.load_graph().expect("load_graph failed");
    assert_eq!(g.node_count(), 0, "expected 0 nodes");
    assert_eq!(g.edge_count(), 0, "expected 0 edges");

    let hashes = store.load_file_hashes().expect("load_file_hashes failed");
    assert!(hashes.is_empty(), "expected empty file hashes");
}

// ---------------------------------------------------------------------------
// Test 2: Round-trip fidelity (in-memory)
// ---------------------------------------------------------------------------

#[test]
fn round_trip_in_memory() {
    let mut store = DuckGraphStore::open_in_memory().expect("open_in_memory failed");
    let original = build_test_graph();

    store.save_graph(&original).expect("save_graph failed");

    let loaded = store.load_graph().expect("load_graph failed");
    assert_eq!(
        loaded.node_count(),
        original.node_count(),
        "node count mismatch"
    );
    assert_eq!(
        loaded.edge_count(),
        original.edge_count(),
        "edge count mismatch"
    );

    let orig_nodes = node_map(&original);
    let load_nodes = node_map(&loaded);

    for (uid, orig) in &orig_nodes {
        let got = load_nodes
            .get(uid)
            .unwrap_or_else(|| panic!("missing node {uid}"));
        assert_eq!(orig.kind, got.kind, "kind mismatch for {uid}");
        assert_eq!(orig.name, got.name, "name mismatch for {uid}");
        assert_eq!(orig.fqn, got.fqn, "fqn mismatch for {uid}");
        assert_eq!(orig.path, got.path, "path mismatch for {uid}");
        assert_eq!(orig.span, got.span, "span mismatch for {uid}");
        assert_eq!(
            orig.provenance, got.provenance,
            "provenance mismatch for {uid}"
        );
        assert!(
            (orig.confidence.value() - got.confidence.value()).abs() < 1e-6,
            "confidence mismatch for {uid}: {} vs {}",
            orig.confidence.value(),
            got.confidence.value(),
        );
    }

    // Check edges order-independently: collect (src, dst, kind) triples.
    use strata_core::graph::Direction;
    let orig_edge_set: std::collections::HashSet<(String, String, String)> = {
        original
            .nodes()
            .flat_map(|n| {
                original
                    .neighbors(&n.uid, Direction::Outgoing, &[])
                    .into_iter()
                    .map(|(e, _)| (e.src.0.clone(), e.dst.0.clone(), format!("{:?}", e.kind)))
                    .collect::<Vec<_>>()
            })
            .collect()
    };
    let loaded_edge_set: std::collections::HashSet<(String, String, String)> = {
        loaded
            .nodes()
            .flat_map(|n| {
                loaded
                    .neighbors(&n.uid, Direction::Outgoing, &[])
                    .into_iter()
                    .map(|(e, _)| (e.src.0.clone(), e.dst.0.clone(), format!("{:?}", e.kind)))
                    .collect::<Vec<_>>()
            })
            .collect()
    };
    assert_eq!(orig_edge_set, loaded_edge_set, "edge sets differ");
}

// ---------------------------------------------------------------------------
// Test 3: Round-trip fidelity (file — drop and reopen)
// ---------------------------------------------------------------------------

#[test]
fn round_trip_file_reopen() {
    let tmp = tempfile::tempdir().expect("tempdir failed");
    let db_path = tmp.path().join("test.db");

    let original = build_test_graph();

    {
        let mut store = DuckGraphStore::open(&db_path).expect("open file store failed");
        store.save_graph(&original).expect("save_graph failed");
        // Drop the store — flush to disk.
    }

    // Reopen and load.
    let store2 = DuckGraphStore::open(&db_path).expect("reopen file store failed");
    let loaded = store2.load_graph().expect("load_graph after reopen failed");

    assert_eq!(
        loaded.node_count(),
        original.node_count(),
        "node count mismatch after reopen"
    );
    assert_eq!(
        loaded.edge_count(),
        original.edge_count(),
        "edge count mismatch after reopen"
    );

    let orig_nodes = node_map(&original);
    let load_nodes = node_map(&loaded);
    for (uid, orig) in &orig_nodes {
        let got = load_nodes
            .get(uid)
            .unwrap_or_else(|| panic!("missing node {uid} after reopen"));
        assert_eq!(orig.kind, got.kind, "kind mismatch {uid}");
        assert_eq!(orig.span, got.span, "span mismatch {uid}");
        assert_eq!(orig.provenance, got.provenance, "provenance mismatch {uid}");
    }
}

// ---------------------------------------------------------------------------
// Test 4: Idempotent save — no duplication
// ---------------------------------------------------------------------------

#[test]
fn idempotent_save_no_duplication() {
    let mut store = DuckGraphStore::open_in_memory().expect("open_in_memory failed");
    let g = build_test_graph();

    store.save_graph(&g).expect("first save failed");
    store.save_graph(&g).expect("second save failed");

    let loaded = store.load_graph().expect("load_graph failed");
    assert_eq!(
        loaded.node_count(),
        g.node_count(),
        "node count after double-save (expected no duplication)"
    );
    assert_eq!(
        loaded.edge_count(),
        g.edge_count(),
        "edge count after double-save (expected no duplication)"
    );
}

// ---------------------------------------------------------------------------
// Test 5: File-hash round-trip (with reopen)
// ---------------------------------------------------------------------------

#[test]
fn file_hash_round_trip() {
    let tmp = tempfile::tempdir().expect("tempdir failed");
    let db_path = tmp.path().join("hashes.db");

    let mut hashes = BTreeMap::new();
    hashes.insert("src/main.ts".to_string(), "abc123def456".to_string());
    hashes.insert("src/lib.ts".to_string(), "feedbeef9900".to_string());

    {
        let mut store = DuckGraphStore::open(&db_path).expect("open file store failed");
        store
            .save_file_hashes(&hashes)
            .expect("save_file_hashes failed");
    }

    let store2 = DuckGraphStore::open(&db_path).expect("reopen failed");
    let loaded = store2.load_file_hashes().expect("load_file_hashes failed");

    assert_eq!(loaded, hashes, "file hashes differ after reopen");
}

// ---------------------------------------------------------------------------
// Test 6: Confidence precision (0.9 round-trips within 1e-6)
// ---------------------------------------------------------------------------

#[test]
fn confidence_precision() {
    let mut g = Graph::new();
    g.add_node(make_node(
        "precise_node",
        NodeKind::Function,
        Provenance::Extracted,
        0.9,
    ));
    g.add_edge(make_edge(
        "precise_node",
        "precise_node",
        EdgeKind::Calls,
        Provenance::Inferred,
        0.9,
    ));

    let mut store = DuckGraphStore::open_in_memory().expect("open_in_memory failed");
    store.save_graph(&g).expect("save_graph failed");
    let loaded = store.load_graph().expect("load_graph failed");

    let node = loaded
        .get_node(&Uid("precise_node".to_string()))
        .expect("node not found");
    assert!(
        (node.confidence.value() - 0.9_f32).abs() < 1e-6,
        "node confidence precision: got {}",
        node.confidence.value(),
    );

    // Check the self-edge confidence.
    use strata_core::graph::Direction;
    let edges = loaded.neighbors(&Uid("precise_node".to_string()), Direction::Outgoing, &[]);
    assert_eq!(edges.len(), 1, "expected 1 outgoing edge");
    assert!(
        (edges[0].0.confidence.value() - 0.9_f32).abs() < 1e-6,
        "edge confidence precision: got {}",
        edges[0].0.confidence.value(),
    );
}

// ---------------------------------------------------------------------------
// Test 7: parse-cache empty load returns empty map
// ---------------------------------------------------------------------------

#[test]
fn parse_cache_empty_load() {
    let store = DuckGraphStore::open_in_memory().expect("open_in_memory failed");
    let cache = store.load_parse_cache().expect("load_parse_cache failed");
    assert!(
        cache.is_empty(),
        "parse cache must be empty before any save"
    );
}

// ---------------------------------------------------------------------------
// Test 8: parse-cache round-trip (≥2 entries, reopen, equal map)
// ---------------------------------------------------------------------------

#[test]
fn parse_cache_round_trip_reopen() {
    let tmp = tempfile::tempdir().expect("tempdir failed");
    let db_path = tmp.path().join("cache_test.db");

    let entry_a = ParseCacheEntry {
        hash: "aaaa1111bbbb2222cccc3333dddd4444eeee5555ffff6666aaaa1111bbbb2222".to_string(),
        analyzed: AnalyzedFile {
            symbols: vec![RawSymbol {
                kind: NodeKind::Function,
                name: "foo".into(),
                fqn: "foo".into(),
                container_fqn: None,
                span: Span {
                    start_line: 1,
                    start_col: 0,
                    end_line: 3,
                    end_col: 1,
                },
            }],
            imports: vec![ImportRef {
                specifier: "./utils".into(),
                imported_names: vec!["helper".into()],
                span: Span::default(),
                name_spans: vec![Span::default()],
            }],
            calls: vec![CallRef {
                callee_name: "helper".into(),
                receiver: None,
                enclosing_fqn: "foo".into(),
                span: Span::default(),
                callee_span: Span::default(),
                receiver_is_path: false,
            }],
            routes: vec![],
            http_calls: vec![],
            gql_documents: vec![],
            resolver_entries: vec![],
            sql_candidates: vec![],
            orm_models: vec![],
        },
    };

    let entry_b = ParseCacheEntry {
        hash: "1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef".to_string(),
        analyzed: AnalyzedFile {
            symbols: vec![RawSymbol {
                kind: NodeKind::Method,
                name: "bar".into(),
                fqn: "A.bar".into(),
                container_fqn: Some("A".into()),
                span: Span {
                    start_line: 10,
                    start_col: 2,
                    end_line: 12,
                    end_col: 3,
                },
            }],
            imports: vec![],
            calls: vec![],
            routes: vec![],
            http_calls: vec![],
            gql_documents: vec![],
            resolver_entries: vec![],
            sql_candidates: vec![],
            orm_models: vec![],
        },
    };

    let mut cache_in = BTreeMap::new();
    cache_in.insert("src/a.ts".to_string(), entry_a.clone());
    cache_in.insert("src/b.ts".to_string(), entry_b.clone());

    {
        let mut store = DuckGraphStore::open(&db_path).expect("open file store failed");
        store
            .save_parse_cache(&cache_in)
            .expect("save_parse_cache failed");
        // Drop — flush to disk.
    }

    // Reopen and verify round-trip equality.
    let store2 = DuckGraphStore::open(&db_path).expect("reopen failed");
    let cache_out = store2
        .load_parse_cache()
        .expect("load_parse_cache after reopen failed");

    assert_eq!(
        cache_in, cache_out,
        "parse cache must survive reopen unchanged"
    );
}

// ---------------------------------------------------------------------------
// Test 9: stale schema_version rows are silently ignored (upgrade-safety guard)
// ---------------------------------------------------------------------------

#[test]
fn parse_cache_ignores_stale_schema_version() {
    let tmp = tempfile::tempdir().expect("tempdir failed");
    let db_path = tmp.path().join("stale_version_test.db");

    let stale_version = ANALYZER_SCHEMA_VERSION - 1;

    // Write a row via save_parse_cache (this stamps the current version).
    let entry = ParseCacheEntry {
        hash: "aaaa1111bbbb2222cccc3333dddd4444eeee5555ffff6666aaaa1111bbbb2222".to_string(),
        analyzed: AnalyzedFile {
            symbols: vec![RawSymbol {
                kind: NodeKind::Function,
                name: "stale".into(),
                fqn: "stale".into(),
                container_fqn: None,
                span: Span::default(),
            }],
            imports: vec![],
            calls: vec![],
            routes: vec![],
            http_calls: vec![],
            gql_documents: vec![],
            resolver_entries: vec![],
            sql_candidates: vec![],
            orm_models: vec![],
        },
    };
    let mut cache_in = BTreeMap::new();
    cache_in.insert("src/stale.ts".to_string(), entry);

    {
        let mut store = DuckGraphStore::open(&db_path).expect("open store failed");
        store
            .save_parse_cache(&cache_in)
            .expect("save_parse_cache failed");
    }

    // Now overwrite the schema_version column with a stale value via raw DuckDB.
    {
        let conn = duckdb::Connection::open(&db_path).expect("raw open failed");
        conn.execute(
            "UPDATE parse_cache SET schema_version = ?",
            duckdb::params![stale_version],
        )
        .expect("UPDATE schema_version failed");
    }

    // Reopen via the store and load — the stale row must be invisible.
    let store2 = DuckGraphStore::open(&db_path).expect("reopen failed");
    let cache_out = store2
        .load_parse_cache()
        .expect("load_parse_cache (stale version) failed");

    assert!(
        cache_out.is_empty(),
        "load_parse_cache must return an empty map when all rows have a stale \
         schema_version (got {} entries)",
        cache_out.len()
    );
}
