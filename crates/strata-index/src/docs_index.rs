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
/// real `docs.idx` — only then is it swapped in (see [`swap_in`]) and any
/// now-stale directory is best-effort cleaned up (see [`cleanup_stale_dirs`]).
/// A concurrent reader therefore only ever observes the fully-committed OLD
/// index or the fully-committed NEW one, never a half-written one, AND (the
/// fix this doc comment now covers) the WRITE itself never fails just
/// because a reader has the old index open.
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

    swap_in(strata_dir, &tmp_path, &final_path)?;
    // Best-effort only, by design — see the function doc. Never lets a
    // cleanup failure turn an already-successful write into an `Err`.
    cleanup_stale_dirs(strata_dir);

    Ok(entries.len())
}

/// Atomically swap the freshly-built `tmp_path` into `final_path`.
///
/// **Review fix:** the previous implementation did `remove_dir_all(final)`
/// then `rename(tmp, final)`. That races a concurrent reader constructing an
/// `Index`/`Searcher` over `final` (open/mmap of the segment files under it):
/// empirically reproduced at 26/60 writes failing `DirectoryNotEmpty` under a
/// concurrent reader. `remove_dir_all` is NOT a single syscall — it lists and
/// recursively unlinks — so it can observe (and lose a race against) whatever
/// the reader's own open touches inside that same directory.
///
/// The fix: never recursively delete a directory a reader might have open.
/// If `final_path` exists, rename it ASIDE to a pid-scoped `docs.idx.stale-<pid>`
/// sibling first — a single `rename` syscall, which cannot itself race a
/// reader's already-open file descriptors/mmaps (POSIX: an open fd/mmap stays
/// valid via the underlying inode regardless of what happens to the directory
/// entry that named it — renaming the directory does not invalidate anything
/// a reader already has open). Only THEN rename `tmp_path` into `final_path`.
/// Deleting the now-stale directory is a separate, later, best-effort step
/// ([`cleanup_stale_dirs`]) — never part of this function's success/failure.
fn swap_in(strata_dir: &Path, tmp_path: &Path, final_path: &Path) -> Result<(), DocsIndexError> {
    if final_path.exists() {
        let stale = stale_dir_path(strata_dir, std::process::id());
        // A same-pid leftover here would only happen if THIS exact pid
        // previously crashed between this rename and its own cleanup sweep —
        // vanishingly unlikely, but clear it first so this rename can never
        // itself fail on an existing destination.
        let _ = std::fs::remove_dir_all(&stale);
        std::fs::rename(final_path, &stale).map_err(|e| io_err(final_path, e))?;
    }
    std::fs::rename(tmp_path, final_path).map_err(|e| io_err(final_path, e))
}

/// The pid-scoped stale-directory path `swap_in` renames an outgoing
/// `docs.idx` aside to, and [`cleanup_stale_dirs`] later sweeps up.
fn stale_dir_path(strata_dir: &Path, pid: u32) -> PathBuf {
    strata_dir.join(format!("{DOCS_INDEX_DIR}.stale-{pid}"))
}

/// The glob-ish prefix every stale directory [`stale_dir_path`] produces
/// starts with — used by [`cleanup_stale_dirs`] to recognize them regardless
/// of which pid wrote them.
const STALE_DIR_PREFIX: &str = "docs.idx.stale-";

/// Best-effort sweep of every `docs.idx.stale-*` sibling in `strata_dir` —
/// this run's own (just produced by `swap_in`, above) AND any leftover from a
/// PREVIOUS run that failed to clean up (e.g. a reader still had it open at
/// the time, the process was killed before this step ran, or the filesystem
/// hiccupped). A failure removing any one of them is silently swallowed and
/// never propagated: the swap itself already succeeded — the fresh index is
/// already live at `docs.idx` — so a stale directory that resists deletion
/// this run is just harmless disk clutter; the next successful write's sweep
/// gets another chance at it. Never fails, never panics: a `strata_dir` that
/// cannot even be listed is also silently skipped.
fn cleanup_stale_dirs(strata_dir: &Path) {
    let Ok(read_dir) = std::fs::read_dir(strata_dir) else {
        return;
    };
    for entry in read_dir.flatten() {
        if entry
            .file_name()
            .to_string_lossy()
            .starts_with(STALE_DIR_PREFIX)
        {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
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

    /// Review finding (Important, empirically reproduced: 26/60 writes failed
    /// `DirectoryNotEmpty` under the OLD `remove_dir_all(final) +
    /// rename(tmp, final)` swap). A rebuild must succeed even while a reader
    /// still holds an open `Searcher` over the CURRENT index (mmapped segment
    /// files) — and a FRESH open after the rebuild must see the new content,
    /// proving the swap actually replaced it rather than merely tolerating
    /// the old reader by leaving the old content in place.
    #[test]
    fn write_docs_index_rebuild_succeeds_with_a_concurrent_reader_holding_a_searcher() {
        let tmp = tempfile::tempdir().unwrap();
        let strata_dir = tmp.path();

        write_docs_index(
            strata_dir,
            &[entry("u1", DocsEntryKind::Section, "alpha content")],
        )
        .unwrap();

        // Open a reader on the CURRENT index and keep its Searcher alive
        // across the rebuild below — this is exactly the outstanding handle
        // the review's race was against.
        let idx_path = strata_dir.join(DOCS_INDEX_DIR);
        let held_index = Index::open_in_dir(&idx_path).unwrap();
        let held_reader = held_index.reader().unwrap();
        let _held_searcher = held_reader.searcher(); // held alive for the whole test

        let result = write_docs_index(
            strata_dir,
            &[entry("u2", DocsEntryKind::Section, "beta content")],
        );
        assert!(
            result.is_ok(),
            "a rebuild must never fail because of a concurrent reader's open Searcher: {result:?}"
        );

        // A FRESH open (not `held_index`/`held_reader` above) must see the
        // NEW content — the swap really replaced the index, it did not just
        // avoid erroring while secretly leaving the old one in place.
        let fresh_index = Index::open_in_dir(&idx_path).unwrap();
        let fresh_reader = fresh_index.reader().unwrap();
        let fresh_searcher = fresh_reader.searcher();
        let body_field = fresh_index.schema().get_field("body").unwrap();
        let query_parser = tantivy::query::QueryParser::for_index(&fresh_index, vec![body_field]);
        let query = query_parser.parse_query("beta").unwrap();
        let top = fresh_searcher
            .search(
                &query,
                &tantivy::collector::TopDocs::with_limit(5).order_by_score(),
            )
            .unwrap();
        assert_eq!(
            top.len(),
            1,
            "a fresh open after rebuild must see the new content"
        );
    }

    /// Review finding (Important, part 3 of the prescribed fix): a stale-dir
    /// cleanup failure must never fail the write — the swap (the part that
    /// matters) already succeeded by the time cleanup runs. Simulated by
    /// leaving a leftover "previous run" stale dir whose contents resist
    /// deletion (write permission stripped from the directory itself, so its
    /// child file cannot be unlinked).
    #[test]
    #[cfg(unix)]
    fn write_docs_index_succeeds_even_when_stale_dir_cleanup_fails() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let strata_dir = tmp.path();

        write_docs_index(strata_dir, &[entry("u1", DocsEntryKind::Section, "seed")]).unwrap();

        // A leftover stale dir from a "previous run" (a fake pid — never the
        // real one, so it can never collide with this run's own stale-aside
        // dir) that cannot be deleted: strip write permission from the
        // directory itself, so unlinking the file inside it fails.
        let undeletable = stale_dir_path(strata_dir, 999_999_999);
        std::fs::create_dir_all(&undeletable).unwrap();
        std::fs::write(undeletable.join("segment"), b"leftover").unwrap();
        std::fs::set_permissions(&undeletable, std::fs::Permissions::from_mode(0o555)).unwrap();

        let result = write_docs_index(
            strata_dir,
            &[entry("u2", DocsEntryKind::Section, "rebuilt")],
        );

        // Restore permissions unconditionally so the tempdir's own Drop
        // cleanup does not itself fail, regardless of the assertions below.
        std::fs::set_permissions(&undeletable, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert!(
            result.is_ok(),
            "a stale-dir cleanup failure must never fail the write: {result:?}"
        );
        assert!(
            undeletable.exists(),
            "test premise: the undeletable stale dir must still be there (cleanup genuinely failed, \
             not merely skipped)"
        );

        // The write itself still succeeded end to end: the new content is
        // live and searchable despite the cleanup failure alongside it.
        let idx_path = strata_dir.join(DOCS_INDEX_DIR);
        let index = Index::open_in_dir(&idx_path).unwrap();
        let reader = index.reader().unwrap();
        assert_eq!(reader.searcher().num_docs(), 1);
    }

    /// A scaled-down re-run of the reviewer's concurrency probe against the
    /// NEW swap sequence: a reader thread continuously opens the index and
    /// constructs a `Searcher` (the exact operation the original race was
    /// against) while the main thread concurrently rebuilds the index several
    /// times. Kept small (20 rebuild iterations, a tight but bounded reader
    /// loop) for CI speed/determinism — this is a regression guard, not a
    /// statistical reproduction of the original 60-write/26-failure measurement.
    /// The only hard assertion is "no write ever fails"; a reader racing a
    /// swap and transiently seeing "not found" is expected/harmless (the same
    /// honest-missing-index state `search_docs` already tolerates) and is
    /// silently ignored, not counted as a failure.
    #[test]
    fn concurrent_reader_never_causes_a_write_failure_under_the_new_swap() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let tmp = tempfile::tempdir().unwrap();
        let strata_dir = tmp.path().to_path_buf();
        write_docs_index(
            &strata_dir,
            &[entry("seed", DocsEntryKind::Section, "seed content")],
        )
        .unwrap();

        let idx_path = strata_dir.join(DOCS_INDEX_DIR);
        let stop = Arc::new(AtomicBool::new(false));

        let reader_idx_path = idx_path.clone();
        let reader_stop = Arc::clone(&stop);
        let reader_thread = std::thread::spawn(move || {
            let mut successful_opens = 0usize;
            while !reader_stop.load(Ordering::Relaxed) {
                if let Ok(index) = Index::open_in_dir(&reader_idx_path) {
                    if let Ok(reader) = index.reader() {
                        let _searcher = reader.searcher();
                        successful_opens += 1;
                    }
                }
            }
            successful_opens
        });

        let mut failures: Vec<String> = Vec::new();
        for i in 0..20 {
            let body = format!("generation {i} content");
            if let Err(e) =
                write_docs_index(&strata_dir, &[entry("seed", DocsEntryKind::Section, &body)])
            {
                failures.push(e.to_string());
            }
        }
        stop.store(true, Ordering::Relaxed);
        let successful_opens = reader_thread.join().unwrap();

        assert!(
            failures.is_empty(),
            "no write may fail under a concurrently-opening reader: {failures:?}"
        );
        assert!(
            successful_opens > 0,
            "sanity check that the race was actually exercised: the reader thread never got a \
             single successful open in"
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
