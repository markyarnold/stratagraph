//! K2: the knowledge-plane builder — Doc/DocSection nodes, banded Mentions
//! edges, drift counting, and docs entering the `impact` blast radius with an
//! honest "needs review" verdict (never "WILL BREAK": a stale doc doesn't
//! fail to compile, it goes stale — design §2 "Impact semantics").
//!
//! Fixture: `tests/fixtures/knowledge_repo/` — two TS files (`beta` declared
//! in both `src/app.ts` and `src/other.ts`, forcing a multi-candidate name;
//! `src/app.ts` also declares `class Foo { bar() {} }`, whose fqn `Foo.bar`
//! differs from its name `bar` — the fixture's ONLY name-tier case) and one
//! markdown doc (`docs/guide.md`) whose three sections exercise every
//! resolution tier:
//!   - "Using alphaOne": a `PathRef` to `src/app.ts` (Extracted 0.95, unique)
//!     and a unique bare name `alphaOne` that resolves at the fqn tier
//!     (Inferred 0.80).
//!   - "Betas": an ambiguous bare name `beta` (two candidates, Ambiguous 0.35
//!     fan-out) and an unresolvable name `vanishedSymbol` (stale, never
//!     edged).
//!   - "Bar method": a unique bare name `bar` that MISSES the fqn tier
//!     (`Foo.bar` != `bar`) and resolves at the name tier (Inferred 0.70) —
//!     pins the tier the other two sections never exercise (review finding
//!     F2; without this, a 0.70→other-value nudge to `KNOW_MENTION_NAME`
//!     would pass the whole suite undetected).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use strata_core::{AffectedNode, AnalyzedFile, Direction, EdgeKind, Graph, Provenance, Uid};
use strata_index::{assemble_graph_with_knowledge, doc_section_uid, KnowledgeLinkCoverage};
use strata_knowledge::{parse_markdown, DocModel};
use strata_lang_ts::{analyze, ResolveOptions};

const REPO: &str = "knowledge-repo";

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("knowledge_repo")
}

fn read_fixture(rel: &str) -> String {
    std::fs::read_to_string(fixture_dir().join(rel))
        .unwrap_or_else(|e| panic!("read fixture knowledge_repo/{rel}: {e}"))
}

/// Analyze the fixture's two TS files (mirrors `within_repo_collision.rs`'s
/// `analyzed()` helper).
fn analyzed() -> BTreeMap<String, AnalyzedFile> {
    let mut m = BTreeMap::new();
    for rel in ["src/app.ts", "src/other.ts"] {
        let src = read_fixture(rel);
        m.insert(rel.to_string(), analyze(rel, &src));
    }
    m
}

/// Parse the fixture's one markdown doc (K1's `parse_markdown`) into the
/// `(path, DocModel)` pairs `build_knowledge_plane` consumes.
fn docs() -> Vec<(String, DocModel)> {
    let path = "docs/guide.md".to_string();
    let content = read_fixture(&path);
    let model = parse_markdown(&path, &content);
    vec![(path, model)]
}

/// Analyze + parse + assemble in one call: the code plane (TS) plus the
/// knowledge plane on top, over the fixture.
fn build_fixture() -> (Graph, KnowledgeLinkCoverage) {
    assemble_graph_with_knowledge(&analyzed(), REPO, &ResolveOptions::default(), &docs())
}

/// Like [`build_fixture`], but `src/app.ts` is re-analyzed with a JSDoc block
/// comment immediately above `alphaOne` — the K3 doc-comment fixture. The
/// markdown doc fixture is unchanged (Mentions and Documents are independent
/// signals over the same graph), so this exercises the doc-comment
/// `Documents` path non-vacuously without disturbing any Mentions assertion
/// elsewhere in this file.
fn build_fixture_with_doc_comments() -> (Graph, KnowledgeLinkCoverage) {
    let mut m = analyzed();
    let src = read_fixture("src/app.ts");
    let documented = format!("/**\n * Adds one, documented.\n */\n{src}");
    m.insert("src/app.ts".to_string(), analyze("src/app.ts", &documented));
    assemble_graph_with_knowledge(&m, REPO, &ResolveOptions::default(), &docs())
}

/// The `ts` UID for a whole-file Module node (mirrors `crate::build::uid_module`'s
/// shape: `Uid::new("ts", repo, path, "<module>", "")`).
fn module_uid(path: &str) -> Uid {
    Uid::new("ts", REPO, path, "<module>", "")
}

/// The `ts` UID for a top-level function/symbol node (mirrors
/// `within_repo_collision.rs`'s `fn_uid` helper).
fn fn_uid(path: &str, fqn: &str) -> Uid {
    Uid::new("ts", REPO, path, fqn, "")
}

/// Outgoing `Mentions` edges from `src`: `(dst, provenance, confidence)`.
fn mentions_of(g: &Graph, src: &Uid) -> Vec<(Uid, Provenance, f32)> {
    g.neighbors(src, Direction::Outgoing, &[EdgeKind::Mentions])
        .into_iter()
        .map(|(e, _)| (e.dst.clone(), e.provenance, e.confidence.value()))
        .collect()
}

/// Outgoing `Documents` edges from `src`: `(dst, provenance, confidence)`
/// (K3, mirrors [`mentions_of`]).
fn documents_of(g: &Graph, src: &Uid) -> Vec<(Uid, Provenance, f32)> {
    g.neighbors(src, Direction::Outgoing, &[EdgeKind::Documents])
        .into_iter()
        .map(|(e, _)| (e.dst.clone(), e.provenance, e.confidence.value()))
        .collect()
}

/// Resolve `name` to the graph node whose `name` field EXACTLY matches (never
/// a substring pick — `query` is lexical/substring, and a heading like "Using
/// alphaOne" also substring-matches "alphaOne", so an exact filter is the only
/// honest way to pick the real symbol out of the candidates).
fn node_named<'g>(g: &'g Graph, name: &str) -> &'g strata_core::Node {
    strata_core::query(g, name)
        .into_iter()
        .find(|n| n.name == name)
        .map(|n| {
            // `query` returns owned clones; re-fetch the graph's own reference
            // by uid so callers get a borrow tied to `g`'s lifetime.
            g.get_node(&n.uid)
                .unwrap_or_else(|| panic!("node {name:?} vanished between query and get_node"))
        })
        .unwrap_or_else(|| panic!("fixture must define a node named {name:?}"))
}

/// `strata_core::impact`'s reverse blast radius for the node named `name`,
/// under default options (contract/infra hops on, no depth/confidence limits
/// beyond the defaults) — the same walk `strata impact` runs.
fn impact_of(g: &Graph, name: &str) -> Vec<AffectedNode> {
    let node = node_named(g, name);
    strata_core::impact(g, &node.uid, &strata_core::ImpactOptions::default()).affected
}

#[test]
fn path_ref_links_extracted_unique_fqn_inferred() {
    let (g, cov) = build_fixture();
    assert_eq!(cov.docs, 1);
    assert_eq!(cov.sections, 3, "preamble is blank/absent; three headings");

    let sec = doc_section_uid(REPO, "docs/guide.md", "using-alphaone");
    let out = mentions_of(&g, &sec);
    assert!(
        out.iter().any(|(d, p, c)| d == &module_uid("src/app.ts")
            && *p == Provenance::Extracted
            && (*c - 0.95).abs() < 1e-6),
        "path ref 0.95: {out:?}"
    );
    assert!(
        out.iter()
            .any(|(d, p, c)| d == &fn_uid("src/app.ts", "alphaOne")
                && *p == Provenance::Inferred
                && (*c - 0.80).abs() < 1e-6),
        "unique name→fqn 0.80: {out:?}"
    );
}

#[test]
fn multi_candidate_name_fans_out_ambiguous_and_stale_is_counted_not_edged() {
    let (g, cov) = build_fixture();
    let sec = doc_section_uid(REPO, "docs/guide.md", "betas");
    let beta_edges: Vec<_> = mentions_of(&g, &sec)
        .into_iter()
        .filter(|(d, _, _)| d.to_string().contains("|beta|"))
        .collect();
    assert_eq!(beta_edges.len(), 2, "one Ambiguous edge per candidate");
    assert!(beta_edges
        .iter()
        .all(|(_, p, c)| *p == Provenance::Ambiguous && (*c - 0.35).abs() < 1e-6));
    assert_eq!(
        cov.stale_doc_mentions, 1,
        "vanishedSymbol counted, never edged"
    );
    assert!(!g
        .edges()
        .any(|e| e.src == sec && e.dst.to_string().contains("vanishedSymbol")));

    // Coverage semantics (design's own worked example: "N mentions linked (M
    // ambiguous)" — ambiguous is a SUBSET of linked, not disjoint from it).
    // Refs: alphaOne (linked, fqn), src/app.ts (linked, path), beta (linked +
    // ambiguous), vanishedSymbol (stale), bar (linked, name — see the
    // "Bar method" section test below). => linked=4, ambiguous=1, stale=1.
    assert_eq!(cov.mentions_linked, 4);
    assert_eq!(cov.mentions_ambiguous, 1);
    assert_eq!(cov.stale_doc_mentions, 1);
}

#[test]
fn unique_bare_name_with_a_qualified_fqn_resolves_at_the_name_tier_inferred_0_70() {
    // Review finding F2: `bar` is a METHOD (fqn "Foo.bar", name "bar"), so the
    // fqn tier MISSES (no symbol's fqn is bare "bar") and this pins the NAME
    // tier specifically — Inferred 0.70, never the fqn tier's 0.80. Without
    // this, nothing in the suite reached the name tier with a value-pinning
    // assertion (see `confidence_bands.rs`'s discrimination note).
    let (g, _) = build_fixture();
    let sec = doc_section_uid(REPO, "docs/guide.md", "bar-method");
    let out = mentions_of(&g, &sec);
    assert!(
        out.iter()
            .any(|(d, p, c)| d == &fn_uid("src/app.ts", "Foo.bar")
                && *p == Provenance::Inferred
                && (*c - 0.70).abs() < 1e-6),
        "unique bare name -> name tier 0.70: {out:?}"
    );
}

#[test]
fn impact_reaches_the_mentioning_doc_section() {
    let (g, _) = build_fixture();
    let affected = impact_of(&g, "alphaOne");
    assert!(
        affected
            .iter()
            .any(|a| a.uid == doc_section_uid(REPO, "docs/guide.md", "using-alphaone")),
        "the section mentioning alphaOne is in its blast radius: {affected:?}"
    );
    // The CLI's own rendering (never "WILL BREAK" for a Doc/DocSection kind)
    // is unit-tested directly in strata-cli against a small hermetic graph
    // (`render_impact_result_marks_a_doc_section_needs_review_never_will_break`)
    // rather than from here — this crate no longer takes a dev-dependency on
    // strata-cli (review verdict: remove); this test stays scoped to the
    // graph-level fact that `impact` reaches the mentioning DocSection.
}

#[test]
fn doc_comment_becomes_documents_edge_and_rides_impact() {
    let (g, cov) = build_fixture_with_doc_comments();
    let sec = doc_section_uid(REPO, "src/app.ts", "doc:alphaOne");
    let out = documents_of(&g, &sec);
    assert_eq!(
        out,
        vec![(
            fn_uid("src/app.ts", "alphaOne"),
            Provenance::Extracted,
            0.95
        )]
    );
    assert!(cov.doc_comments >= 1);
    assert!(
        impact_of(&g, "alphaOne").iter().any(|a| a.uid == sec),
        "change the symbol → its doc needs review"
    );
}

#[test]
fn context_docs_bucket_lists_both_documents_and_mentions_refs_only() {
    // K6: `context(alphaOne).docs` unions the doc-comment `Documents` edge
    // (K3) and the markdown `Mentions` edge (K2) over the SAME real,
    // builder-produced graph — not a hand-crafted fixture — so this pins the
    // bucket end-to-end through the actual knowledge-plane pipeline.
    let (g, _) = build_fixture_with_doc_comments();
    let node = node_named(&g, "alphaOne");
    let ctx = strata_core::context(&g, &node.uid).expect("context resolves alphaOne");

    let anchors: Vec<&str> = ctx.docs.iter().map(|d| d.anchor.as_str()).collect();
    assert!(
        anchors.contains(&"doc:alphaOne"),
        "the doc comment's Documents ref must be present: {anchors:?}"
    );
    assert!(
        anchors.contains(&"using-alphaone"),
        "the markdown section's Mentions ref must be present: {anchors:?}"
    );

    // Refs-only: each entry carries the EDGE's own provenance/confidence
    // (never a node-level value), matching what `documents_of`/`mentions_of`
    // read directly off the graph.
    let doc_comment_ref = ctx
        .docs
        .iter()
        .find(|d| d.anchor == "doc:alphaOne")
        .expect("doc-comment ref present");
    assert_eq!(doc_comment_ref.provenance, Provenance::Extracted);
    assert!((doc_comment_ref.confidence - 0.95).abs() < 1e-6);
    assert_eq!(doc_comment_ref.path, "src/app.ts");

    let mention_ref = ctx
        .docs
        .iter()
        .find(|d| d.anchor == "using-alphaone")
        .expect("mention ref present");
    assert_eq!(mention_ref.provenance, Provenance::Inferred);
    assert!((mention_ref.confidence - 0.80).abs() < 1e-6);
    assert_eq!(mention_ref.path, "docs/guide.md");

    // Sorted by uid ascending, mirroring every other context bucket.
    let mut sorted_anchors = ctx.docs.clone();
    sorted_anchors.sort_by(|a, b| a.uid.cmp(&b.uid));
    assert_eq!(ctx.docs, sorted_anchors, "docs bucket must be uid-sorted");
}

#[test]
fn doc_and_docsection_nodes_exist_with_the_designed_shape() {
    let (g, _) = build_fixture();

    let doc = g
        .get_node(&Uid::new("doc", REPO, "docs/guide.md", "docs/guide.md", ""))
        .expect("Doc node exists");
    assert_eq!(doc.kind, strata_core::NodeKind::Doc);
    assert_eq!(doc.name, "guide.md");
    assert_eq!(doc.fqn, "docs/guide.md");
    assert_eq!(doc.path, "docs/guide.md");

    let sec = g
        .get_node(&doc_section_uid(REPO, "docs/guide.md", "using-alphaone"))
        .expect("DocSection node exists");
    assert_eq!(sec.kind, strata_core::NodeKind::DocSection);
    assert_eq!(sec.name, "Using alphaOne");
    assert_eq!(sec.fqn, "docs/guide.md#using-alphaone");
    assert_eq!(sec.path, "docs/guide.md");

    // Doc —Contains→ DocSection, Extracted 1.0.
    let contains: Vec<_> = g
        .edges()
        .filter(|e| e.kind == EdgeKind::Contains && e.src == doc.uid && e.dst == sec.uid)
        .collect();
    assert_eq!(contains.len(), 1);
    assert_eq!(contains[0].provenance, Provenance::Extracted);
    assert!((contains[0].confidence.value() - 1.0).abs() < 1e-6);
}
