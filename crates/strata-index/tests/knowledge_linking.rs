//! K2: the knowledge-plane builder — Doc/DocSection nodes, banded Mentions
//! edges, drift counting, and docs entering the `impact` blast radius with an
//! honest "needs review" verdict (never "WILL BREAK": a stale doc doesn't
//! fail to compile, it goes stale — design §2 "Impact semantics").
//!
//! Fixture: `tests/fixtures/knowledge_repo/` — two TS files (`beta` declared
//! in both `src/app.ts` and `src/other.ts`, forcing a multi-candidate name)
//! and one markdown doc (`docs/guide.md`) whose two sections exercise every
//! resolution tier:
//!   - "Using alphaOne": a `PathRef` to `src/app.ts` (Extracted 0.95, unique)
//!     and a unique bare name `alphaOne` that resolves at the fqn tier
//!     (Inferred 0.80).
//!   - "Betas": an ambiguous bare name `beta` (two candidates, Ambiguous 0.35
//!     fan-out) and an unresolvable name `vanishedSymbol` (stale, never
//!     edged).

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

/// Render `impact(name)` through the REAL CLI renderer
/// (`strata_cli::render_impact_result`) — no DB/disk IO, so this proves the
/// shipped renderer's "needs review" wording against an in-memory fixture
/// graph, not a re-implemented copy of its logic.
fn render_impact_for_test(g: &Graph, name: &str) -> String {
    let node = node_named(g, name).clone();
    let result = strata_core::impact(g, &node.uid, &strata_core::ImpactOptions::default());
    strata_cli::render_impact_result(g, &node, &result)
}

/// The line of a rendered impact report naming `needle` (e.g. a DocSection's
/// heading) — so an assertion about that ONE row cannot be satisfied by some
/// unrelated row elsewhere in the table.
fn line_containing<'s>(rendered: &'s str, needle: &str) -> &'s str {
    rendered
        .lines()
        .find(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("no rendered line contains {needle:?}:\n{rendered}"))
}

#[test]
fn path_ref_links_extracted_unique_fqn_inferred() {
    let (g, cov) = build_fixture();
    assert_eq!(cov.docs, 1);
    assert_eq!(cov.sections, 2, "preamble is blank/absent; two headings");

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
    // ambiguous), vanishedSymbol (stale). => linked=3, ambiguous=1, stale=1.
    assert_eq!(cov.mentions_linked, 3);
    assert_eq!(cov.mentions_ambiguous, 1);
    assert_eq!(cov.stale_doc_mentions, 1);
}

#[test]
fn impact_reaches_docs_and_cli_renders_needs_review() {
    let (g, _) = build_fixture();
    let affected = impact_of(&g, "alphaOne");
    assert!(
        affected
            .iter()
            .any(|a| a.uid == doc_section_uid(REPO, "docs/guide.md", "using-alphaone")),
        "the section mentioning alphaOne is in its blast radius: {affected:?}"
    );

    let rendered = render_impact_for_test(&g, "alphaOne");
    assert!(
        rendered.contains("needs review"),
        "doc nodes never say WILL BREAK: {rendered}"
    );
    assert!(!line_containing(&rendered, "Using alphaOne").contains("WILL BREAK"));
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
