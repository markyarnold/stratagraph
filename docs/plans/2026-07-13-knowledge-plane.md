# Knowledge Plane Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship the knowledge plane per `docs/specs/2026-07-13-knowledge-plane-design.md` — docs, doc comments, and spec descriptions as first-class graph citizens with deterministic linking, lexical search, and token-budgeted agent guidance.

**Architecture:** A new pure `strata-knowledge` crate parses markdown into section models with extracted references; `strata-index` gains a `build_knowledge_plane` step emitting `Doc`/`DocSection` nodes and `Documents`/`Mentions` edges (banded, drift-counted); bodies are never stored — `guidance` reads spans from disk at query time; a tantivy index at `.strata/docs.idx` serves `search_docs`. Two new MCP tools (+ CLI siblings), a `docs` bucket on `context`, a capped `docs:` line on `blast --format agent`, and steering/kit updates.

**Tech Stack:** Rust workspace (edition 2021), pulldown-cmark (markdown), tantivy (lexical index), existing tree-sitter analyzers, DuckDB store, serde. No ML anywhere.

## Global Constraints

- **Never confidently wrong:** every edge banded per the spec §2 table; unresolvable refs are counted (`stale_doc_mentions`), never guessed. Ambiguous = 0.35 fan-out, one edge per candidate.
- **Bodies-from-disk:** no document body text in the graph, the store, or any artifact. Spans only. (Spec decisions 4, §8.)
- **Token budgets are tested requirements:** `guidance` default budget 4,800 chars (~1,200 tokens); per-section cap 1,200 chars; blast `docs:` line ≤ 200 chars, top 3 + count. (Spec §4, §5.)
- **Additive schema only:** new `NodeKind::{Doc, DocSection}`, `EdgeKind::{Documents, Mentions}`; `Node` struct unchanged; `RawSymbol` gains `doc_span: Option<Span>` (serde-default) with an `ANALYZER_SCHEMA_VERSION` bump; `OperationDef` gains `description: Option<String>` (serde-default). Incremental==full must hold.
- **Gates per task:** red-first tests; `cargo test -p <crate>` plain with exit codes read (never pipe a gate through a filter that can't distinguish pass from no-output); `cargo clippy --all-targets -- -D warnings`; `cargo fmt --check`. Workspace test + `detect_changes` before each PR.
- **UID stability:** section UIDs are `doc|<repo>|<path>|<path>#<anchor>|` with GitHub-style anchors (duplicate anchors suffixed `-1`, `-2`). Doc-comment sections: fqn `<source-path>#doc:<symbol-fqn>`.
- **Commit style:** as the repo does — `feat(knowledge): …`, trailer `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`. Branch per task or one `feat/knowledge-plane` branch with per-task commits (executor's choice; PRs to `main`).
- Dependency pins: `pulldown-cmark` and `tantivy` added via `cargo add` (resolves current stable; grammars/deps are exact-pinned in Cargo.lock as the repo already does).

---

### Task K1: `strata-knowledge` crate — markdown model + ref extraction (pure)

**Files:**
- Create: `crates/strata-knowledge/Cargo.toml`
- Create: `crates/strata-knowledge/src/lib.rs`
- Modify: `Cargo.toml` (workspace members already glob `crates/*` — verify, no edit needed if globbed)

**Interfaces:**
- Consumes: `strata_core::Span` (existing: `{ start_line, start_col, end_line, end_col }`, 1-based lines).
- Produces (used by K2, K5):
  ```rust
  pub struct DocModel { pub path: String, pub sections: Vec<DocSectionModel> }
  pub struct DocSectionModel {
      pub heading: String,      // heading text; "(preamble)" for pre-heading content
      pub anchor: String,       // github-slug, deduped with -1/-2; "preamble" for preamble
      pub span: Span,           // heading line .. line before next same-or-higher heading
      pub body_range: (usize, usize), // byte offsets into the source (for K5 indexing)
      pub refs: Vec<DocRef>,
  }
  pub enum DocRefKind { InlineCode, FenceToken, PathRef }
  pub struct DocRef { pub text: String, pub kind: DocRefKind }
  pub fn parse_markdown(path: &str, content: &str) -> DocModel
  pub fn github_slug(heading: &str) -> String
  ```

- [ ] **Step 1: Scaffold the crate and write the failing tests**

`crates/strata-knowledge/Cargo.toml`:
```toml
[package]
name = "strata-knowledge"
edition.workspace = true
version.workspace = true
license.workspace = true

[dependencies]
strata-core = { path = "../strata-core" }
pulldown-cmark = { version = "0.13", default-features = false }
```

Tests in `src/lib.rs` `#[cfg(test)] mod tests` (RED first):
```rust
#[test]
fn sections_split_on_headings_with_github_anchors() {
    let m = parse_markdown("docs/a.md", "# Alpha One\nbody\n## Beta & Gamma!\nmore\n# Alpha One\ntail\n");
    let a: Vec<(&str, &str)> = m.sections.iter().map(|s| (s.heading.as_str(), s.anchor.as_str())).collect();
    assert_eq!(a, vec![("Alpha One", "alpha-one"), ("Beta & Gamma!", "beta--gamma"), ("Alpha One", "alpha-one-1")]);
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
    assert!(texts.contains(&"src/lib.rs"), "link destination is a PathRef");
    assert!(texts.contains(&"build_graph"), "fence identifiers extracted");
    assert!(!texts.contains(&"if"), "short/keyword-ish tokens (<3 chars) excluded");
}

#[test]
fn refs_are_deduped_and_capped() {
    let many: String = (0..200).map(|i| format!("`sym{i}` ")).collect();
    let m = parse_markdown("d.md", &format!("# S\n{many}"));
    assert!(m.sections[0].refs.len() <= 64, "per-section ref cap");
    let m2 = parse_markdown("d.md", "# S\n`same` and `same` again\n");
    assert_eq!(m2.sections[0].refs.iter().filter(|r| r.text == "same").count(), 1);
}
```

- [ ] **Step 2: Run tests — verify they fail to compile (RED)**

Run: `cargo test -p strata-knowledge`
Expected: compile error — `parse_markdown` not defined.

- [ ] **Step 3: Implement the parser**

Core logic in `src/lib.rs`:
```rust
pub fn github_slug(heading: &str) -> String {
    heading.chars()
        .filter_map(|c| match c {
            c if c.is_ascii_alphanumeric() => Some(c.to_ascii_lowercase()),
            ' ' | '-' => Some('-'),
            _ => None, // punctuation dropped (so "Beta & Gamma!" → "beta--gamma": '&' drops, both spaces map)
        })
        .collect()
}

pub fn parse_markdown(path: &str, content: &str) -> DocModel {
    use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};
    // Walk events with offsets (Parser::new_ext(..).into_offset_iter()):
    // - On Start(Heading): close the open section at the previous line; open a new
    //   one (record heading text from following Text events, level, start offset).
    // - Section span: heading line .. line before next heading of same-or-higher level
    //   (track open sections as a stack by level; lower-level headings do NOT close
    //   the parent — M1 keeps sections FLAT: every heading starts a new section and
    //   closes the previous one, matching the tests above).
    // - Preamble: content before the first heading → section {heading: "(preamble)",
    //   anchor: "preamble"} only if non-blank.
    // - Refs per section:
    //   * Event::Code(t) (inline code): trimmed, single-line, len 3..=80 → InlineCode.
    //   * Start(Tag::CodeBlock): collect Text until End; tokenize on
    //     non-[A-Za-z0-9_:.] boundaries; keep tokens with an ascii letter, len >= 3;
    //     qualified tokens (containing :: or .) kept whole AND split into segments.
    //   * Start(Tag::Link{dest_url,..}): dest without scheme (no "://", no '#'-only)
    //     and containing '/' or '.' → PathRef.
    //   * Any InlineCode text containing '/' and a '.' extension shape → ALSO PathRef.
    //   * Dedupe (by (kind,text)) preserving first-seen order; cap 64 per section.
    // - Line numbers: precompute a byte-offset→line table from `content` once;
    //   convert event offsets. body_range = (section start byte, end byte).
    // Anchor dedup: HashMap<String, u32> count; suffix "-{n}" for repeats.
    …
}
```
(Implementer writes the full event loop; the tests above define the contract precisely. No filesystem access anywhere in this crate.)

- [ ] **Step 4: Run tests — verify GREEN**

Run: `cargo test -p strata-knowledge`
Expected: `test result: ok. 4 passed`

- [ ] **Step 5: Gate + commit**

Run: `cargo clippy -p strata-knowledge --all-targets -- -D warnings && cargo fmt --check` (read real exit codes)
```bash
git add crates/strata-knowledge Cargo.lock
git commit -m "feat(knowledge): strata-knowledge crate - markdown model + ref extraction (pure)"
```

---

### Task K2: plane builder — Doc/DocSection nodes, Mentions edges, coverage, drift, impact "needs review"

**Files:**
- Modify: `crates/strata-core/src/model.rs` (NodeKind + EdgeKind additive variants + kind_name arms)
- Modify: `crates/strata-core/src/lib.rs` (impact reverse-walk kind set — find the existing traversed-kinds list next to `impact`/`reverse_walk` and add `Documents`, `Mentions`)
- Create: `crates/strata-index/src/knowledge.rs`
- Modify: `crates/strata-index/src/lib.rs` (markdown collection + wiring + `knowledge:` summary line + coverage struct export)
- Modify: `apps/strata-desktop/src-tauri/src/subgraph.rs` (`plane_of` exhaustive match: `Doc | DocSection => "knowledge"`)
- Modify: `crates/strata-mcp/src/tools.rs` (graph_schema_json vocab: new kinds — mirror the MapsTo drift-guard precedent)
- Modify: `crates/strata-cli/src/lib.rs` (impact/blast renderers: doc kinds print verdict `needs review` instead of `WILL BREAK`)
- Modify: `crates/strata-index/src/changes.rs` (`detect_changes` affected docs → the CLI "docs to review" refs line is K6; here only ensure doc kinds flow through `AffectedNode` untouched)
- Test: `crates/strata-index/tests/knowledge_linking.rs` + fixtures `crates/strata-index/tests/fixtures/knowledge_repo/**` + extend `crates/strata-index/tests/confidence_bands.rs`

**Interfaces:**
- Consumes: K1's `parse_markdown`/`DocModel`.
- Produces (used by K3/K5/K6):
  ```rust
  // crates/strata-index/src/knowledge.rs
  pub struct KnowledgeLinkCoverage {
      pub docs: usize, pub sections: usize,
      pub mentions_linked: usize, pub mentions_ambiguous: usize,
      pub stale_doc_mentions: usize, pub doc_comments: usize, // doc_comments filled in K3
  }
  pub fn build_knowledge_plane(
      g: &mut Graph, repo: &str,
      docs: &[(String, strata_knowledge::DocModel)],
  ) -> KnowledgeLinkCoverage
  pub fn doc_section_uid(repo: &str, path: &str, anchor: &str) -> Uid // fqn = "{path}#{anchor}"
  // Confidence consts (band-guardrail-tested):
  pub const KNOW_MENTION_PATH: f32 = 0.95;   // Extracted
  pub const KNOW_MENTION_FQN: f32 = 0.80;    // Inferred
  pub const KNOW_MENTION_NAME: f32 = 0.70;   // Inferred
  pub const KNOW_AMBIGUOUS: f32 = 0.35;      // Ambiguous fan-out
  ```

- [ ] **Step 1: Fixture + failing integration tests (RED)**

Fixture `tests/fixtures/knowledge_repo/`:
- `src/app.ts`: `export function alphaOne() {}\nexport function beta() {}\n`
- `src/other.ts`: `export function beta() {}\n` (makes `beta` multi-candidate)
- `docs/guide.md`:
  ```markdown
  # Using alphaOne
  Call `alphaOne` before anything. See [the app](src/app.ts).
  ## Betas
  `beta` is ambiguous here. `vanishedSymbol` no longer exists.
  ```

`tests/knowledge_linking.rs` (helpers mirror `within_repo_collision.rs`: analyze fixture files, assemble graph — use/extend the existing assemble entry so `build_knowledge_plane` runs; expose a test-visible assemble that takes docs):
```rust
#[test]
fn path_ref_links_extracted_unique_fqn_inferred() {
    let (g, cov) = build_fixture(); // analyze ts files + parse_markdown(guide.md) + assemble
    let sec = doc_section_uid(REPO, "docs/guide.md", "using-alphaone");
    let out = mentions_of(&g, &sec); // outgoing Mentions: (dst, provenance, conf)
    assert!(out.iter().any(|(d, p, c)| d == &module_uid("src/app.ts")
        && *p == Provenance::Extracted && (*c - 0.95).abs() < 1e-6), "path ref 0.95");
    assert!(out.iter().any(|(d, p, c)| d == &fn_uid("src/app.ts", "alphaOne")
        && *p == Provenance::Inferred && (*c - 0.80).abs() < 1e-6), "unique name→fqn 0.80");
}

#[test]
fn multi_candidate_name_fans_out_ambiguous_and_stale_is_counted_not_edged() {
    let (g, cov) = build_fixture();
    let sec = doc_section_uid(REPO, "docs/guide.md", "betas");
    let beta_edges: Vec<_> = mentions_of(&g, &sec).into_iter()
        .filter(|(d, _, _)| d.to_string().contains("|beta|")).collect();
    assert_eq!(beta_edges.len(), 2, "one Ambiguous edge per candidate");
    assert!(beta_edges.iter().all(|(_, p, c)| *p == Provenance::Ambiguous && (*c - 0.35).abs() < 1e-6));
    assert_eq!(cov.stale_doc_mentions, 1, "vanishedSymbol counted, never edged");
    assert!(!g.edges().any(|e| e.src == sec && e.dst.to_string().contains("vanishedSymbol")));
}

#[test]
fn impact_reaches_docs_and_cli_renders_needs_review() {
    let (g, _) = build_fixture();
    let affected = impact_of(&g, "alphaOne"); // strata_core::impact incoming walk
    assert!(affected.iter().any(|a| a.uid == doc_section_uid(REPO, "docs/guide.md", "using-alphaone")),
        "the section mentioning alphaOne is in its blast radius");
    let rendered = render_impact_for_test(&g, "alphaOne"); // CLI renderer fn
    assert!(rendered.contains("needs review"), "doc nodes never say WILL BREAK: {rendered}");
    assert!(!rendered_doc_line(&rendered).contains("WILL BREAK"));
}
```
Extend `confidence_bands.rs`: `Documents`/`Mentions` edges satisfy the band invariant non-vacuously (≥1 real edge per band from the fixture; Ambiguous < 0.40, Inferred 0.40..0.90, Extracted ≥ 0.90).

- [ ] **Step 2: Run — verify RED** (`cargo test -p strata-index --test knowledge_linking`; compile errors for missing kinds/fns are the RED)

- [ ] **Step 3: Implement**

1. `strata-core` model: add `NodeKind::{Doc, DocSection}`, `EdgeKind::{Documents, Mentions}` (+ `kind_name`, serde as the existing variants do). Add both edge kinds to the impact reverse-walk set.
2. `knowledge.rs`: build lookup tables in one pass over `g.nodes()` — `by_fqn: HashMap<&str, Vec<Uid>>`, `by_name: HashMap<&str, Vec<Uid>>`, `by_path: HashMap<&str, Vec<Uid>>` (module/table/operation/file-bearing nodes). Then per doc: create the `Doc` node; per section: create the `DocSection` node **plus a `Doc —Contains→ DocSection` edge (Extracted 1.0, exactly how ApiId containment is emitted — `Contains` is never impact-traversed)**; per ref resolve **in priority order** `PathRef→by_path (0.95)`, then `by_fqn` exact (0.80 unique / fan-out), then `by_name` (0.70 unique / fan-out); refs resolving to the section's own Doc are skipped; unresolved → `stale_doc_mentions += 1`. Dedupe edges by (src,dst).
3. Indexer: `collect_markdown(repo)` with WalkBuilder — include `docs/**/*.md`, root `*.md`, any `README.md`; exclude `CHANGELOG*`, `.strata`, vendored (existing pruning). Parse → `build_knowledge_plane` after the data plane; print
   `knowledge:      {docs} doc(s), {sections} section(s); {linked} mention(s) linked ({ambiguous} ambiguous), {stale} stale`.
4. Renderers: in the CLI impact/blast table code, when `AffectedNode.kind` is `"Doc"`/`"DocSection"`, print verdict `needs review` (regardless of will_break bool; JSON keeps mechanical fields unchanged). Desktop `plane_of` arm `=> "knowledge"`; MCP `graph_schema_json` vocab + drift-guard test extended.

- [ ] **Step 4: Run — verify GREEN**: `cargo test -p strata-index` and `cargo test -p strata-core -p strata-mcp -p strata-desktop` — all suites ok.

- [ ] **Step 5: Gate + commit**

`cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check`
```bash
git add crates/strata-core crates/strata-index crates/strata-mcp crates/strata-cli apps/strata-desktop
git commit -m "feat(knowledge): plane builder - Doc/DocSection nodes, banded Mentions, drift counts, docs in the blast radius"
```

---

### Task K3: doc comments — `doc_span` in all four analyzers + `Documents` edges

**Files:**
- Modify: `crates/strata-core/src/analyze.rs` (`RawSymbol` + `doc_span: Option<Span>` serde-default; bump `ANALYZER_SCHEMA_VERSION` by 1 — read the current value first, do not assume it)
- Modify: `crates/strata-lang-rust/src/lib.rs` (or its analyze module), `crates/strata-lang-ts/src/analyze.rs`, `crates/strata-lang-py/src/analyze.rs`, `crates/strata-lang-cs/src/analyze.rs`
- Modify: `crates/strata-index/src/knowledge.rs` (doc-comment `DocSection` nodes + `Documents` edges; `doc_comments` coverage count; new const `KNOW_DOC_COMMENT: f32 = 0.95`)
- Test: each analyzer's extraction test file + `knowledge_linking.rs` additions

**Interfaces:**
- Consumes: K2's builder; analyzers' `AnalyzedFile`.
- Produces: doc-comment sections `fqn = "{source_path}#doc:{symbol_fqn}"`, `name = "doc: {symbol_name}"`; `Documents` edge DocSection→symbol, Extracted 0.95. `build_knowledge_plane` signature grows a param: `analyzed: &BTreeMap<String, AnalyzedFile>` (K2 call sites updated in this task).

- [ ] **Step 1: Failing analyzer tests (RED)** — one per language, e.g. Rust:
```rust
#[test]
fn rustdoc_block_sets_doc_span_adjacent_only() {
    let f = analyze("src/a.rs", "/// Adds one.\n/// Really.\npub fn add_one(x: i32) -> i32 { x + 1 }\n\n// detached comment\n\npub fn plain() {}\n");
    let add = f.symbols.iter().find(|s| s.name == "add_one").unwrap();
    let span = add.doc_span.expect("doc span captured");
    assert_eq!((span.start_line, span.end_line), (1, 2));
    assert!(f.symbols.iter().find(|s| s.name == "plain").unwrap().doc_span.is_none(),
        "a blank-line-separated comment is NOT a doc comment");
}
```
TS: `/** … */` immediately before (optionally `export`-wrapped) declaration. Python: docstring = first string-expression statement of a `def`/`class` body (span of the string). C#: contiguous `///` run immediately above. Each language also asserts the negative (detached → `None`).
Integration (knowledge_linking.rs):
```rust
#[test]
fn doc_comment_becomes_documents_edge_and_rides_impact() {
    let (g, cov) = build_fixture_with_doc_comments();
    let sec = doc_section_uid(REPO, "src/app.ts", "doc:alphaOne");
    let out = documents_of(&g, &sec);
    assert_eq!(out, vec![(fn_uid("src/app.ts", "alphaOne"), Provenance::Extracted, 0.95)]);
    assert!(cov.doc_comments >= 1);
    assert!(impact_of(&g, "alphaOne").iter().any(|a| a.uid == sec), "change the symbol → its doc needs review");
}
```

- [ ] **Step 2: RED run** (`cargo test -p strata-lang-rust -p strata-lang-ts -p strata-lang-py -p strata-lang-cs -p strata-index` — new tests fail).

- [ ] **Step 3: Implement** — per language, in the existing symbol-emission path find the declaration node, then walk `prev_sibling` chain collecting the contiguous comment block (no blank line between block end and declaration start; tree-sitter gives comment nodes + row gaps). Python instead inspects the body's first statement. Set `doc_span`. In `knowledge.rs`, after markdown sections: for every symbol with `doc_span`, emit the DocSection node + `Documents` edge (Extracted, `KNOW_DOC_COMMENT`), count `doc_comments`. Bump `ANALYZER_SCHEMA_VERSION` (+1 from current), confirm the incremental==full test still passes (serde-default keeps old caches loadable → full reparse on version bump is the existing behaviour; verify with the existing incremental suite).

- [ ] **Step 4: GREEN run**: the four analyzer crates + `strata-index` full — all ok; plus `cargo test -p strata-index --test incremental`.

- [ ] **Step 5: Gate + commit** (`clippy --workspace`, fmt)
```bash
git commit -am "feat(knowledge): doc comments - doc_span across TS/Py/Rust/C# + Extracted Documents edges"
```

---

### Task K4: spec descriptions — `OperationDef.description` (OpenAPI + GraphQL)

**Files:**
- Modify: `crates/strata-contract/src/lib.rs` (`OperationDef` + `description: Option<String>` serde-default)
- Modify: `crates/strata-contract/src/openapi.rs` (capture `summary` and `description` per operation: `Some(format!("{summary}\n\n{description}"))` when both, else whichever exists, trimmed; `None` when neither)
- Modify: `crates/strata-contract/src/graphql.rs` (apollo-parser field description of each root field)
- Test: `crates/strata-contract/tests/openapi.rs`, `crates/strata-contract/tests/graphql.rs`

**Interfaces:**
- Produces: `OperationDef.description: Option<String>` — read by K5 (indexing) and K6 (`guidance` serves it live by re-extracting from the spec at query time, so no staleness).

- [ ] **Step 1: Failing tests (RED)**
```rust
// openapi.rs tests
#[test]
fn operation_summary_and_description_are_captured() {
    let ops = extract_fixture(r#"{"openapi":"3.0.0","paths":{"/users":{"get":{"operationId":"listUsers","summary":"List users","description":"Paginated."}}}}"#);
    assert_eq!(ops[0].description.as_deref(), Some("List users\n\nPaginated."));
}
#[test]
fn absent_description_is_none() { /* op with neither field → None */ }

// graphql.rs tests
#[test]
fn sdl_description_is_captured() {
    let ops = extract_sdl("\"\"\"Fetch one user\"\"\"\ntype Query { getUser: String }");
    // description belongs to the FIELD; adapt to how the adapter walks fields:
    let get_user = ops.iter().find(|o| o.key == "Query.getUser").unwrap();
    assert_eq!(get_user.description.as_deref(), Some("Fetch one user"));
}
```
(Exact SDL shape: field-level docstrings sit above the field inside the type; write the fixture accordingly — `type Query {\n  \"\"\"Fetch one user\"\"\"\n  getUser: String\n}`.)

- [ ] **Step 2: RED run** (`cargo test -p strata-contract` — new tests fail on missing field).
- [ ] **Step 3: Implement** (field additive; estate identity/dedup untouched — description is NOT part of `(api_id, format, key)`; add one estate test asserting two repos' merged op keeps a description from either).
- [ ] **Step 4: GREEN**: `cargo test -p strata-contract -p strata-index` all ok (byte-identical graphs elsewhere).
- [ ] **Step 5: Gate + commit** — `feat(knowledge): capture OpenAPI/GraphQL operation descriptions (additive)`

---

### Task K5: tantivy lexical index + `search_docs`

**Files:**
- Modify: `crates/strata-index/Cargo.toml` (+ tantivy), `crates/strata-index/src/knowledge.rs` (index writer)
- Modify: `crates/strata-index/src/lib.rs` (write `.strata/docs.idx` during index, after plane build; estate: per-member)
- Modify: `crates/strata-mcp/Cargo.toml` (+ tantivy), `crates/strata-mcp/src/tools.rs` (`search_docs` tool; `ToolCtx` + `member_roots: Vec<PathBuf>` serde-independent additive field, default empty)
- Modify: `crates/strata-cli/src/lib.rs` + `src/main.rs` (CLI sibling `strata search-docs "<query>" [--limit N]`; workspace mode fills `member_roots` from the manifest)
- Test: `crates/strata-index/tests/docs_index.rs`, `crates/strata-mcp/src/tools.rs` tests

**Interfaces:**
- Produces:
  - Index schema fields: `uid` (STRING|STORED), `name` (TEXT|STORED), `path` (STRING|STORED), `anchor` (STRING|STORED), `kind` (STRING|STORED — `section|doc_comment|spec_description`), `body` (TEXT, indexed not stored… **stored** actually required for snippets: TEXT|STORED).
  - Documents indexed: every markdown section (body sliced via `body_range` from the in-memory content), every doc comment (span-sliced source), every `OperationDef.description`.
  - `search_docs { query: string, limit?: number=5 }` → `{ results: [{ uid, name, path, anchor, kind, score, snippet, matched_terms: [string] }] }` — snippet via tantivy `SnippetGenerator` (caps itself ~150 chars); labeled lexical.
  - Lookup path: `<repo_root>/.strata/docs.idx`; estate = search every `member_roots` index, merge by score desc, cap `limit`.

- [ ] **Step 1: Failing tests (RED)**
```rust
// strata-index/tests/docs_index.rs — build a temp repo with one md + index it,
// then open .strata/docs.idx with tantivy and assert a search for a distinctive
// term returns the section's uid.
#[test]
fn index_time_docs_idx_is_written_and_searchable() {
    let tmp = fixture_repo_with_md("# Retry policy\nAlways use exponential backoff.\n");
    run_index(&tmp);
    let hits = open_and_search(&tmp.join(".strata/docs.idx"), "backoff", 5);
    assert_eq!(hits[0].anchor, "retry-policy");
}

// strata-mcp tools tests
#[test]
fn search_docs_returns_capped_labeled_hits() {
    let (graph, ctx) = graph_with_docs_idx(); // helper builds a temp idx + ToolCtx{repo_root}
    let v = call_tool_ctx(&graph, &ctx, "search_docs", &json!({"query": "backoff"})).unwrap();
    let results = v["results"].as_array().unwrap();
    assert!(results.len() <= 5);
    assert!(results[0]["snippet"].as_str().unwrap().to_lowercase().contains("backoff"));
    assert!(results[0]["matched_terms"].as_array().unwrap().iter().any(|t| t == "backoff"));
}
#[test]
fn search_docs_without_index_is_empty_not_error() {
    let (graph, ctx) = graph_with_empty_ctx();
    let v = call_tool_ctx(&graph, &ctx, "search_docs", &json!({"query": "x"})).unwrap();
    assert_eq!(v["results"].as_array().unwrap().len(), 0);
}
```

- [ ] **Step 2: RED run.**
- [ ] **Step 3: Implement** — writer: fresh index dir per run (delete + recreate `.strata/docs.idx` atomically: build in `docs.idx.tmp`, rename); default tokenizer; commit once. Tool: open index read-only; `QueryParser::for_index(&index, vec![body, name])`; collect top `limit`; snippet generator on `body`; matched terms from the parsed query's term set. Missing/corrupt index → empty results plus `"note": "no docs index — run strata index"`. CLI subcommand renders `path#anchor (score) — snippet` lines.
- [ ] **Step 4: GREEN** — `cargo test -p strata-index -p strata-mcp -p strata-cli` all ok.
- [ ] **Step 5: Gate + commit** — `feat(knowledge): tantivy docs index + search_docs (lexical, explainable)`

---

### Task K6: `guidance`, `context` docs bucket, blast `docs:` line, detect-changes refs, budget guardrail, CLI siblings

**Files:**
- Modify: `crates/strata-core/src/lib.rs` (`ContextResult` + `docs: Vec<ContextDocRef>` — `{ uid, name, anchor, path, provenance, confidence }`; populated from incoming `Documents`/`Mentions`)
- Modify: `crates/strata-mcp/src/tools.rs` (`guidance` tool + schemas + `tools/list` now NINE tools; context payload carries `docs`)
- Modify: `crates/strata-index/src/changes.rs` + `crates/strata-cli/src/lib.rs` (blast agent-format `docs:` line ≤ 200 chars top-3+count; `detect-changes` CLI "docs to review (N): refs" line)
- Modify: `crates/strata-cli/src/lib.rs` + `main.rs` (CLI `strata guidance <symbol|file> [--budget N] [--section ANCHOR]`)
- Test: `crates/strata-mcp/src/tools.rs` tests (incl. the **budget guardrail**), `crates/strata-cli` render tests, `knowledge_linking.rs` context-bucket test

**Interfaces:**
- Consumes: K2/K3 edges + spans; K4 descriptions (re-extracted live from the spec file by path at query time); `ToolCtx.repo_root`/`member_roots`.
- Produces:
  ```text
  guidance { symbol?: string, file?: string, budget?: number (chars, default 4800),
             section?: string (anchor → return that single section, full) }
  → { target: {uid,name,kind},
      sections: [{ uid, path, anchor, provenance, confidence, text, truncated: bool }],
      budget_used: number, note?: string }
  Ordering: own doc comment → Documents → Mentions by confidence desc.
  Per-section cap 1200 chars with "… [truncated — fetch {path}#{anchor}]" marker.
  Body read: file at repo_root (or the owning member root) sliced by span lines;
  file missing → { text: "", truncated: false, note: "body unavailable" } entry-level.
  ```

- [ ] **Step 1: Failing tests (RED)** — the two that matter most shown in full:
```rust
#[test]
fn guidance_orders_by_tier_and_respects_budget() {
    let (graph, ctx) = fixture_with_fat_docs(); // one doc comment + one Documents + three Mentions,
                                                // each section body ~2000 chars on disk
    let v = call_tool_ctx(&graph, &ctx, "guidance", &json!({"symbol": "alphaOne"})).unwrap();
    let sections = v["sections"].as_array().unwrap();
    assert!(sections[0]["anchor"].as_str().unwrap().starts_with("doc:"), "own doc comment first");
    let total: usize = sections.iter().map(|s| s["text"].as_str().unwrap().len()).sum();
    assert!(total <= 4800 + 200, "default budget holds (marker slack), got {total}");
    assert!(sections.iter().any(|s| s["truncated"] == true), "fat sections truncate with a marker");
    assert!(sections.iter().any(|s| s["text"].as_str().unwrap().contains("[truncated — fetch")));
}

#[test]
fn blast_agent_format_docs_line_is_capped() {
    let out = blast_agent_format_for_fixture(); // file whose symbols have 6 linked sections
    let line = out.lines().find(|l| l.starts_with("docs:")).expect("docs line present");
    assert!(line.len() <= 200, "hard cap: {}", line.len());
    assert!(line.contains("+3 more"), "top-3 + count");
    assert!(line.contains("guidance"), "points at the drill-down tool");
}
```
Plus: `guidance {file}` aggregates the file's symbols' docs; `section` arg returns one full body; missing file on disk → `body unavailable` note (never an error); `context` docs bucket refs-only test; detect-changes render test for the "docs to review" refs line; `tools/list` count test 7→9.

- [ ] **Step 2: RED run.**
- [ ] **Step 3: Implement** — shared core in `strata-mcp` (or `strata-core` if the CLI needs it without MCP — follow the one-dispatch rule: CLI calls `call_tool_ctx` exactly like blast/detect-changes siblings do). Budget mechanics: iterate ordered sections, slice body lines from disk, take min(remaining_budget, 1200) chars at char-boundary, append marker when cut, stop at budget. Spec descriptions: when the target is an ApiOperation/GraphqlField, re-run the adapter's description extraction on `spec_path` and serve it as the first section (`anchor: "description"`).
- [ ] **Step 4: GREEN** — `cargo test -p strata-mcp -p strata-cli -p strata-index -p strata-core`; then FULL `cargo test --workspace`.
- [ ] **Step 5: Gate + commit** — `feat(knowledge): guidance + docs bucket + capped blast docs line + docs-to-review (token budgets tested)`

---

### Task K7: agent kits, manual docs, website, changelog, dogfood + accuracy report

**Files:**
- Modify: `crates/strata-cli/src/init/content.rs` (steering: tool list + the conditional MUST + honesty + commit-rule lines per spec §5 — exact text below; `CLAUDE_ROUTING` row; `KIRO_ROUTING` guidance clause), `crates/strata-cli/src/init/kiro.rs` (pre-edit prompt clause), skills content (`strata-guide`, `strata-exploring`)
- Modify: `docs/src/` — new page `concepts/knowledge.md` + SUMMARY entry; `reference/mcp.md` (nine tools, guidance/search_docs sections incl. budgets); `reference/cli.md` (two new commands); `concepts/planes.md` (fifth plane); `project/changelog.md` (Unreleased entry)
- Create: `docs/accuracy/knowledge-linking.md` (measured on the strata repo itself)
- Mirror: changed `docs/src/**` → `strataindex/src/content/docs/**` (+ nav.json entry for the new page) + build + push
- Test: content/init tests updated (steering phrase pins); kit token audit asserted (steering addition ≤ 10 lines; `docs:` line cap already tested in K6)

**Interfaces:** consumes everything; produces no code interfaces.

- [ ] **Step 1: Steering text (exact, both kits + AGENTS.md)** — add to the Always Do block:
```text
- **MUST act on the `docs:` line the pre-edit blast injects.** When it lists a
  section at ≥ 0.80 covering the file that you have not consulted this session,
  fetch `guidance` for the file BEFORE editing. Never call guidance
  unconditionally — the hook line is the trigger.
- **Doc guidance is repo knowledge, not ground truth.** Docs can be stale: the
  graph marks drift, and a mention below 0.40 or ambiguous is UNKNOWN — the
  same trust policy as every other band.
- **Report `detect_changes`' "docs to review" line in your pre-commit summary**
  and offer (never auto-apply) updates for stale sections.
```
Tool list additions: `guidance` (budgeted digest of what the repo knows about a symbol/file) and `search_docs` (lexical, explainable; use instead of grepping docs). Kiro pre-edit prompt gains: `If the injected blast output lists a docs: line at >=0.80 that you have not consulted, run the guidance tool for this file first.`
Steering/content tests pin these phrases (update `kiro_routing_*` + steering tests accordingly).

- [ ] **Step 2: Skills** — `strata-guide`: routing row `Repo conventions / "is there guidance on X?" → guidance / search_docs`; tool table rows. `strata-exploring`: open unfamiliar areas with `guidance {file}` + `search_docs` before reading code. No fifth skill.

- [ ] **Step 3: Manual + changelog** — `concepts/knowledge.md` covers: the model (§2 table reproduced), bodies-from-disk freshness rule, budgets table, drift + docs-in-blast-radius, degradation notes. Changelog Unreleased entry in the established user-facing voice.

- [ ] **Step 4: Dogfood + accuracy report** — rebuild release binary; `strata index .` on the strata repo; record the real `knowledge:` line; run `guidance strata-index/src/contract.rs` and `search_docs "confidence bands"`; write `docs/accuracy/knowledge-linking.md` with the measured counts (docs/sections/linked/ambiguous/stale/doc_comments) and 2–3 verified example chains. Then the client-repo pass (two private client repos) — numbers only, no client content in the report.

- [ ] **Step 5: mdbook + website** — `mdbook build docs` green; mirror changed pages + nav.json to `strataindex`, `npm run build` green, commit + push both repos.

- [ ] **Step 6: Gate + commit(s)** — full workspace test + clippy + fmt + `detect_changes` (report per-plane + docs-to-review — this feature reviewing itself); commit `feat(knowledge): agent kits + docs + accuracy report`; PR.

---

## Execution notes

- **Order is K1→K7**; K3 and K4 are independent of each other (both depend on K2) and may run in either order or parallel worktrees.
- **Independent review per task** (the repo's established gate): reviewer verifies empirically — runs the tests, checks bands against the spec table, and for K6 re-runs the budget guardrail with a fatter fixture than the author used.
- **detect_changes before every commit**; pause on HIGH/CRITICAL per steering (expected HIGH on core-file edits — calibrate against the suites as this batch has done throughout).
- The spec is the arbiter: `docs/specs/2026-07-13-knowledge-plane-design.md`. Any deviation discovered mid-task goes back to Mark, not into the code silently.
