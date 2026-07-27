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

use std::collections::{HashMap, HashSet};

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

/// Coverage + drift counts [`build_knowledge_plane`] returns — the `knowledge:`
/// summary line's data, and (from K3 on) the vehicle for doc-comment counts.
///
/// `mentions_linked` and `mentions_ambiguous` are NOT disjoint:
/// `mentions_ambiguous` is the SUBSET of `mentions_linked` whose reference
/// resolved to 2+ candidates (fanned out) rather than one confident hit —
/// mirroring the design's own worked example, "480 mentions linked (12
/// ambiguous), 9 stale". `stale_doc_mentions` IS disjoint from both (a
/// reference that matched nothing at any tier).
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
    /// References that matched NOTHING at any tier — counted, never edged (the
    /// doc-drift signal: this reference is lying about what exists).
    pub stale_doc_mentions: usize,
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

/// The three lookup tables [`build_knowledge_plane`] resolves refs against,
/// built in one pass over `g.nodes()` (§3 Step 3 item 2). `by_path` is
/// restricted to [`is_path_bearing`] kinds; `by_fqn`/`by_name` cover every
/// node (including the `Doc`/`DocSection` nodes just added, so cross-doc
/// mentions by fqn/name are possible too, though the fixture only exercises
/// code symbols).
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
}

fn build_lookup_tables(g: &Graph) -> LookupTables {
    let mut by_fqn: HashMap<String, Vec<Uid>> = HashMap::new();
    let mut by_name: HashMap<String, Vec<Uid>> = HashMap::new();
    let mut by_path: HashMap<String, Vec<Uid>> = HashMap::new();
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
    }
    LookupTables {
        by_fqn,
        by_name,
        by_path,
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
    if fqn_hit.is_some() || r.kind == DocRefKind::FenceToken {
        return fqn_hit;
    }
    tables
        .by_name
        .get(r.text.as_str())
        .map(|v| (v.as_slice(), KNOW_MENTION_NAME, Provenance::Inferred))
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
        if r.kind != DocRefKind::FenceToken {
            cov.stale_doc_mentions += 1;
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
/// structural edges, and banded `Mentions` edges, for every doc in `docs`
/// (`(repo-relative path, parsed DocModel)` pairs — K1's `parse_markdown`
/// output). Returns the coverage/drift tally.
///
/// Three phases, in order:
/// 1. Create every `Doc`/`DocSection` node and `Contains` edge for EVERY doc
///    first — so the lookup tables built next see the full node set
///    regardless of `docs`' order (a doc processed first can still be the
///    target of a later doc's `PathRef`, and vice versa).
/// 2. Build the `by_fqn`/`by_name`/`by_path` lookup tables in one pass over
///    `g.nodes()` (now including the nodes phase 1 just added).
/// 3. Resolve every section's refs against those tables.
pub fn build_knowledge_plane(
    g: &mut Graph,
    repo: &str,
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

    // ── Phase 3: resolve every section's refs. ──
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
    let cov = build_knowledge_plane(&mut g, repo_name, docs);
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
        let cov = build_knowledge_plane(&mut g, REPO, &[("docs/guide.md".to_string(), doc)]);
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
        let cov = build_knowledge_plane(&mut g, REPO, &[("docs/self.md".to_string(), doc)]);

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
        let cov = build_knowledge_plane(&mut g, REPO, &[("docs/g.md".to_string(), doc)]);

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

        let s_uid = doc_section_uid(REPO, "docs/g.md", "h");
        assert_eq!(
            g.edges().filter(|e| e.src == s_uid).count(),
            0,
            "no Mentions edge from either fence token or the stale inline code"
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
    }
}
