//! K2: the knowledge-plane builder. Turns parsed markdown
//! (`strata_knowledge::DocModel`, K1) into graph citizens: `Doc`/`DocSection`
//! nodes, `Doc —Contains→ DocSection` structural edges, and banded `Mentions`
//! edges to whatever a section's extracted refs resolve to. Unresolvable refs
//! are counted (`stale_doc_mentions`), never guessed into a phantom edge — the
//! "never confidently wrong" rule applies to what the repo says about itself
//! (design `docs/specs/2026-07-13-knowledge-plane-design.md` §2).
//!
//! **Bodies-from-disk:** this module never reads or stores a section's body
//! text — only the [`strata_core::Span`]s `parse_markdown` already computed
//! and the short extracted ref strings on `strata_knowledge::DocRef`. K5 reads
//! bodies from the working tree at query time, straight from
//! `DocSectionModel::body_range`.

use std::collections::{BTreeMap, HashMap, HashSet};

use strata_core::{
    AnalyzedFile, Confidence, Edge, EdgeKind, Graph, Node, NodeKind, Provenance, Uid,
};
use strata_knowledge::{DocModel, DocRef, DocRefKind, DocSectionModel};

/// The knowledge-plane UID language tag, alongside `"ts"`/`"py"`/`"cs"`/`"rust"`/
/// `"contract"`/`"infra"` — the design's "new UID namespace `doc`" decision.
const LANG: &str = "doc";

/// `Doc —Contains→ DocSection` containment confidence: a syntactic fact (this
/// section IS part of this file), graded at the ceiling — exactly how the infra
/// `ApiId` containment precedent (`infra::add_contains_edge`'s Resource tier)
/// grades a same-template structural fact. Never impact-traversed (see
/// `strata_core::traverse::reverse_walk`'s knowledge-edge comment).
const CONTAINS_CONF: f32 = 1.0;

// ── confidence bands (design §2 table; band-guardrail-tested) ────────────────

/// `Mentions` via an exact repo-relative path reference (a markdown link
/// destination, or a path-shaped inline-code span) — Extracted: the ref NAMES
/// the exact file, no inference involved.
pub const KNOW_MENTION_PATH: f32 = 0.95;
/// `Mentions` via a unique fully-qualified-name match: a code-fence token or
/// inline-code span whose text equals exactly one node's `fqn` — Inferred.
pub const KNOW_MENTION_FQN: f32 = 0.80;
/// `Mentions` via a unique bare-name match (tried only after an `fqn` miss,
/// and only for a [`DocRefKind::InlineCode`] ref — a fence token never falls
/// through to this tier, F1): an inline-code span whose text equals exactly
/// one node's `name` — Inferred, graded below the `fqn` tier because a bare
/// name is a weaker, more collision-prone signal.
pub const KNOW_MENTION_NAME: f32 = 0.70;
/// `Mentions` fan-out when a reference matches MULTIPLE candidates at whichever
/// tier resolved it (path, fqn, or name) — one edge per candidate, all
/// Ambiguous. Never a confident pick among several.
pub const KNOW_AMBIGUOUS: f32 = 0.35;
/// `Documents` via a doc comment SYNTACTICALLY adjacent to its symbol's
/// declaration (`RawSymbol::doc_span`, K3) — Extracted: the adjacency is a
/// syntactic fact the parser observed directly, not an inference, so it grades
/// at the same ceiling tier as an exact path reference.
pub const KNOW_DOC_COMMENT: f32 = 0.95;

/// Coverage + drift counts [`build_knowledge_plane`] returns — the `knowledge:`
/// summary line's data, and (from K3 on) the vehicle for doc-comment counts.
///
/// `mentions_linked` and `mentions_ambiguous` are NOT disjoint:
/// `mentions_ambiguous` is the SUBSET of `mentions_linked` whose reference
/// resolved to 2+ candidates (fanned out) rather than one confident hit —
/// mirroring the design's own worked example, "480 mentions linked (12
/// ambiguous), 9 stale". `stale_doc_mentions` IS disjoint from both (a
/// reference that matched nothing at any tier) — see its own doc comment for
/// the K7 fix F2 shape-based carve-out; [`unresolved_plain_refs`](Self::unresolved_plain_refs)
/// is disjoint from all three (an unresolvable reference lands in exactly one
/// of `stale_doc_mentions`/`unresolved_plain_refs`, never both).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct KnowledgeLinkCoverage {
    /// `Doc` nodes created (one per ingested markdown file).
    pub docs: usize,
    /// `DocSection` nodes created (one per parsed heading, plus the non-blank
    /// preamble).
    pub sections: usize,
    /// References that resolved to at least one `Mentions` edge — a unique hit
    /// OR an ambiguous fan-out both count here (see the struct-level note).
    pub mentions_linked: usize,
    /// The subset of [`mentions_linked`](Self::mentions_linked) that resolved
    /// to 2+ candidates (Ambiguous 0.35 fan-out, one edge per candidate).
    pub mentions_ambiguous: usize,
    /// References that matched NOTHING at any tier AND read as an authorial
    /// claim about something that should exist — counted, never edged (the
    /// doc-drift signal: this reference is lying about what exists). A
    /// [`DocRefKind::PathRef`] miss is always counted here (an exact path
    /// claim that resolves nowhere). A [`DocRefKind::InlineCode`] miss is
    /// counted here ONLY when its text is symbol-shaped (K7 fix F2 —
    /// `inline_code_looks_symbol_shaped`: contains `::`/`.`, or is
    /// compound-case); a plain-word/`SCREAMING_SNAKE_CASE` `InlineCode` miss
    /// is schema-invisible, not drift, and goes to
    /// [`unresolved_plain_refs`](Self::unresolved_plain_refs) instead. A
    /// [`DocRefKind::FenceToken`] miss is never counted anywhere (unchanged —
    /// incidental example vocabulary, not an authorial claim).
    pub stale_doc_mentions: usize,
    /// K7 fix F2 (reviewer-recommended refinement, controller-adopted): an
    /// unresolvable [`DocRefKind::InlineCode`] reference whose text is NOT
    /// symbol-shaped — a bare all-lowercase word or a `SCREAMING_SNAKE_CASE`
    /// token (a constant, a config key, a CLI flag, or prose that merely sits
    /// in backticks). The graph has no node for a raw constant/config-key, so
    /// this is not evidence the doc is lying (drift) — only evidence the
    /// graph's own reach stops at named, structured symbols. NEVER folded into
    /// [`stale_doc_mentions`](Self::stale_doc_mentions); tracked separately so
    /// the drift metric stays meaningful instead of being swamped by every
    /// bare word an author ever wrapped in backticks (the reviewer's fitness
    /// analysis found ~80% of sampled `stale_doc_mentions` misses, pre-fix,
    /// were exactly this schema-invisible shape).
    pub unresolved_plain_refs: usize,
    /// Doc-comment `Documents` edges. Always `0` until K3 wires `doc_span`
    /// across the four analyzers; the field lives here now so
    /// `KnowledgeLinkCoverage` is K3-ready without another schema bump.
    pub doc_comments: usize,
}

/// The `doc` UID for the `Doc` node of the markdown file at `path`: fqn == the
/// path itself (design table: a `Doc`'s fqn IS its repo-relative path),
/// mirroring [`crate::build::uid_module`]'s shape but with the real path as the
/// fqn rather than a synthetic `<module>` marker (a `Doc`, unlike a code
/// `Module`, has no separate qualified name — the path already is one).
fn doc_uid(repo: &str, path: &str) -> Uid {
    Uid::new(LANG, repo, path, path, "")
}

/// The `doc` UID for a `DocSection` node: `doc|<repo>|<path>|<path>#<anchor>|`
/// — the plan's pinned UID-stability shape. GitHub-style anchors (deduped
/// `-1`/`-2` by `strata_knowledge::parse_markdown`) make this uid stable across
/// unrelated edits elsewhere in the file (a graph-sync obligation).
pub fn doc_section_uid(repo: &str, path: &str, anchor: &str) -> Uid {
    Uid::new(LANG, repo, path, &format!("{path}#{anchor}"), "")
}

/// The base file name (text after the last `/`), or the whole path if none —
/// a `Doc` node's `name`. A tiny local copy rather than reaching into
/// `crate::build`'s private `base_name`: one file's naming convention should
/// not become a cross-module coupling for three lines of logic.
fn file_name(path: &str) -> &str {
    match path.rfind('/') {
        Some(idx) => &path[idx + 1..],
        None => path,
    }
}

/// Build the `Doc` node for one ingested markdown file (design table: `name` =
/// filename, `fqn` = `path` = repo-relative path, `span` = the whole file —
/// approximated here as the union of its sections' spans, since `DocModel`
/// does not carry a separate "file span"; a doc with no sections at all, e.g.
/// an empty or all-blank file, falls back to [`strata_core::Span::default`]).
fn doc_node(uid: Uid, path: &str, doc: &DocModel) -> Node {
    let span = match (doc.sections.first(), doc.sections.last()) {
        (Some(first), Some(last)) => strata_core::Span {
            start_line: first.span.start_line,
            start_col: first.span.start_col,
            end_line: last.span.end_line,
            end_col: last.span.end_col,
        },
        _ => strata_core::Span::default(),
    };
    Node {
        uid,
        kind: NodeKind::Doc,
        name: file_name(path).to_string(),
        fqn: path.to_string(),
        path: path.to_string(),
        span,
        provenance: Provenance::Extracted,
        confidence: Confidence::new(1.0),
    }
}

/// Build the `DocSection` node for one parsed section (design table: `name` =
/// heading text, `fqn` = `<path>#<anchor>`, `path` = the CONTAINING file (not
/// the fqn — matching how a code `Function`'s `path` is its file, not its
/// fqn), `span` = heading through end of body, exactly as `parse_markdown`
/// computed it).
fn doc_section_node(uid: Uid, path: &str, section: &DocSectionModel) -> Node {
    Node {
        uid,
        kind: NodeKind::DocSection,
        name: section.heading.clone(),
        fqn: format!("{path}#{}", section.anchor),
        path: path.to_string(),
        span: section.span,
        provenance: Provenance::Extracted,
        confidence: Confidence::new(1.0),
    }
}

/// Build the `DocSection` node for one doc comment (K3; plan §"Task K3"
/// Interfaces): `name` = `"doc: {symbol_name}"`, `fqn` =
/// `"{source_path}#doc:{symbol_fqn}"`, `path` = the source file the symbol is
/// declared in, `span` = `RawSymbol::doc_span` (the comment's own span,
/// captured by the language analyzer — never its text, bodies-from-disk). This
/// is the synthetic-name analogue of [`doc_section_node`]'s markdown-heading
/// `name`: a doc COMMENT has no heading to borrow, and reading its first line
/// for a title would mean storing body text, which the design forbids — so the
/// name is built from data already on hand (the symbol's own name) rather than
/// read from the comment.
fn doc_comment_section_node(
    uid: Uid,
    path: &str,
    symbol_fqn: &str,
    symbol_name: &str,
    span: strata_core::Span,
) -> Node {
    Node {
        uid,
        kind: NodeKind::DocSection,
        name: format!("doc: {symbol_name}"),
        fqn: format!("{path}#doc:{symbol_fqn}"),
        path: path.to_string(),
        span,
        provenance: Provenance::Extracted,
        confidence: Confidence::new(1.0),
    }
}

/// Node kinds whose `.path` uniquely (or near-uniquely) identifies a FILE, so a
/// [`DocRefKind::PathRef`] resolves against exactly the file-level entity —
/// never every symbol defined in that file (which shares the same `.path`).
/// Code modules, data-plane schema tables, contract operations, and `Doc`
/// nodes themselves (so one doc can `PathRef` another — resolution #3).
fn is_path_bearing(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Module
            | NodeKind::Table
            | NodeKind::ApiOperation
            | NodeKind::GraphqlField
            | NodeKind::Doc
    )
}

/// The lookup tables [`build_knowledge_plane`] resolves refs against, built in
/// one pass over `g.nodes()` (§3 Step 3 item 2). `by_path` is restricted to
/// [`is_path_bearing`] kinds; `by_fqn`/`by_name` cover every node (including
/// the `Doc`/`DocSection` nodes just added, so cross-doc mentions by fqn/name
/// are possible too, though the fixture only exercises code symbols).
/// `by_path_fqn` (K3) is the precise doc-comment lookup: keyed on the EXACT
/// `(path, fqn)` pair a `RawSymbol` carries, so a doc-comment `Documents` edge
/// targets the one real graph node that symbol produced — never a name/fqn
/// collision from an unrelated file the way `by_fqn` alone could (a syntactic
/// doc-comment adjacency is a fact about ONE specific declaration, not a
/// markdown author's ambiguous prose reference).
///
/// Keys are OWNED `String`s, not `&str` borrows into `g`: phase 3 needs `&mut
/// Graph` (to add `Mentions` edges) at the same time it consults these
/// tables, and a borrowed key would keep `g` immutably borrowed for the
/// tables' whole lifetime — incompatible with the concurrent mutation. The
/// clone cost is one-time, at plane-build time, not per-ref.
struct LookupTables {
    by_fqn: HashMap<String, Vec<Uid>>,
    by_name: HashMap<String, Vec<Uid>>,
    by_path: HashMap<String, Vec<Uid>>,
    by_path_fqn: HashMap<(String, String), Vec<Uid>>,
}

fn build_lookup_tables(g: &Graph) -> LookupTables {
    let mut by_fqn: HashMap<String, Vec<Uid>> = HashMap::new();
    let mut by_name: HashMap<String, Vec<Uid>> = HashMap::new();
    let mut by_path: HashMap<String, Vec<Uid>> = HashMap::new();
    let mut by_path_fqn: HashMap<(String, String), Vec<Uid>> = HashMap::new();
    for node in g.nodes() {
        by_fqn
            .entry(node.fqn.clone())
            .or_default()
            .push(node.uid.clone());
        by_name
            .entry(node.name.clone())
            .or_default()
            .push(node.uid.clone());
        if is_path_bearing(node.kind) {
            by_path
                .entry(node.path.clone())
                .or_default()
                .push(node.uid.clone());
        }
        by_path_fqn
            .entry((node.path.clone(), node.fqn.clone()))
            .or_default()
            .push(node.uid.clone());
    }
    LookupTables {
        by_fqn,
        by_name,
        by_path,
        by_path_fqn,
    }
}

/// Resolve one ref against the tables, in priority order: a [`DocRefKind::PathRef`]
/// tries ONLY `by_path` (an exact path reference is either a hit or nothing — a
/// path string is never usefully retried as a bare name); a
/// [`DocRefKind::InlineCode`] tries `by_fqn` first, then (only on a `by_fqn`
/// MISS — zero candidates, not merely an ambiguous 2+) `by_name`; a
/// [`DocRefKind::FenceToken`] tries `by_fqn` ONLY — **no name-tier fallback**
/// (review finding F1). A fence token is incidental code-example vocabulary
/// (`r.process(x)`'s `process` segment), not a deliberate symbol callout the
/// way an author's inline `` `code` `` span is; falling a bare fence-token
/// name through to `by_name` would spuriously match unrelated symbols across
/// the whole repo far more often than it would catch a real reference. A tier
/// "hits" the moment it has ≥1 candidate: 2+ candidates still resolves AT that
/// tier (ambiguous fan-out), it does not fall through. Returns `None` when no
/// tried tier produced a candidate (the caller decides whether that counts as
/// stale — see `resolve_ref`'s fence-token carve-out, F1).
///
/// The name-tier fallback gate is a POSITIVE match on
/// [`DocRefKind::InlineCode`] (N1, review finding from K2) rather than a
/// `!= FenceToken` exclusion: a hypothetical future `DocRefKind` variant then
/// defaults to the conservative "no name-tier fallback" behavior automatically,
/// instead of silently inheriting it by accident of not being `FenceToken`.
/// Behavior for the two variants that exist today is unchanged — `PathRef`
/// never reaches this point (it returns above), so the only two kinds live
/// here are `InlineCode` and `FenceToken`.
fn candidates_for<'t>(
    r: &DocRef,
    tables: &'t LookupTables,
) -> Option<(&'t [Uid], f32, Provenance)> {
    if r.kind == DocRefKind::PathRef {
        return tables
            .by_path
            .get(r.text.as_str())
            .map(|v| (v.as_slice(), KNOW_MENTION_PATH, Provenance::Extracted));
    }
    let fqn_hit = tables
        .by_fqn
        .get(r.text.as_str())
        .map(|v| (v.as_slice(), KNOW_MENTION_FQN, Provenance::Inferred));
    if fqn_hit.is_some() {
        return fqn_hit;
    }
    if r.kind != DocRefKind::InlineCode {
        return None;
    }
    tables
        .by_name
        .get(r.text.as_str())
        .map(|v| (v.as_slice(), KNOW_MENTION_NAME, Provenance::Inferred))
}

/// K7 fix F2 (reviewer-recommended, controller-adopted): whether an
/// unresolvable [`DocRefKind::InlineCode`] ref's raw text plausibly NAMES A
/// SYMBOL, splitting the miss between `stale_doc_mentions` (a real, broken
/// reference) and `unresolved_plain_refs` (schema-invisible: a constant,
/// config key, or plain word the graph was never going to model).
///
/// `true` when the text:
/// - contains `::` or `.` (an explicit qualification, e.g. `mod::item`,
///   `a.b` — unambiguously symbol-shaped regardless of case), OR
/// - is compound-case: it has BOTH an ASCII lowercase and an ASCII uppercase
///   letter (`renamedSymbol`, `DocSection` — camelCase/PascalCase-shaped)
///   AND is therefore not all-caps-with-underscores. The second clause is
///   written out explicitly (rather than relying on it following logically
///   from "has both cases") because a `SCREAMING_SNAKE_CASE` token by
///   definition has NO lowercase letter — the two checks read as the same
///   test the design calls for, so a reader can trace this code straight back
///   to the spec sentence without re-deriving the entailment.
///
/// `false` for a `SCREAMING_SNAKE_CASE` token (`CONF_BARE_MULTI`) or a bare
/// all-lowercase single word (`foo`) — never guessed into `stale_doc_mentions`
/// (never-confidently-wrong governs the metric itself, not only graph edges).
fn inline_code_looks_symbol_shaped(text: &str) -> bool {
    if text.contains("::") || text.contains('.') {
        return true;
    }
    let has_lower = text.bytes().any(|b| b.is_ascii_lowercase());
    let has_upper = text.bytes().any(|b| b.is_ascii_uppercase());
    let all_caps_with_underscores = has_upper && !has_lower;
    has_lower && has_upper && !all_caps_with_underscores
}

/// Resolve and edge ONE ref from `section_uid`, updating `cov` and `edged` (the
/// per-section dedup-by-dst set — edges are deduped by (src,dst), and `src` is
/// fixed to `section_uid` within one section's ref loop). `own_doc_uid` is the
/// uid of the `Doc` node this section itself belongs to (named to avoid
/// shadowing the free function [`doc_uid`], F5).
#[allow(clippy::too_many_arguments)]
fn resolve_ref(
    g: &mut Graph,
    cov: &mut KnowledgeLinkCoverage,
    section_uid: &Uid,
    own_doc_uid: &Uid,
    r: &DocRef,
    tables: &LookupTables,
    edged: &mut HashSet<Uid>,
) {
    let Some((raw_candidates, unique_conf, unique_prov)) = candidates_for(r, tables) else {
        // Fence-token misses are NOT drift (F1, controller decision): drift
        // (`stale_doc_mentions`) is an AUTHORIAL-CLAIM signal — the doc told
        // you a symbol exists and it doesn't. A fence token is incidental
        // code-example vocabulary the author never claimed as a reference (no
        // inline `` `code` `` intent, no link), so a miss there is silently
        // dropped, exactly like a fence-token match is silently NOT
        // name-tier-resolved (F1) — only `InlineCode`/`PathRef` misses are an
        // honest "this doc references something that doesn't exist".
        //
        // Positive match (N1, review finding from K2): staleness is counted
        // for the kinds KNOWN to be an authorial claim (`InlineCode`,
        // `PathRef`) rather than excluded for the one kind known NOT to be
        // (`FenceToken`) — so a hypothetical future `DocRefKind` variant
        // defaults to conservative (not counted as drift) until explicitly
        // added here, instead of silently inheriting drift-counting by
        // accident of not being `FenceToken`.
        //
        // K7 fix F2 (reviewer-recommended refinement, controller-adopted): a
        // PathRef miss is ALWAYS `stale_doc_mentions` — an exact path claim
        // that resolves nowhere is unambiguously an authorial claim about a
        // specific, nonexistent file, so it needs no further split. An
        // InlineCode miss is further split by SHAPE
        // (`inline_code_looks_symbol_shaped`): symbol-shaped text
        // (`strata_core::impact`, `renamedSymbol`) reads exactly like a
        // broken reference and stays `stale_doc_mentions`; a
        // `SCREAMING_SNAKE_CASE` token or bare all-lowercase word
        // (`CONF_BARE_MULTI`, `foo`) is schema-invisible — the graph never
        // models a raw constant/config-key, so "unresolved" here is not
        // evidence of drift, only of the graph's own reach — and is counted
        // separately as `unresolved_plain_refs`, NEVER folded into "stale".
        // See `KnowledgeLinkCoverage::stale_doc_mentions`'s doc comment,
        // `docs/accuracy/knowledge-linking.md`, and
        // `docs/src/concepts/knowledge.md`.
        match r.kind {
            DocRefKind::PathRef => cov.stale_doc_mentions += 1,
            DocRefKind::InlineCode if inline_code_looks_symbol_shaped(&r.text) => {
                cov.stale_doc_mentions += 1;
            }
            DocRefKind::InlineCode => cov.unresolved_plain_refs += 1,
            DocRefKind::FenceToken => {}
        }
        return;
    };

    // Ambiguity is graded on the RAW candidate count — BEFORE the self-skip
    // filter below (review finding F3). A reference matching several real
    // candidates is fundamentally ambiguous even when one of those candidates
    // happens to be the section's own Doc: removing that self-match doesn't
    // resolve which of the OTHER candidates was meant, so grading the lone
    // survivor as a clean, confident unique hit would be exactly the
    // confident-wrong "never confidently wrong" forbids. See the
    // `ambiguity_is_graded_on_raw_candidates_before_the_self_skip_filter` test
    // (two same-named docs, one self-referencing) for the pinned case.
    let ambiguous = raw_candidates.len() > 1;

    // Drop a self-reference (a section mentioning its OWN Doc) — skipped
    // silently: it DID resolve (not stale), but a doc trivially "mentioning"
    // itself is not a real cross-reference (not an edge either). If every raw
    // candidate was the section's own Doc, `targets` is empty and we return
    // without touching `cov` at all.
    let targets: Vec<&Uid> = raw_candidates
        .iter()
        .filter(|u| *u != own_doc_uid)
        .collect();
    if targets.is_empty() {
        return;
    }

    let (conf, prov) = if ambiguous {
        (KNOW_AMBIGUOUS, Provenance::Ambiguous)
    } else {
        (unique_conf, unique_prov)
    };

    cov.mentions_linked += 1;
    if ambiguous {
        cov.mentions_ambiguous += 1;
    }

    for dst in targets {
        // Dedupe edges by (src,dst): src is fixed (section_uid) within this
        // loop, so a dst already edged by an earlier ref in this section is
        // skipped — the ref still counted as linked above, just no duplicate
        // edge.
        if edged.insert(dst.clone()) {
            g.add_edge(Edge {
                src: section_uid.clone(),
                dst: dst.clone(),
                kind: EdgeKind::Mentions,
                provenance: prov,
                confidence: Confidence::new(conf),
            });
        }
    }
}

/// Build the knowledge plane on `g`: `Doc`/`DocSection` nodes, `Contains`
/// structural edges, banded `Mentions` edges, AND (K3) doc-comment
/// `DocSection` nodes + Extracted `Documents` edges for every symbol in
/// `analyzed` whose `RawSymbol::doc_span` is set. `docs` is K1's
/// `parse_markdown` output — `(repo-relative path, parsed DocModel)` pairs;
/// `analyzed` is the SAME combined (ts+py+cs+rust) analyzed-file map every
/// other plane builder consumes. Returns the coverage/drift tally.
///
/// Four phases, in order:
/// 1. Create every `Doc`/`DocSection` node and `Contains` edge for EVERY doc
///    first — so the lookup tables built next see the full node set
///    regardless of `docs`' order (a doc processed first can still be the
///    target of a later doc's `PathRef`, and vice versa).
/// 2. Build the `by_fqn`/`by_name`/`by_path`/`by_path_fqn` lookup tables in
///    one pass over `g.nodes()` (now including the nodes phase 1 just added).
/// 3. (K3) For every symbol in `analyzed` with a `doc_span`, emit its
///    doc-comment `DocSection` node + `Documents` edge to the one graph node
///    `by_path_fqn` resolves it to (never a fan-out — a doc-comment's target
///    is a syntactic fact about ONE declaration, not a markdown guess; an
///    unmatched or ambiguous `(path, fqn)` is skipped rather than an edge
///    invented, though this should not arise in practice since `analyzed` is
///    the same input the code planes themselves were built from).
/// 4. Resolve every section's refs against the phase-2 tables.
pub fn build_knowledge_plane(
    g: &mut Graph,
    repo: &str,
    analyzed: &BTreeMap<String, AnalyzedFile>,
    docs: &[(String, DocModel)],
) -> KnowledgeLinkCoverage {
    let mut cov = KnowledgeLinkCoverage {
        docs: docs.len(),
        sections: docs.iter().map(|(_, d)| d.sections.len()).sum(),
        ..KnowledgeLinkCoverage::default()
    };

    // ── Phase 1: nodes + Contains edges. ──
    for (path, doc) in docs {
        let d_uid = doc_uid(repo, path);
        g.add_node(doc_node(d_uid.clone(), path, doc));
        for section in &doc.sections {
            let s_uid = doc_section_uid(repo, path, &section.anchor);
            g.add_node(doc_section_node(s_uid.clone(), path, section));
            g.add_edge(Edge {
                src: d_uid.clone(),
                dst: s_uid,
                kind: EdgeKind::Contains,
                provenance: Provenance::Extracted,
                confidence: Confidence::new(CONTAINS_CONF),
            });
        }
    }

    // ── Phase 2: lookup tables over the full node set. ──
    let tables = build_lookup_tables(g);

    // ── Phase 3 (K3): doc-comment `Documents` edges. ──
    for (path, file) in analyzed {
        for symbol in &file.symbols {
            let Some(span) = symbol.doc_span else {
                continue;
            };
            let Some(candidates) = tables.by_path_fqn.get(&(path.clone(), symbol.fqn.clone()))
            else {
                continue;
            };
            // Exactly one candidate: a doc comment documents ONE declaration,
            // never several — a miss (0) or a same-(path,fqn) collision (2+,
            // not expected from any analyzer today) is skipped rather than
            // guessed (never confidently wrong).
            let [target] = candidates.as_slice() else {
                continue;
            };
            let sec_uid = doc_section_uid(repo, path, &format!("doc:{}", symbol.fqn));
            g.add_node(doc_comment_section_node(
                sec_uid.clone(),
                path,
                &symbol.fqn,
                &symbol.name,
                span,
            ));
            g.add_edge(Edge {
                src: sec_uid,
                dst: target.clone(),
                kind: EdgeKind::Documents,
                provenance: Provenance::Extracted,
                confidence: Confidence::new(KNOW_DOC_COMMENT),
            });
            cov.doc_comments += 1;
        }
    }

    // ── Phase 4: resolve every section's refs. ──
    for (path, doc) in docs {
        let d_uid = doc_uid(repo, path);
        for section in &doc.sections {
            let s_uid = doc_section_uid(repo, path, &section.anchor);
            let mut edged: HashSet<Uid> = HashSet::new();
            for r in &section.refs {
                resolve_ref(g, &mut cov, &s_uid, &d_uid, r, &tables, &mut edged);
            }
        }
    }

    cov
}

/// Test/tool-visible convenience: build the CODE graph (via
/// [`crate::build::assemble_graph`]) then the knowledge plane on top, in one
/// call — mirrors [`crate::contract::assemble_graph_with_contracts`] /
/// [`crate::data::assemble_graph_with_data`]. `index_impl` wires the same two
/// calls into the full multi-plane pipeline instead of using this directly
/// (it already has a graph assembled with every OTHER plane by the time
/// markdown is collected).
pub fn assemble_graph_with_knowledge(
    analyzed: &std::collections::BTreeMap<String, AnalyzedFile>,
    repo_name: &str,
    opts: &strata_lang_ts::ResolveOptions,
    docs: &[(String, DocModel)],
) -> (Graph, KnowledgeLinkCoverage) {
    let mut g = crate::build::assemble_graph(analyzed, repo_name, opts);
    let cov = build_knowledge_plane(&mut g, repo_name, analyzed, docs);
    (g, cov)
}

#[cfg(test)]
mod tests {
    use super::*;
    use strata_knowledge::parse_markdown;

    const REPO: &str = "kt";

    #[test]
    fn doc_section_uid_matches_the_pinned_shape() {
        // The plan's UID-stability rule: `doc|<repo>|<path>|<path>#<anchor>|`.
        let uid = doc_section_uid(REPO, "docs/guide.md", "using-alphaone");
        assert_eq!(
            uid.as_str(),
            "doc|kt|docs/guide.md|docs/guide.md#using-alphaone|"
        );
    }

    #[test]
    fn doc_uid_fqn_is_the_path_itself() {
        let uid = doc_uid(REPO, "docs/guide.md");
        assert_eq!(uid.as_str(), "doc|kt|docs/guide.md|docs/guide.md|");
    }

    #[test]
    fn contains_edge_is_extracted_one_and_never_impact_traversed() {
        let mut g = Graph::new();
        let doc = parse_markdown("docs/guide.md", "# Heading\nbody\n");
        let cov = build_knowledge_plane(
            &mut g,
            REPO,
            &BTreeMap::new(),
            &[("docs/guide.md".to_string(), doc)],
        );
        assert_eq!(cov.docs, 1);
        assert_eq!(cov.sections, 1);

        let d_uid = doc_uid(REPO, "docs/guide.md");
        let s_uid = doc_section_uid(REPO, "docs/guide.md", "heading");
        let contains: Vec<_> = g
            .edges()
            .filter(|e| e.src == d_uid && e.dst == s_uid)
            .collect();
        assert_eq!(
            contains.len(),
            1,
            "exactly one Doc-Contains-DocSection edge"
        );
        assert_eq!(contains[0].kind, EdgeKind::Contains);
        assert_eq!(contains[0].provenance, Provenance::Extracted);
        assert!((contains[0].confidence.value() - 1.0).abs() < 1e-6);

        // Contains must never be impact-traversed (strata_core::traverse owns the
        // authoritative guard/test; this just confirms the edge this module emits
        // is the SAME kind that guard covers).
        let r = strata_core::impact(&g, &s_uid, &strata_core::ImpactOptions::default());
        assert!(
            !r.affected.iter().any(|a| a.uid == d_uid),
            "Contains must never be impact-traversed: {r:?}"
        );
    }

    #[test]
    fn a_doc_that_pathrefs_its_own_file_is_skipped_silently() {
        // A relative link back to the doc's own file (a common "edit this page"/
        // self-referential pattern) must be skipped: not stale (it DID resolve),
        // not an edge (a doc trivially "mentions" itself).
        let doc = parse_markdown(
            "docs/self.md",
            "# Heading\nSee [this file](docs/self.md) for more.\n",
        );
        let mut g = Graph::new();
        let cov = build_knowledge_plane(
            &mut g,
            REPO,
            &BTreeMap::new(),
            &[("docs/self.md".to_string(), doc)],
        );

        assert_eq!(cov.mentions_linked, 0, "self-mention is not linked");
        assert_eq!(cov.mentions_ambiguous, 0);
        assert_eq!(
            cov.stale_doc_mentions, 0,
            "self-mention is not stale either"
        );

        let s_uid = doc_section_uid(REPO, "docs/self.md", "heading");
        assert_eq!(
            g.edges().filter(|e| e.src == s_uid).count(),
            0,
            "no Mentions edge for a pure self-reference"
        );
    }

    #[test]
    fn fence_token_miss_is_not_edged_and_not_stale_but_inline_code_miss_is_stale() {
        // Review finding F1. A fenced `r.process(x);` yields FenceToken refs
        // "r.process" and "process" (K1's own extraction: the whole qualified
        // token, then its qualifying segment — "x" is too short to qualify at
        // all). Neither matches any real symbol in this graph (which has no
        // code nodes, only the doc itself). Per F1: a fence-token miss is NOT
        // a name-tier candidate (no by_name fallback) AND is NOT drift — a
        // fence token is incidental code-example vocabulary, not an
        // authorial claim the way an inline `` `code` `` span is. The
        // trailing inline-code `vanishedSymbol`, by contrast, DOES miss both
        // tiers and IS an authorial claim, so it must still count as stale —
        // asserted here for contrast, unchanged from before F1.
        let doc = parse_markdown(
            "docs/g.md",
            "# H\n```rust\nr.process(x);\n```\n`vanishedSymbol` is gone.\n",
        );
        let mut g = Graph::new();
        let cov = build_knowledge_plane(
            &mut g,
            REPO,
            &BTreeMap::new(),
            &[("docs/g.md".to_string(), doc)],
        );

        assert_eq!(
            cov.mentions_linked, 0,
            "neither fence token nor the inline-code miss produced an edge"
        );
        assert_eq!(cov.mentions_ambiguous, 0);
        assert_eq!(
            cov.stale_doc_mentions, 1,
            "only the InlineCode miss (vanishedSymbol) counts as stale; the \
             two fence-token misses (r.process, process) do not"
        );
        // K7 fix F2: "vanishedSymbol" is compound-case (has both an ASCII
        // lower and upper letter) — symbol-shaped, so it stays stale and
        // never lands in the new plain-unresolved counter.
        assert_eq!(
            cov.unresolved_plain_refs, 0,
            "vanishedSymbol is compound-case (symbol-shaped), not plain"
        );

        let s_uid = doc_section_uid(REPO, "docs/g.md", "h");
        assert_eq!(
            g.edges().filter(|e| e.src == s_uid).count(),
            0,
            "no Mentions edge from either fence token or the stale inline code"
        );
    }

    #[test]
    fn inline_code_looks_symbol_shaped_matches_the_design_shape_rule() {
        // K7 fix F2, pinned directly against the pure predicate: `::`/`.`
        // qualification always counts; otherwise compound-case (both an ASCII
        // lower AND an ASCII upper letter present) counts; a
        // SCREAMING_SNAKE_CASE token or a bare all-lowercase word does not.
        assert!(inline_code_looks_symbol_shaped("strata_core::impact"));
        assert!(inline_code_looks_symbol_shaped("a.b"));
        assert!(inline_code_looks_symbol_shaped("renamedSymbol"));
        assert!(inline_code_looks_symbol_shaped("DocSection"));
        assert!(
            !inline_code_looks_symbol_shaped("CONF_BARE_MULTI"),
            "SCREAMING_SNAKE_CASE has no lowercase letter at all"
        );
        assert!(
            !inline_code_looks_symbol_shaped("FOO"),
            "all-caps single word — no lowercase either"
        );
        assert!(
            !inline_code_looks_symbol_shaped("foo"),
            "bare all-lowercase word"
        );
        assert!(
            !inline_code_looks_symbol_shaped("foo_bar"),
            "snake_case, still all-lowercase"
        );
    }

    #[test]
    fn stale_vs_plain_unresolved_split_by_ref_kind_and_shape() {
        // K7 fix F2 (reviewer-recommended refinement, controller-adopted) —
        // the exact scenarios the fix wave specified, in one section so the
        // split is visible at a glance. None of these refs matches anything
        // in this graph (no code was analyzed here), so every one is a
        // genuine miss and none produces a Mentions edge:
        //   - `src/gone.rs` (PathRef miss): ALWAYS stale, regardless of shape
        //     — an exact path claim to a file that does not exist.
        //   - `CONF_BARE_MULTI` (InlineCode miss, SCREAMING_SNAKE_CASE):
        //     plain unresolved, NEVER stale — schema-invisible constant shape.
        //   - `foo` (InlineCode miss, bare all-lowercase): plain unresolved.
        //   - `renamedSymbol` (InlineCode miss, camelCase/compound-case):
        //     stale — reads as a real, broken symbol reference.
        //   - `strata_core::impact` (InlineCode miss, `::`-qualified): stale
        //     — explicit qualification is unambiguously symbol-shaped.
        let doc = parse_markdown(
            "docs/g.md",
            "# H\nSee [gone](src/gone.rs) for details.\n`CONF_BARE_MULTI` was \
             removed. `renamedSymbol` was renamed. `foo` is undocumented. \
             `strata_core::impact` moved.\n",
        );
        let mut g = Graph::new();
        let cov = build_knowledge_plane(
            &mut g,
            REPO,
            &BTreeMap::new(),
            &[("docs/g.md".to_string(), doc)],
        );

        assert_eq!(cov.mentions_linked, 0, "every ref here is an honest miss");
        assert_eq!(
            cov.stale_doc_mentions, 3,
            "three symbol-shaped misses: src/gone.rs (PathRef, always stale), \
             renamedSymbol (camelCase), strata_core::impact (`::`-qualified)"
        );
        assert_eq!(
            cov.unresolved_plain_refs, 2,
            "two plain misses: CONF_BARE_MULTI (SCREAMING_SNAKE_CASE) and foo \
             (bare all-lowercase) — schema-invisible, never folded into stale"
        );

        let s_uid = doc_section_uid(REPO, "docs/g.md", "h");
        assert_eq!(
            g.edges().filter(|e| e.src == s_uid).count(),
            0,
            "no Mentions edge from any of these misses, stale or plain"
        );
    }

    #[test]
    fn ambiguity_is_graded_on_raw_candidates_before_the_self_skip_filter() {
        // Review finding F3. Two docs both literally named "README.md"
        // (different paths) — by_name["README.md"] fans out to BOTH. Doc A's
        // own section mentions `README.md` inline: the self-skip filter
        // removes doc A's OWN uid from the candidate set, leaving exactly one
        // survivor (doc B) — but the reference was ambiguous BEFORE that
        // filter ran (2 real docs share the name), so the edge to doc B must
        // still read Ambiguous 0.35, never a confident NAME-tier 0.70 just
        // because self-filtering happened to leave one candidate standing.
        let doc_a = parse_markdown("packages/a/README.md", "# H\nSee `README.md` too.\n");
        let doc_b = parse_markdown("packages/b/README.md", "# H\nNothing here.\n");
        let mut g = Graph::new();
        let cov = build_knowledge_plane(
            &mut g,
            REPO,
            &BTreeMap::new(),
            &[
                ("packages/a/README.md".to_string(), doc_a),
                ("packages/b/README.md".to_string(), doc_b),
            ],
        );

        let sec_a = doc_section_uid(REPO, "packages/a/README.md", "h");
        let doc_b_uid = doc_uid(REPO, "packages/b/README.md");

        let edges: Vec<_> = g
            .edges()
            .filter(|e| e.kind == EdgeKind::Mentions && e.src == sec_a && e.dst == doc_b_uid)
            .collect();
        assert_eq!(
            edges.len(),
            1,
            "one Mentions edge from A's section to doc B: {edges:?}"
        );
        assert_eq!(
            edges[0].provenance,
            Provenance::Ambiguous,
            "must be Ambiguous even though self-filtering left exactly one \
             survivor — the RAW candidate count (2) is what grades this"
        );
        assert!(
            (edges[0].confidence.value() - KNOW_AMBIGUOUS).abs() < 1e-6,
            "must be the shared 0.35 Ambiguous tier, not the NAME tier's 0.70: {:?}",
            edges[0].confidence.value()
        );
        assert_eq!(cov.mentions_ambiguous, 1);
        assert_eq!(cov.mentions_linked, 1);
        assert_eq!(cov.stale_doc_mentions, 0);
    }

    #[test]
    fn know_confidence_constants_are_within_their_bands() {
        // §4.1: Extracted 0.95..=1.0, Inferred 0.40..=0.80, Ambiguous < 0.40.
        const {
            assert!(KNOW_MENTION_PATH >= 0.95 && KNOW_MENTION_PATH <= 1.0);
        }
        const {
            assert!(KNOW_MENTION_FQN >= 0.40 && KNOW_MENTION_FQN <= 0.80);
        }
        const {
            assert!(KNOW_MENTION_NAME >= 0.40 && KNOW_MENTION_NAME <= 0.80);
        }
        const {
            assert!(KNOW_MENTION_FQN > KNOW_MENTION_NAME);
        }
        const {
            assert!(KNOW_AMBIGUOUS < 0.40);
        }
        const {
            assert!(KNOW_DOC_COMMENT >= 0.95 && KNOW_DOC_COMMENT <= 1.0);
        }
    }
}
