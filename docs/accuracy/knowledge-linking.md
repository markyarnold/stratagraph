# Knowledge-Plane Linking

Measured coverage of Strata's **knowledge plane**: how much of the repository's
own written knowledge — markdown under `docs/`, root/nested READMEs, code doc
comments, and (design-scoped, not yet exercised by this corpus) OpenAPI/GraphQL
spec descriptions — the knowledge-plane builder (`build_knowledge_plane`,
`crates/strata-index/src/knowledge.rs`) turns into `Doc`/`DocSection` nodes and
banded `Documents`/`Mentions` edges, and at what honest-provenance tier. This
is the knowledge-plane companion to `docs/accuracy/data-linking.md`,
`infra-linking.md`, and the others — same discipline, different corpus.

## This report measures a different kind of corpus, on purpose

Every other report in this directory is measured against a **committed,
hermetic fixture** with a `_matches_committed_numbers` consistency test and a
`_meets_documented_floors` CI gate. This one is not. The design (§6,
`docs/specs/2026-07-13-knowledge-plane-design.md`) calls for "Dogfood: the
strata repo itself (rich `docs/` tree), then the usual real-repo verification
pass" — the knowledge plane's whole point is to link a repository's *actual*
documentation, so the most honest first measurement is the repository this
engine ships in, not a small hand-built fixture standing in for one. That
means:

- **The corpus is a live, moving target** — this repo's own `docs/` tree, doc
  comments, and specs, as they stand at measurement time (see
  [Regeneration](#regeneration) for the exact commit this was captured
  against). The numbers **will drift** as the repo grows; that is expected,
  not a regression.
- **There is no CI floor gate here.** A floor makes sense against a frozen
  fixture; it does not against a corpus that changes with every commit. The
  fixture-based correctness tests that DO gate CI live in
  `crates/strata-index/tests/knowledge_linking.rs` (K1–K3 fixture, one file
  per resolution tier) and `crates/strata-mcp/src/tools.rs` (the `guidance`
  budget guardrail, `search_docs` dispatch tests) — this report is a
  **measurement**, not a **gate**.
- **No client-repo content appears here.** Only this repository's own
  markdown, doc comments, and code — nothing from any customer or partner
  codebase.

## Regeneration

The measured counts below come from two independent runs against this exact
commit, agreeing byte-for-byte:

1. The real CLI, release build, exactly as a user would run it:
   ```
   cargo build --release -p strata-cli
   ./target/release/strata index .
   ```
   (the `knowledge:` line of the summary)
2. A dedicated dogfood test that calls the SAME production entry point
   (`index_repo_with_options`) the CLI uses, over an in-memory store, and
   prints the full `KnowledgeLinkCoverage` — including `doc_comments`, which
   the CLI's human-readable summary line does not print:
   ```
   cargo test -p strata-index --test knowledge_linking print_self_repo_knowledge_coverage -- --ignored --nocapture
   ```

## What is counted

Per the `KnowledgeLinkCoverage` struct (`crates/strata-index/src/knowledge.rs`),
over every markdown file the default collection set ingests
(`docs/**/*.md`, a root `*.md`, or a nested `README.md`; `CHANGELOG*` and
vendored/`.strata` trees excluded) plus every doc comment the four language
analyzers captured (`RawSymbol::doc_span`):

- `docs` — `Doc` nodes created, one per ingested markdown file.
- `sections` — `DocSection` nodes created, one per parsed heading (plus a
  non-blank preamble section).
- `mentions_linked` — markdown references that resolved to at least one
  `Mentions` edge (a unique hit **or** an ambiguous fan-out both count here).
- `mentions_ambiguous` — the **subset** of `mentions_linked` that resolved to
  2+ candidates (Ambiguous 0.35 fan-out, one edge per candidate) rather than
  one confident hit. Not disjoint from `mentions_linked` — see the struct's
  own doc comment.
- `stale_doc_mentions` — references that matched **nothing** at any tier:
  counted, never guessed into a phantom edge. Disjoint from the two above.
- `doc_comments` — `Documents` edges: doc comments syntactically adjacent to
  their symbol's declaration (Extracted 0.95, always — see
  [The knowledge plane → confidence bands](../src/concepts/knowledge.md#edges-the-confidence-bands)).

## Results

Measured on this repository at commit `16c2a5503c66` (dirty at capture time —
K7's own docs edits were mid-flight; the plane links its own in-progress
documentation same as any other):

| metric | value |
|---|---:|
| `files_indexed` (all planes) | **260** |
| `nodes` / `edges` (whole graph, all planes) | **5,747** / **37,732** |
| `docs` | **63** |
| `sections` | **635** |
| `mentions_linked` | **1,459** |
| &nbsp;&nbsp;of which `mentions_ambiguous` | 608 |
| `stale_doc_mentions` | **4,404** |
| `doc_comments` (`Documents` edges) | **1,406** |

The real `strata index .` summary line, byte-identical to the coverage struct
above:

```
knowledge:      63 doc(s), 635 section(s); 1459 mention(s) linked (608 ambiguous), 4404 stale
```

## Reading the numbers

**63 docs, 635 sections.** This repository's `docs/` tree (the mdBook manual
under `docs/src/`, the accuracy reports under `docs/accuracy/`, the design
specs and implementation plans under `docs/specs/`/`docs/plans/`, the
top-level `docs/strata-design.md`) plus root/nested READMEs — a genuinely rich
corpus for a first real-repo measurement, not a toy.

**1,406 doc comments — the single largest, highest-confidence bucket.** Every
one is an Extracted 0.95 `Documents` edge: the parser observed the comment
sitting immediately above the declaration, a syntactic fact, not an inference.
This is also the **most heavily self-documented** part of the corpus — this
codebase's own convention of long, load-bearing `///`/`"""`/JSDoc blocks on
nearly every public item (visible throughout this very report's citations)
means the doc-comment signal dominates by volume, exactly as intended: a doc
comment is cheaper to keep honest than prose, and the plane rewards that.

**1,459 mentions linked, 608 of them ambiguous (42%).** A high ambiguous
fraction is expected, not a defect, in a codebase whose accuracy reports
constantly cite short, common inline-code names (`text`, `path`, `name`) that
collide across dozens of structs/functions — the Ambiguous band exists
precisely so a collision fans out honestly instead of guessing one winner.

**4,404 stale — high, and worth explaining rather than hiding.** Read as "how
many documentation references break," this number would be alarming: it is
**3× the linked count**. It is not that. To find out what it actually
measures, this report sampled 349 inline-code identifiers pulled from the
design docs, implementation plans, and the manual, and checked each against
the live graph (`strata query <token>`) independently of the plane's own
resolution. **162 of the 349 (46%) had no graph match at all** — and every one
of those 162, checked by hand against the source, falls into one of four
honest, structural categories, **none of which is "the docs are lying":**

1. **Constants are never extracted as graph nodes, in any language.** There is
   no `Const`/`Constant` `NodeKind` among the 21 that exist (see
   [Graph schema](../src/reference/schema.md#nodekind)) — Rust extraction
   captures fns/structs/enums/unions/traits/impls/mods/uses/calls, never a
   top-level `const`. So every one of the dozens of calibration constants the
   *other* accuracy reports document in exhaustive detail —
   `CONF_BARE_MULTI`, `CONF_SAME_FILE`, `CONF_TYPE_QUALIFIED`,
   `KNOW_DOC_COMMENT`, `MAX_NODES`, `DEFAULT_DB`, `PROTOCOL_VERSION`, and
   twenty more sampled here — is **permanently** unresolvable, correctly
   named, forever counted stale. Verified concretely: `CONF_BARE_MULTI` is a
   real constant (`pub const CONF_BARE_MULTI: f32 = 0.35;`,
   `crates/strata-index/src/build.rs:56`), accurately cited in
   `docs/src/concepts/confidence.md`, `docs/accuracy/ts-resolution.md`, and
   `docs/accuracy/py-extraction.md` — and `strata query CONF_BARE_MULTI`
   returns no match. The doc is right; the graph's granularity simply stops
   above field/constant level, by design (the same bound the code plane
   states for `HasColumn`'s columns and every plane's structural members).
2. **Struct/JSON fields are not their own nodes either.** `stale_doc_mentions`
   itself — the very field this paragraph is about — is one: a real field of
   `KnowledgeLinkCoverage`, accurately named in this repo's own source and
   docs, and permanently unresolvable for the same reason as a constant.
   `by_fqn`, `by_name`, `body_range`, `spec_path`, `calls_total`,
   `mentions_linked`, `start_line`/`end_line`, `imports_in`/`imports_out` — all
   the same category.
3. **A `NodeKind`/`EdgeKind` *variant name* is not a node's *own* name.**
   `ApiOperation`, `GraphqlField`, `LambdaFn`, `IamRole`, `HasColumn`,
   `MemberOf` name real Rust enum variants documented at length in
   [Graph schema](../src/reference/schema.md), but `query` matches node
   *instance* names (`getUser`, `UserRole`) — no node is literally named
   `"ApiOperation"` — so a doc correctly discussing the *kind* never resolves
   to an instance.
4. **External vocabulary and illustrative examples were never symbol
   references.** Tree-sitter's own AST node-kind strings quoted in prose
   (`call_expression`, `field_expression`, `scoped_identifier`), Terragrunt's
   HCL built-in functions (`read_terragrunt_config`, `find_in_parent_folders`
   — confirmed in `crates/strata-infra/src/terragrunt.rs`'s own doc comment,
   describing *Terragrunt's* functions, not ours), AWS/.NET SDK types used as
   illustrative examples (`PutObjectCommand`, `HttpRequestMessage`), and
   deliberately-fictional worked-example names in the manual's "Understanding
   Output" sections (`PolicyOperationsFunction`, `getPolicyStats`) all read as
   plausible identifiers to the extractor but were never meant to name a node
   in *this* graph.

Put together: of the 162 unresolved samples, the count traces to a
**structural granularity bound** (fields/constants: ~130), a **kind-vs-instance
mismatch** (~10), or **external/illustrative vocabulary** (~20) — **zero** to a
confirmed rename-left-behind. That is itself the honest finding for a codebase
under this much internal cross-referencing discipline: real drift is rare;
the counter is dominated by references that were always going to miss, by
construction, not by rot. A future refinement (out of scope for M1) could
narrow `stale_doc_mentions` to symbol-shaped candidates only (skip a token
that structurally can't be a `Function`/`Method`/`Class`/`Table`/`Doc`
target), which would sharply cut this count without changing a single link
that DOES resolve today.

## Verified example chains

Three concrete links, each opened and confirmed against the real source at
measurement time — not asserted from the aggregate counts alone.

**1. A `Mentions` edge (PathRef, Extracted 0.95).**
`docs/plans/2026-07-13-knowledge-plane.md`, under `### Task K2: plane
builder — Doc/DocSection nodes, Mentions edges, coverage, drift, impact
"needs review"` (anchor
`task-k2-plane-builder--docdocsection-nodes-mentions-edges-coverage-drift-impact-needs-review`),
line 172 reads:
```
- Create: `crates/strata-index/src/knowledge.rs`
```
An exact repo-relative path in inline code — the `PathRef` shape — resolving
to the real `Doc` node for `crates/strata-index/src/knowledge.rs`. Confirmed
live via `strata guidance crates/strata-index/src/knowledge.rs --file`, whose
output lists this section among the file's Extracted 0.95 mentions.

**2. A `Documents` doc-comment edge (Extracted 0.95).**
`crates/strata-index/src/knowledge.rs`, immediately above `pub struct
KnowledgeLinkCoverage`:
```rust
/// Coverage + drift counts [`build_knowledge_plane`] returns — the `knowledge:`
/// summary line's data, and (from K3 on) the vehicle for doc-comment counts.
///
/// `mentions_linked` and `mentions_ambiguous` are NOT disjoint: …
pub struct KnowledgeLinkCoverage { … }
```
A syntactically-adjacent outer `///` block — the parser-observed fact that
earns the `Documents` tier rather than `Mentions`. Confirmed live: `strata
query KnowledgeLinkCoverage` returns both the `Class`-kind node AND its
`doc:KnowledgeLinkCoverage` `DocSection`; `strata guidance
crates/strata-index/src/knowledge.rs --file` returns this exact comment text
as the first (highest-priority) section.

**3. A stale mention, confirmed genuinely stale (structural, not a typo).**
`CONF_BARE_MULTI` (`docs/src/concepts/confidence.md` line 78, and
`docs/accuracy/ts-resolution.md`/`py-extraction.md`) accurately names a real
constant, `pub const CONF_BARE_MULTI: f32 = 0.35;`
(`crates/strata-index/src/build.rs:56`). `strata query CONF_BARE_MULTI`
returns **no match** — confirmed stale by the plane's own definition (a
reference that resolves to nothing) — and confirmed *why* by reading the
source: no `Const` `NodeKind` exists, so a constant can never be a graph
node, ever, regardless of how accurately or how often a doc names it. This is
the honest bound §2 above documents, caught in the act on a single,
independently-verified reference.

## Honest bounds (M1 scope, restated precisely)

- **Fence tokens never fall through to the name tier (F1).** A fenced code
  block's identifier-looking tokens try the `fqn` tier only — incidental
  code-example vocabulary (`r.process(x)`'s `process`) never spuriously binds
  to an unrelated same-named symbol across the repo. Only an inline
  `` `code` `` span gets the weaker bare-name fallback, and only after an
  `fqn` miss.
- **The 0.70 bare-name tier is inline-code-only.** `KNOW_MENTION_NAME` (0.70)
  fires for a unique bare name in inline `` `code` `` — never for a fenced
  block, which stops at the `fqn` tier or fails.
- **Rust `//!` (inner doc comments) are excluded from forward capture
  entirely** — not deferred, not deprioritized. A `//!` documents the
  ENCLOSING scope (module/file/block), and attributing it to the next
  declaration would mint a confidently-wrong Extracted 0.95 edge to the wrong
  symbol. See [The knowledge plane → doc comments](../src/concepts/knowledge.md#doc-comments-per-language).
  (This repo's own doc-comment count, 1,406, is entirely outer `///`/JSDoc/
  docstring/XML-doc captures; no inner comment contributes.)
- **Markdown prose never earns `Documents`**, only `Mentions` — a paragraph
  discussing a symbol is not proof of documentation the way a doc comment's
  syntactic adjacency is.
- **This corpus does not exercise OpenAPI/GraphQL spec descriptions**
  (`OperationDef.description`): this repository ships no API spec of its own.
  That path is covered by the K4 fixture tests
  (`crates/strata-mcp/src/tools.rs`), not this report.
- **No semantic/embedding search, anywhere in this measurement.** Every
  number above is a deterministic graph traversal or a tantivy BM25 lexical
  match — zero ML, zero API calls, fully reproducible offline.
