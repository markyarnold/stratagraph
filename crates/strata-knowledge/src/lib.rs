//! strata-knowledge: pure markdown parsing into section models with
//! extracted references.
//!
//! This crate has **no filesystem access** — callers read the file and hand
//! this crate `(path, content)`; `path` is carried through purely as a label
//! for the resulting [`DocModel`]. It is the foundation for the knowledge
//! plane: K2 builds `Doc`/`DocSection` graph nodes and `Mentions` edges from
//! a `DocModel`, and K5 indexes each section's body straight from disk using
//! [`DocSectionModel::body_range`] — per the "bodies-from-disk" rule, no
//! document body text is ever stored in the graph, the store, or any
//! artifact here. Only spans and short extracted ref strings are.
//!
//! Sections are **flat** in this milestone: every heading, regardless of
//! level, closes the previously open section and opens a new one. A
//! same-or-higher-level nesting model is a future milestone, not this one.

use std::collections::{HashMap, HashSet};

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use strata_core::Span;

/// One parsed markdown document: its sections in document order.
#[derive(Debug, Clone, PartialEq)]
pub struct DocModel {
    pub path: String,
    pub sections: Vec<DocSectionModel>,
}

/// One section of a document, delimited by a heading (or by the start of the
/// document for the synthetic preamble section) and the next heading (or end
/// of document).
#[derive(Debug, Clone, PartialEq)]
pub struct DocSectionModel {
    /// Heading text; `"(preamble)"` for content before the first heading.
    pub heading: String,
    /// GitHub-style slug of `heading`, deduped with `-1`/`-2` suffixes for
    /// repeats within the same document; `"preamble"` for the preamble.
    pub anchor: String,
    /// Heading line .. line before the next heading (1-based lines, 0-based
    /// columns — matching the workspace `Span` convention).
    pub span: Span,
    /// Byte offsets `(start, end)` into the original `content` string
    /// covering this section, heading through end of body. Used by K5 to
    /// index the section's text straight from disk — the body text itself
    /// is never captured or stored by this crate.
    pub body_range: (usize, usize),
    pub refs: Vec<DocRef>,
}

/// The kind of thing a [`DocRef`] points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DocRefKind {
    /// A single-line inline code span, e.g. `` `strata_core::impact` ``.
    InlineCode,
    /// An identifier-shaped token pulled out of a fenced code block.
    FenceToken,
    /// A filesystem-path-shaped reference: a link destination, or an inline
    /// code span that itself looks like a path (contains `/` and a
    /// dot-extension).
    PathRef,
}

/// One reference extracted from a section's markdown, deduped within that
/// section by `(kind, text)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocRef {
    pub text: String,
    pub kind: DocRefKind,
}

/// Per-section cap on extracted refs. Dedup preserves first-seen order;
/// anything past this cap is dropped rather than letting a pathological
/// document (e.g. hundreds of inline-code spans) grow the ref list without
/// bound. See the K1 test `refs_are_deduped_and_capped`.
const MAX_REFS_PER_SECTION: usize = 64;

/// Refs shorter than this are noise (`if`, `x`, `g`, ...) — see the K1 test
/// `inline_code_and_fence_tokens_and_paths_are_extracted`.
const MIN_REF_LEN: usize = 3;

/// Inline code spans longer than this stop looking like a symbol/path
/// reference and start looking like an embedded snippet; they are not
/// extracted as refs.
const MAX_INLINE_CODE_LEN: usize = 80;

/// GitHub-style heading slug: ascii-alphanumerics are lowercased and kept,
/// spaces and hyphens become `-`, everything else (punctuation) is dropped.
/// This is the anchor half of a section's identity — anchors are re-derived
/// from headings via this function, never stored verbatim, so this function
/// IS the contract UIDs are built on (see the plan's UID-stability note).
pub fn github_slug(heading: &str) -> String {
    heading
        .chars()
        .filter_map(|c| match c {
            c if c.is_ascii_alphanumeric() => Some(c.to_ascii_lowercase()),
            ' ' | '-' => Some('-'),
            _ => None,
        })
        .collect()
}

/// Maps byte offsets (as produced by `Parser::into_offset_iter`) to 1-based
/// line / 0-based column pairs, matching the workspace `Span` convention.
struct LineIndex {
    /// Byte offset of the start of each line; `line_starts[0] == 0`.
    line_starts: Vec<usize>,
}

impl LineIndex {
    fn new(content: &str) -> Self {
        let mut line_starts = vec![0];
        for (i, b) in content.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i + 1);
            }
        }
        LineIndex { line_starts }
    }

    /// 1-based line, 0-based column for a byte offset into the original text.
    fn line_col(&self, offset: usize) -> (u32, u32) {
        let idx = match self.line_starts.binary_search(&offset) {
            Ok(i) => i,
            Err(i) => i - 1,
        };
        (idx as u32 + 1, (offset - self.line_starts[idx]) as u32)
    }
}

/// Collects a section's refs: dedups by `(kind, text)` preserving
/// first-seen order, capped at [`MAX_REFS_PER_SECTION`].
#[derive(Default)]
struct RefCollector {
    seen: HashSet<(DocRefKind, String)>,
    refs: Vec<DocRef>,
}

impl RefCollector {
    fn push(&mut self, kind: DocRefKind, text: &str) {
        if self.refs.len() >= MAX_REFS_PER_SECTION {
            return;
        }
        if self.seen.insert((kind, text.to_string())) {
            self.refs.push(DocRef {
                text: text.to_string(),
                kind,
            });
        }
    }

    fn into_vec(self) -> Vec<DocRef> {
        self.refs
    }
}

/// The section currently being accumulated while walking events.
struct Open {
    heading: String,
    anchor: String,
    start_line: u32,
    start_col: u32,
    start_byte: usize,
    is_preamble: bool,
    refs: RefCollector,
}

/// Closes `open`, converting it into a [`DocSectionModel`] and pushing it
/// onto `sections` — unless it is the preamble and turned out to be blank,
/// in which case it is dropped (preamble is only a section "if non-blank").
///
/// `raw_end` is the byte offset where the section ends: the next heading's
/// start byte, or `content.len()` for the last section in the document.
fn finish_section(
    sections: &mut Vec<DocSectionModel>,
    line_index: &LineIndex,
    content: &str,
    open: Open,
    raw_end: usize,
) {
    let is_blank = content
        .get(open.start_byte..raw_end)
        .map(|s| s.trim().is_empty())
        .unwrap_or(true);
    if open.is_preamble && is_blank {
        return;
    }

    // The reported end position lands at the end of the last actual content
    // line, not on the byte where the *next* heading starts — back off a
    // trailing newline so `end_line`/`end_col` read as "end of the line
    // before the next heading", matching the K1 test contract.
    let close_at =
        if raw_end > open.start_byte && content.as_bytes().get(raw_end - 1) == Some(&b'\n') {
            raw_end - 1
        } else {
            raw_end
        };
    let (end_line, end_col) = line_index.line_col(close_at.max(open.start_byte));

    sections.push(DocSectionModel {
        heading: open.heading,
        anchor: open.anchor,
        span: Span {
            start_line: open.start_line,
            start_col: open.start_col,
            end_line,
            end_col,
        },
        body_range: (open.start_byte, raw_end),
        refs: open.refs.into_vec(),
    });
}

/// `true` if `s` contains a `/` and its last path segment has a
/// dot-extension shape (e.g. `src/lib.rs`, not `src/build` or `a.b`).
fn has_path_shape(s: &str) -> bool {
    if !s.contains('/') {
        return false;
    }
    let last_segment = s.rsplit('/').next().unwrap_or(s);
    last_segment
        .rsplit_once('.')
        .is_some_and(|(_, ext)| !ext.is_empty() && ext.chars().all(|c| c.is_ascii_alphanumeric()))
}

/// Inline code span (`Event::Code`): trimmed, single-line, length in
/// `MIN_REF_LEN..=MAX_INLINE_CODE_LEN` → `InlineCode`. If the trimmed text
/// also looks like a path (contains `/` and a dot-extension), it is *also*
/// recorded as a `PathRef` (same text, different kind).
fn add_inline_code_ref(refs: &mut RefCollector, raw: &str) {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.contains('\n') {
        return;
    }
    let len = trimmed.chars().count();
    if !(MIN_REF_LEN..=MAX_INLINE_CODE_LEN).contains(&len) {
        return;
    }
    refs.push(DocRefKind::InlineCode, trimmed);
    if has_path_shape(trimmed) {
        refs.push(DocRefKind::PathRef, trimmed);
    }
}

/// Link destination (`Tag::Link { dest_url, .. }`): a scheme-less,
/// not-purely-a-fragment destination containing `/` or `.` → `PathRef`.
fn add_link_ref(refs: &mut RefCollector, dest: &str) {
    if dest.contains("://") || dest.starts_with('#') {
        return;
    }
    if dest.contains('/') || dest.contains('.') {
        refs.push(DocRefKind::PathRef, dest);
    }
}

/// `true` if `token` is worth keeping as a ref on its own: has an ascii
/// letter (excludes pure numbers/punctuation runs) and is at least
/// [`MIN_REF_LEN`] chars (excludes noise like `if`, `x`, `g`).
fn is_qualifying_token(token: &str) -> bool {
    token.chars().count() >= MIN_REF_LEN && token.chars().any(|c| c.is_ascii_alphabetic())
}

/// Splits a qualified token (one containing `::` or `.`) into its
/// `::`/`.`-separated segments, e.g. `strata_core::impact` → `["strata_core",
/// "impact"]`, `foo.bar.rs` → `["foo", "bar", "rs"]`.
fn split_segments(token: &str) -> Vec<&str> {
    token
        .split("::")
        .flat_map(|part| part.split('.'))
        .filter(|s| !s.is_empty())
        .collect()
}

/// One token from a fenced code block's tokenizer. A qualifying token is
/// kept whole; if it also contains `::` or `.` (a "qualified" token, e.g. a
/// module path or a dotted call), its segments are extracted too so
/// `strata_core::impact` yields both the whole path and `strata_core` /
/// `impact` individually.
fn add_fence_token(refs: &mut RefCollector, token: &str) {
    if !is_qualifying_token(token) {
        return;
    }
    refs.push(DocRefKind::FenceToken, token);
    if token.contains("::") || token.contains('.') {
        for segment in split_segments(token) {
            if is_qualifying_token(segment) {
                refs.push(DocRefKind::FenceToken, segment);
            }
        }
    }
}

/// Tokenizes a fenced code block's text on any byte outside
/// `[A-Za-z0-9_:.]` and records each qualifying token (and, for qualified
/// tokens, their segments) as a `FenceToken` ref.
fn add_fence_tokens(refs: &mut RefCollector, text: &str) {
    for token in
        text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == ':' || c == '.'))
    {
        if !token.is_empty() {
            add_fence_token(refs, token);
        }
    }
}

/// Parses `content` (the text of the markdown file at `path`) into a
/// [`DocModel`]. Pure function: no filesystem access, no globals — `path`
/// is only ever used as a label on the returned model.
pub fn parse_markdown(path: &str, content: &str) -> DocModel {
    let line_index = LineIndex::new(content);
    let mut sections = Vec::new();
    let mut anchor_counts: HashMap<String, u32> = HashMap::new();
    // Reserve "preamble" up front so a real heading that happens to slug to
    // the same text (e.g. a heading literally titled "Preamble") is
    // suffixed instead of silently colliding with the preamble's anchor.
    anchor_counts.insert("preamble".to_string(), 1);

    let mut current = Open {
        heading: "(preamble)".to_string(),
        anchor: "preamble".to_string(),
        start_line: 1,
        start_col: 0,
        start_byte: 0,
        is_preamble: true,
        refs: RefCollector::default(),
    };

    let mut in_heading = false;
    let mut heading_text = String::new();
    let mut heading_start_line = 1u32;
    let mut heading_start_col = 0u32;
    let mut heading_start_byte = 0usize;

    let mut in_code_block = false;
    let mut code_block_text = String::new();

    for (event, range) in Parser::new_ext(content, Options::empty()).into_offset_iter() {
        match event {
            Event::Start(Tag::Heading { .. }) => {
                in_heading = true;
                heading_text.clear();
                let (line, col) = line_index.line_col(range.start);
                heading_start_line = line;
                heading_start_col = col;
                heading_start_byte = range.start;
            }
            Event::End(TagEnd::Heading(_)) => {
                in_heading = false;
                let base = github_slug(&heading_text);
                let count = anchor_counts.entry(base.clone()).or_insert(0);
                let anchor = if *count == 0 {
                    base.clone()
                } else {
                    format!("{base}-{count}")
                };
                *count += 1;

                let new_open = Open {
                    heading: heading_text.clone(),
                    anchor,
                    start_line: heading_start_line,
                    start_col: heading_start_col,
                    start_byte: heading_start_byte,
                    is_preamble: false,
                    refs: RefCollector::default(),
                };
                let closed = std::mem::replace(&mut current, new_open);
                finish_section(
                    &mut sections,
                    &line_index,
                    content,
                    closed,
                    heading_start_byte,
                );
            }
            Event::Text(t) if in_heading => heading_text.push_str(&t),
            Event::Code(t) if in_heading => heading_text.push_str(&t),
            Event::Code(t) => add_inline_code_ref(&mut current.refs, &t),
            Event::Start(Tag::CodeBlock(_)) => {
                in_code_block = true;
                code_block_text.clear();
            }
            Event::Text(t) if in_code_block => code_block_text.push_str(&t),
            Event::End(TagEnd::CodeBlock) => {
                in_code_block = false;
                add_fence_tokens(&mut current.refs, &code_block_text);
            }
            Event::Start(Tag::Link { dest_url, .. }) => add_link_ref(&mut current.refs, &dest_url),
            _ => {}
        }
    }
    finish_section(&mut sections, &line_index, content, current, content.len());

    DocModel {
        path: path.to_string(),
        sections,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sections_split_on_headings_with_github_anchors() {
        let m = parse_markdown(
            "docs/a.md",
            "# Alpha One\nbody\n## Beta & Gamma!\nmore\n# Alpha One\ntail\n",
        );
        let a: Vec<(&str, &str)> = m
            .sections
            .iter()
            .map(|s| (s.heading.as_str(), s.anchor.as_str()))
            .collect();
        assert_eq!(
            a,
            vec![
                ("Alpha One", "alpha-one"),
                ("Beta & Gamma!", "beta--gamma"),
                ("Alpha One", "alpha-one-1")
            ]
        );
        assert_eq!(m.sections[0].span.start_line, 1);
        assert_eq!(m.sections[0].span.end_line, 2); // ends before the ## heading
    }

    #[test]
    fn preamble_before_first_heading_is_a_section() {
        let m = parse_markdown("README.md", "intro line with `alpha`\n\n# First\n");
        assert_eq!(m.sections[0].anchor, "preamble");
        assert!(m.sections[0].refs.iter().any(|r| r.text == "alpha"));
    }

    #[test]
    fn inline_code_and_fence_tokens_and_paths_are_extracted() {
        let md = "# S\nUse `strata_core::impact` on `docs`.\nSee [x](src/lib.rs).\n```rust\nlet g = build_graph(repo);\nif x { }\n```\n";
        let m = parse_markdown("d.md", md);
        let refs = &m.sections[0].refs;
        let texts: Vec<&str> = refs.iter().map(|r| r.text.as_str()).collect();
        assert!(texts.contains(&"strata_core::impact"), "{texts:?}");
        assert!(texts.contains(&"docs"));
        assert!(
            texts.contains(&"src/lib.rs"),
            "link destination is a PathRef"
        );
        assert!(
            texts.contains(&"build_graph"),
            "fence identifiers extracted"
        );
        assert!(
            !texts.contains(&"if"),
            "short/keyword-ish tokens (<3 chars) excluded"
        );
    }

    #[test]
    fn refs_are_deduped_and_capped() {
        let many: String = (0..200).map(|i| format!("`sym{i}` ")).collect();
        let m = parse_markdown("d.md", &format!("# S\n{many}"));
        assert!(m.sections[0].refs.len() <= 64, "per-section ref cap");
        let m2 = parse_markdown("d.md", "# S\n`same` and `same` again\n");
        assert_eq!(
            m2.sections[0]
                .refs
                .iter()
                .filter(|r| r.text == "same")
                .count(),
            1
        );
    }
}
