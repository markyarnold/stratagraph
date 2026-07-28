//! K5: the lexical (tantivy) docs index is written at index time and is
//! searchable — the full `index_repo` → `.strata/docs.idx` → tantivy `Index`
//! path, end to end. `docs_index::write_docs_index`'s own atomicity/schema
//! unit tests live in `crates/strata-index/src/docs_index.rs`; this file
//! proves the INDEX-TIME WIRING: markdown sections, doc comments, and spec
//! descriptions all really do land in the index the real pipeline writes.

use std::path::Path;

use strata_index::index_repo;
use strata_store::DuckGraphStore;
use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::Value;
use tantivy::{Index, TantivyDocument};

/// A minimal hit: just the fields these tests assert on.
struct Hit {
    uid: String,
    name: String,
    path: String,
    anchor: String,
    kind: String,
    snippet_source: String,
}

/// Write `md_content` as a root-level `README.md` in a fresh temp repo (the
/// default markdown collection set — design §3 — picks up any root `*.md`).
fn fixture_repo_with_md(md_content: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("README.md"), md_content).unwrap();
    dir
}

/// Run the real `index_repo` pipeline (the code planes, the knowledge plane,
/// and the K5 docs-index write) against `dir`, using an on-disk store under
/// `dir/.strata/graph.duckdb` (the canonical location `index_impl` expects).
fn run_index(dir: &Path) {
    let strata = dir.join(".strata");
    std::fs::create_dir_all(&strata).unwrap();
    let mut store = DuckGraphStore::open(&strata.join("graph.duckdb")).unwrap();
    index_repo(dir, &mut store).unwrap();
}

/// Open `idx_path` and run `query` over the `body`+`name` fields (the same
/// two fields `search_docs` queries), returning up to `limit` hits ordered by
/// score descending — the read-side shape `strata-mcp`'s `tool_search_docs`
/// uses, reimplemented minimally here so this crate's test does not need to
/// depend on `strata-mcp` (which depends on `strata-index`, not the other way
/// around).
fn open_and_search(idx_path: &Path, query: &str, limit: usize) -> Vec<Hit> {
    let index = Index::open_in_dir(idx_path).unwrap();
    let schema = index.schema();
    let uid_f = schema.get_field("uid").unwrap();
    let name_f = schema.get_field("name").unwrap();
    let path_f = schema.get_field("path").unwrap();
    let anchor_f = schema.get_field("anchor").unwrap();
    let kind_f = schema.get_field("kind").unwrap();
    let body_f = schema.get_field("body").unwrap();

    let reader = index.reader().unwrap();
    let searcher = reader.searcher();
    let query_parser = QueryParser::for_index(&index, vec![body_f, name_f]);
    let parsed = query_parser.parse_query(query).unwrap();
    let top_docs = searcher
        .search(&parsed, &TopDocs::with_limit(limit).order_by_score())
        .unwrap();

    top_docs
        .into_iter()
        .map(|(_score, addr)| {
            let doc: TantivyDocument = searcher.doc(addr).unwrap();
            let get = |f| {
                doc.get_first(f)
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string()
            };
            Hit {
                uid: get(uid_f),
                name: get(name_f),
                path: get(path_f),
                anchor: get(anchor_f),
                kind: get(kind_f),
                snippet_source: get(body_f),
            }
        })
        .collect()
}

#[test]
fn index_time_docs_idx_is_written_and_searchable() {
    let tmp = fixture_repo_with_md("# Retry policy\nAlways use exponential backoff.\n");
    run_index(tmp.path());

    let idx_path = tmp.path().join(".strata").join("docs.idx");
    assert!(idx_path.is_dir(), "index_repo must write .strata/docs.idx");

    let hits = open_and_search(&idx_path, "backoff", 5);
    assert!(!hits.is_empty(), "a distinctive term must find a hit");
    assert_eq!(hits[0].anchor, "retry-policy");
    assert_eq!(hits[0].kind, "section");
    assert_eq!(hits[0].path, "README.md");
    assert!(hits[0].snippet_source.to_lowercase().contains("backoff"));
}

#[test]
fn doc_comment_text_is_indexed_with_kind_doc_comment() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join("src/lib.rs"),
        "/// Trips the circuitbreaker when downstream latency exceeds the threshold.\npub fn trip_circuitbreaker() {}\n",
    )
    .unwrap();
    run_index(dir.path());

    let idx_path = dir.path().join(".strata").join("docs.idx");
    let hits = open_and_search(&idx_path, "circuitbreaker", 5);
    assert!(
        hits.iter().any(|h| h.kind == "doc_comment"),
        "a doc comment's text must be indexed under kind=doc_comment; got {:?}",
        hits.iter().map(|h| &h.kind).collect::<Vec<_>>()
    );
    let hit = hits.iter().find(|h| h.kind == "doc_comment").unwrap();
    assert_eq!(hit.path, "src/lib.rs");
    assert!(hit.name.contains("trip_circuitbreaker"), "{}", hit.name);
    // The uid must be the SAME shape `doc_section_uid` gives the knowledge
    // plane's own doc-comment DocSection node: `doc|<repo>|<path>|<path>#doc:<fqn>|`.
    assert!(hit.uid.contains("#doc:trip_circuitbreaker"), "{}", hit.uid);
}

#[test]
fn spec_description_is_indexed_with_kind_spec_description() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("openapi.json"),
        r#"{"openapi":"3.0.0","paths":{"/widgets":{"get":{"operationId":"listWidgets","summary":"List widgets","description":"Returns a paginated list of quantumwidgets."}}}}"#,
    )
    .unwrap();
    run_index(dir.path());

    let idx_path = dir.path().join(".strata").join("docs.idx");
    let hits = open_and_search(&idx_path, "quantumwidgets", 5);
    assert!(
        hits.iter().any(|h| h.kind == "spec_description"),
        "an OpenAPI description must be indexed under kind=spec_description; got {:?}",
        hits.iter().map(|h| &h.kind).collect::<Vec<_>>()
    );
    let hit = hits.iter().find(|h| h.kind == "spec_description").unwrap();
    assert_eq!(hit.path, "openapi.json");
    assert_eq!(hit.name, "listWidgets");
}

#[test]
fn reindex_atomically_replaces_stale_docs_content() {
    let tmp = fixture_repo_with_md("# Alpha\nThe alphastarterterm content.\n");
    run_index(tmp.path());

    let idx_path = tmp.path().join(".strata").join("docs.idx");
    let first = open_and_search(&idx_path, "alphastarterterm", 5);
    assert_eq!(first.len(), 1, "the original content must be found");

    // Change the doc content and re-index: the old content must be gone and
    // the new content searchable — proves the writer's rename-aside + swap +
    // best-effort stale-dir cleanup (`docs_index::swap_in`/`cleanup_stale_dirs`)
    // is really wired into `index_impl`, not just unit-tested in isolation.
    std::fs::write(
        tmp.path().join("README.md"),
        "# Alpha\nThe betaswappedterm content.\n",
    )
    .unwrap();
    run_index(tmp.path());

    let stale = open_and_search(&idx_path, "alphastarterterm", 5);
    assert!(stale.is_empty(), "stale content must not survive a reindex");
    let fresh = open_and_search(&idx_path, "betaswappedterm", 5);
    assert_eq!(fresh.len(), 1, "the new content must be searchable");

    let strata_dir = tmp.path().join(".strata");
    assert!(
        !strata_dir.join("docs.idx.tmp").exists(),
        "no docs.idx.tmp litter must remain after a successful reindex"
    );
    let stale_leftovers: Vec<_> = std::fs::read_dir(&strata_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("docs.idx.stale-"))
        .collect();
    assert!(
        stale_leftovers.is_empty(),
        "the best-effort stale-dir cleanup must have swept up the renamed-aside old index: {stale_leftovers:?}"
    );
}

#[test]
fn a_repo_with_no_docs_still_gets_a_valid_empty_index() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join("src/a.ts"),
        "export function f() { return 1; }\n",
    )
    .unwrap();
    run_index(dir.path());

    let idx_path = dir.path().join(".strata").join("docs.idx");
    assert!(
        idx_path.is_dir(),
        "docs.idx must exist even for a repo with no markdown/doc comments/spec descriptions"
    );
    let index = Index::open_in_dir(&idx_path).unwrap();
    let reader = index.reader().unwrap();
    assert_eq!(reader.searcher().num_docs(), 0);
}

/// Review fix: a docs-index write failure must no longer be silent — it must
/// surface on `IndexStats::docs_index_warning`, not just an `eprintln!` no one
/// reads (`strata-cli`'s `cmd_index` renders this field in its own summary;
/// covered separately at that layer). The docs-index write is still a
/// best-effort side artifact, though: `index_repo` itself must still return
/// `Ok` and the CODE graph must still be indexed — a sidecar failing to write
/// must never take down an otherwise-successful index run.
///
/// Forced deterministically: pre-create a plain FILE at the exact path
/// `write_docs_index` needs to `create_dir_all` as its `docs.idx.tmp`
/// scratch directory, so the very first fallible step inside it errors out
/// (`remove_dir_all`/`create_dir_all` on a path that is a file, not a
/// directory) — no filesystem permission games needed, fully portable.
#[test]
fn a_docs_index_write_failure_surfaces_on_index_stats_not_just_stderr() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("README.md"), "# H\nbody\n").unwrap();

    let strata = dir.path().join(".strata");
    std::fs::create_dir_all(&strata).unwrap();
    // A FILE (not a directory) sitting where docs.idx.tmp needs to be a
    // directory — write_docs_index's own remove_if_exists/create_dir_all
    // must fail on this.
    std::fs::write(strata.join("docs.idx.tmp"), b"not a directory").unwrap();

    let mut store = DuckGraphStore::open(&strata.join("graph.duckdb")).unwrap();
    let stats = index_repo(dir.path(), &mut store)
        .expect("a docs-index write failure must never fail the surrounding index_repo call");

    let warning = stats
        .docs_index_warning
        .as_deref()
        .expect("a forced docs-index write failure must set docs_index_warning");
    assert!(
        warning.contains("docs index: write failed"),
        "got: {warning}"
    );
    assert!(
        warning.contains("search_docs will serve the previous index"),
        "got: {warning}"
    );

    // The code graph itself still indexed fine — the sidecar failure is
    // fully contained.
    assert!(stats.nodes > 0, "the code/knowledge graph must still build");
}
