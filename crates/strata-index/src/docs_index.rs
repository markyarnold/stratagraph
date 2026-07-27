//! K5: the lexical (tantivy) docs index writer.
//!
//! `write_docs_index` turns a flat batch of [`DocsIndexEntry`] — assembled by
//! the index-time pipeline (`crate::index_impl`) from the three knowledge-plane
//! sources (K2 markdown sections, K3 doc comments, K4 spec descriptions) —
//! into a tantivy full-text index on disk at `<strata_dir>/docs.idx`. It is a
//! **local-only, deterministic lexical** artifact: no ML, no embeddings, and
//! (per the design's "bodies-from-disk" rule) it is the ONE place body text is
//! allowed to be stored at all — never on a graph [`strata_core::Node`] or
//! [`strata_core::Edge`], and never synced as part of a shared/estate artifact
//! (design §4, §8; the graph-sync protocol note pins this).
//!
//! `search_docs` (`strata-mcp`) is the reader; this module only writes.

use std::path::{Path, PathBuf};

use tantivy::schema::{Schema, STORED, STRING, TEXT};
use tantivy::{doc, Index, IndexWriter};

/// The tantivy index directory name inside a repo's `.strata/` directory.
pub const DOCS_INDEX_DIR: &str = "docs.idx";

/// The sibling directory a fresh build is written into before the atomic
/// swap. Never a valid index to open — a reader must only ever look at
/// [`DOCS_INDEX_DIR`].
const DOCS_INDEX_TMP_DIR: &str = "docs.idx.tmp";

/// tantivy's own per-thread minimum ([`tantivy::indexer::index_writer::MEMORY_BUDGET_NUM_BYTES_MIN`],
/// 15MB as of this writing). Passing exactly the minimum keeps the writer
/// single-threaded — fine at repo-doc scale, and it keeps segment creation
/// deterministic (one thread, one commit, no merge-order nondeterminism).
const INDEX_WRITER_MEMORY_BUDGET: usize = 15_000_000;

/// The kind of source a [`DocsIndexEntry`] was assembled from. Serialized
/// into the tantivy `kind` field verbatim via [`DocsEntryKind::as_str`], so a
/// `search_docs` hit can say what it is — never presented as more than a
/// term match (the brief's "labeled lexical" contract).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocsEntryKind {
    /// A markdown heading section (K2 `DocSectionModel`).
    Section,
    /// A doc comment adjacent to a code symbol (K3 `RawSymbol::doc_span`).
    DocComment,
    /// An OpenAPI/GraphQL operation's `description` (K4 `OperationDef`).
    SpecDescription,
}

impl DocsEntryKind {
    /// The exact string stored in the tantivy `kind` field.
    pub fn as_str(self) -> &'static str {
        match self {
            DocsEntryKind::Section => "section",
            DocsEntryKind::DocComment => "doc_comment",
            DocsEntryKind::SpecDescription => "spec_description",
        }
    }
}

/// One document destined for the lexical index.
///
/// `uid` is the SAME uid the corresponding graph node/section carries
/// (`doc_section_uid` for a markdown section or doc comment;
/// `crate::contract::operation_uid` for a spec description) — never a
/// separate identity — so a `search_docs` hit correlates 1:1 with
/// `impact`/`context` on the graph. `body` is transient: it is written into
/// this local tantivy index and nowhere else (bodies-from-disk).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocsIndexEntry {
    pub uid: String,
    pub name: String,
    pub path: String,
    pub anchor: String,
    pub kind: DocsEntryKind,
    pub body: String,
}

/// Errors from building/writing the lexical docs index. Callers (the
/// index-time pipeline) treat this as a best-effort side artifact — a write
/// failure is logged, never propagated as an `IndexError` (mirrors how
/// `stamp::IndexStamp::write`'s failure is handled at the end of `index_impl`:
/// the code graph having indexed successfully must not be undone by a sidecar
/// artifact failing to write).
#[derive(Debug, thiserror::Error)]
pub enum DocsIndexError {
    #[error("io error at {0}: {1}")]
    Io(PathBuf, #[source] std::io::Error),
    #[error("tantivy error: {0}")]
    Tantivy(#[from] tantivy::TantivyError),
}

fn io_err(path: &Path, source: std::io::Error) -> DocsIndexError {
    DocsIndexError::Io(path.to_path_buf(), source)
}

/// Field handles into the schema [`docs_schema`] builds — shared shape
/// between this writer and (independently, by field NAME, not by this type)
/// the `search_docs` reader in `strata-mcp`.
struct DocsFields {
    uid: tantivy::schema::Field,
    name: tantivy::schema::Field,
    path: tantivy::schema::Field,
    anchor: tantivy::schema::Field,
    kind: tantivy::schema::Field,
    body: tantivy::schema::Field,
}

/// The docs-index schema (brief's Interfaces table): `uid`/`path`/`anchor`/
/// `kind` are `STRING | STORED` (exact-match, not tokenized — they are
/// identifiers/labels, not prose); `name` and `body` are `TEXT | STORED` —
/// tokenized so both are searchable, and **stored** because `search_docs`
/// needs `body`'s text back to generate a snippet (a `SnippetGenerator`
/// reads the stored value, not just the postings list) and `name` back to
/// render a hit.
fn docs_schema() -> (Schema, DocsFields) {
    let mut builder = Schema::builder();
    let uid = builder.add_text_field("uid", STRING | STORED);
    let name = builder.add_text_field("name", TEXT | STORED);
    let path = builder.add_text_field("path", STRING | STORED);
    let anchor = builder.add_text_field("anchor", STRING | STORED);
    let kind = builder.add_text_field("kind", STRING | STORED);
    let body = builder.add_text_field("body", TEXT | STORED);
    (
        builder.build(),
        DocsFields {
            uid,
            name,
            path,
            anchor,
            kind,
            body,
        },
    )
}

/// Remove `path` if it exists (directory or file), ignoring a "not found"
/// error (the common, non-error case: nothing to clean up).
fn remove_if_exists(path: &Path) -> Result<(), DocsIndexError> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io_err(path, e)),
    }
}

/// (Re)build `<strata_dir>/docs.idx` from `entries`, atomically.
///
/// Always writes a valid index, even for an empty `entries` slice (a repo
/// with no markdown/doc comments/spec descriptions still gets a real, empty,
/// openable index) — so `search_docs` can tell "indexed, nothing to find"
/// (empty `results`, no note) apart from "never indexed" (empty `results`
/// **plus** a note), which matters for an honest agent-facing tool.
///
/// **Atomicity:** a fresh index is built in the sibling `docs.idx.tmp`
/// directory and `commit()`-ed there in full BEFORE anything touches the
/// real `docs.idx` — only then is the old `docs.idx` (if any) removed and
/// `docs.idx.tmp` renamed over it. A concurrent reader therefore only ever
/// observes the fully-committed OLD index or the fully-committed NEW one,
/// never a half-written one. The brief-noted caveat: `rename` cannot be made
/// atomic together with the preceding `remove` (two syscalls), so there is a
/// small window where `docs.idx` does not exist at all — this is safe by
/// construction because a missing index is already a first-class, honestly
/// reported state (`search_docs` → empty results + a note), never an error.
pub fn write_docs_index(
    strata_dir: &Path,
    entries: &[DocsIndexEntry],
) -> Result<usize, DocsIndexError> {
    let tmp_path = strata_dir.join(DOCS_INDEX_TMP_DIR);
    let final_path = strata_dir.join(DOCS_INDEX_DIR);

    // A stale tmp dir from a previous crashed/killed run would make
    // `Index::create_in_dir` fail with `IndexAlreadyExists` — clear it first
    // so this run starts from a genuinely fresh directory.
    remove_if_exists(&tmp_path)?;
    std::fs::create_dir_all(&tmp_path).map_err(|e| io_err(&tmp_path, e))?;

    let (schema, fields) = docs_schema();
    let index = Index::create_in_dir(&tmp_path, schema)?;
    let mut writer: IndexWriter = index.writer(INDEX_WRITER_MEMORY_BUDGET)?;
    for entry in entries {
        writer.add_document(doc!(
            fields.uid => entry.uid.as_str(),
            fields.name => entry.name.as_str(),
            fields.path => entry.path.as_str(),
            fields.anchor => entry.anchor.as_str(),
            fields.kind => entry.kind.as_str(),
            fields.body => entry.body.as_str(),
        ))?;
    }
    writer.commit()?;
    // Release the writer/index (and their file handles/locks) before the
    // directory-level swap below.
    drop(writer);
    drop(index);

    remove_if_exists(&final_path)?;
    std::fs::rename(&tmp_path, &final_path).map_err(|e| io_err(&final_path, e))?;

    Ok(entries.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tantivy::schema::Value;

    fn entry(uid: &str, kind: DocsEntryKind, body: &str) -> DocsIndexEntry {
        DocsIndexEntry {
            uid: uid.to_string(),
            name: "n".to_string(),
            path: "p.md".to_string(),
            anchor: "a".to_string(),
            kind,
            body: body.to_string(),
        }
    }

    /// Open the freshly-written index and confirm the schema + a stored field
    /// round-trip — the writer's own unit-level correctness (the full
    /// index-then-search path is exercised by the crate's integration test,
    /// `tests/docs_index.rs`, against the real `index_impl` pipeline).
    #[test]
    fn write_docs_index_produces_an_openable_index_with_the_right_schema() {
        let tmp = tempfile::tempdir().unwrap();
        let strata_dir = tmp.path();
        let n = write_docs_index(
            strata_dir,
            &[entry(
                "doc|r|a.md|a.md#h|",
                DocsEntryKind::Section,
                "hello world",
            )],
        )
        .unwrap();
        assert_eq!(n, 1);

        let idx_path = strata_dir.join(DOCS_INDEX_DIR);
        assert!(idx_path.is_dir(), "docs.idx must exist after a write");
        let index = Index::open_in_dir(&idx_path).unwrap();
        let schema = index.schema();
        for field in ["uid", "name", "path", "anchor", "kind", "body"] {
            assert!(
                schema.get_field(field).is_ok(),
                "schema must carry a `{field}` field"
            );
        }

        let reader = index.reader().unwrap();
        let searcher = reader.searcher();
        assert_eq!(searcher.num_docs(), 1);
    }

    /// An empty `entries` slice still produces a real, openable (empty)
    /// index — never skipped — so a repo with no docs is distinguishable from
    /// a repo that was never indexed at all.
    #[test]
    fn write_docs_index_with_no_entries_still_writes_an_empty_valid_index() {
        let tmp = tempfile::tempdir().unwrap();
        let strata_dir = tmp.path();
        let n = write_docs_index(strata_dir, &[]).unwrap();
        assert_eq!(n, 0);

        let idx_path = strata_dir.join(DOCS_INDEX_DIR);
        let index = Index::open_in_dir(&idx_path).unwrap();
        let reader = index.reader().unwrap();
        assert_eq!(reader.searcher().num_docs(), 0);
    }

    /// The atomic swap: re-running `write_docs_index` on the SAME
    /// `strata_dir` fully replaces the old index (old content gone, new
    /// content searchable) and leaves no `docs.idx.tmp` litter behind.
    #[test]
    fn write_docs_index_rebuild_atomically_replaces_the_old_index() {
        let tmp = tempfile::tempdir().unwrap();
        let strata_dir = tmp.path();

        write_docs_index(
            strata_dir,
            &[entry("u1", DocsEntryKind::Section, "alpha content")],
        )
        .unwrap();
        write_docs_index(
            strata_dir,
            &[entry("u2", DocsEntryKind::Section, "beta content")],
        )
        .unwrap();

        let idx_path = strata_dir.join(DOCS_INDEX_DIR);
        let index = Index::open_in_dir(&idx_path).unwrap();
        let reader = index.reader().unwrap();
        let searcher = reader.searcher();
        assert_eq!(
            searcher.num_docs(),
            1,
            "the second build must REPLACE, not append to, the first"
        );

        let uid_field = index.schema().get_field("uid").unwrap();
        let query_parser = tantivy::query::QueryParser::for_index(
            &index,
            vec![index.schema().get_field("body").unwrap()],
        );
        let query = query_parser.parse_query("beta").unwrap();
        let top = searcher
            .search(
                &query,
                &tantivy::collector::TopDocs::with_limit(5).order_by_score(),
            )
            .unwrap();
        assert_eq!(top.len(), 1, "the new content must be searchable");
        let doc: tantivy::TantivyDocument = searcher.doc(top[0].1).unwrap();
        assert_eq!(
            doc.get_first(uid_field).and_then(|v| v.as_str()),
            Some("u2")
        );

        let query = query_parser.parse_query("alpha").unwrap();
        let top = searcher
            .search(
                &query,
                &tantivy::collector::TopDocs::with_limit(5).order_by_score(),
            )
            .unwrap();
        assert!(top.is_empty(), "the old content must be gone");

        assert!(
            !strata_dir.join(DOCS_INDEX_TMP_DIR).exists(),
            "no docs.idx.tmp litter must remain after a successful rebuild"
        );
    }

    /// The `kind` field carries the exact label strings the brief pins.
    #[test]
    fn docs_entry_kind_serializes_to_the_pinned_strings() {
        assert_eq!(DocsEntryKind::Section.as_str(), "section");
        assert_eq!(DocsEntryKind::DocComment.as_str(), "doc_comment");
        assert_eq!(DocsEntryKind::SpecDescription.as_str(), "spec_description");
    }
}
