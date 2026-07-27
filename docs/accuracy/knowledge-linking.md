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
- `stale_doc_mentions` — references that matched **nothing** at any tier
  **and** read as an authorial claim the graph could plausibly have resolved:
  counted, never guessed into a phantom edge. Disjoint from the two above.
  Precisely: an unresolvable `PathRef` (an exact repo-relative path claim) is
  always counted here; an unresolvable `InlineCode` reference is counted here
  only when its text is **symbol-shaped** — contains `::`/`.`, or is
  compound-case (has both an ASCII lower and an ASCII upper letter, e.g.
  `renamedSymbol`) — reading like a real, broken symbol reference.
- `unresolved_plain_refs` — the other half of an unresolvable `InlineCode`
  miss: text that is **not** symbol-shaped — a bare all-lowercase word or a
  `SCREAMING_SNAKE_CASE` token (a constant, a config key, a CLI flag, or prose
  that merely sits in backticks). The graph has no node for a raw constant or
  a struct/JSON field (there is no `Const`/field-level `NodeKind`), so this is
  not evidence a doc is lying — only evidence the graph's own reach stops
  above field/constant granularity. Disjoint from `stale_doc_mentions` and
  the two `mentions_*` counters; **never** folded into the drift signal (see
  [Reading the numbers](#reading-the-numbers) for why this split exists and
  [Honest bounds](#honest-bounds-m1-scope-restated-precisely) for what it
  does not fix).
- A `FenceToken` miss is counted **nowhere** — a fenced code block's
  identifier-looking tokens are incidental example vocabulary, never an
  authorial claim the way an inline `` `code` `` span or a link is (F1, K2).
- `doc_comments` — `Documents` edges: doc comments syntactically adjacent to
  their symbol's declaration (Extracted 0.95, always — see
  [The knowledge plane → confidence bands](../src/concepts/knowledge.md#edges-the-confidence-bands)).

## Results

**Reclassified at commit `1b94957a1f26`** (dirty at capture time — this F3
docs pass was mid-flight; the plane links its own in-progress documentation
same as any other). This is the K7 review fix wave's F2 change: `InlineCode`
misses are now split by symbol-shape between `stale_doc_mentions` and the new
`unresolved_plain_refs` (see [What is counted](#what-is-counted) above and
[Reading the numbers](#reading-the-numbers) below for why). The two prior
measurements below are kept for the historical record — they predate the
split, so their one undifferentiated "stale" number is not directly
comparable to either new column, only to their sum.

| metric | at `16c2a5503c66` (original) | at `1b94957a1f26` (post-F2, this report) |
|---|---:|---:|
| `files_indexed` (all planes) | 260 | **260** |
| `nodes` / `edges` (whole graph, all planes) | 5,747 / 37,732 | **5,764** / **37,918** |
| `docs` | 63 | **64** |
| `sections` | 635 | **644** |
| `mentions_linked` | 1,459 | **1,538** |
| &nbsp;&nbsp;of which `mentions_ambiguous` | 608 | 628 |
| `stale_doc_mentions` | 4,404 (undifferentiated) | **2,420** |
| `unresolved_plain_refs` | *(did not exist yet)* | **2,122** |
| `doc_comments` (`Documents` edges) | 1,406 | **1,408** |

`docs`/`sections`/`nodes`/`edges` grew slightly between the two measurements
from ordinary corpus growth (this report and the surrounding fix-wave commits
added their own new prose and doc comments — the plane linking its own
in-progress documentation, as the note above says), not from the
reclassification itself, which only moves counts between `stale_doc_mentions`
and the new `unresolved_plain_refs` column. The two are best compared as
sums: `2,420 + 2,122 = 4,542` today against `4,404` originally — a ~138
increase attributable to that ordinary corpus growth, not to the fix. What
the fix changes is entirely how that (roughly stable) total is split: instead
of one undifferentiated "stale" bucket, roughly half now reads
`unresolved_plain_refs` (schema-invisible constants/fields/config-keys) and
half stays `stale_doc_mentions` (symbol-shaped misses — the part worth
treating as a drift signal).

The real `strata index .` summary line at `1b94957a1f26`, byte-identical to
the coverage struct above:

```
knowledge:      64 doc(s), 644 section(s); 1538 mention(s) linked (628 ambiguous), 2420 stale, 2122 plain unresolved
```

## Reading the numbers

**64 docs, 644 sections.** This repository's `docs/` tree (the mdBook manual
under `docs/src/`, the accuracy reports under `docs/accuracy/`, the design
specs and implementation plans under `docs/specs/`/`docs/plans/`, the
top-level `docs/strata-design.md`) plus root/nested READMEs — a genuinely rich
corpus for a first real-repo measurement, not a toy.

**1,408 doc comments — the single largest, highest-confidence bucket.** Every
one is an Extracted 0.95 `Documents` edge: the parser observed the comment
sitting immediately above the declaration, a syntactic fact, not an inference.
This is also the **most heavily self-documented** part of the corpus — this
codebase's own convention of long, load-bearing `///`/`"""`/JSDoc blocks on
nearly every public item (visible throughout this very report's citations)
means the doc-comment signal dominates by volume, exactly as intended: a doc
comment is cheaper to keep honest than prose, and the plane rewards that.

**1,538 mentions linked, 628 of them ambiguous (41%).** A high ambiguous
fraction is expected, not a defect, in a codebase whose accuracy reports
constantly cite short, common inline-code names (`text`, `path`, `name`) that
collide across dozens of structs/functions — the Ambiguous band exists
precisely so a collision fans out honestly instead of guessing one winner.

**2,420 stale, 2,122 plain unresolved — the K7 fix wave's split (F2), and
why it exists.** Before this fix, this section reported ONE undifferentiated
"stale" number (4,404, "3× the linked count") and explained it via a sampled
fitness analysis: 349 inline-code identifiers pulled from the design docs,
implementation plans, and the manual, checked by hand against the live graph
(`strata query <token>`) independently of the plane's own resolution. **162
of the 349 (46%) had no graph match at all**, and every one of those 162
fell into one of four honest, structural categories, **none of which was
"the docs are lying":**

1. **Constants are never extracted as graph nodes, in any language.** There is
   no `Const`/`Constant` `NodeKind` among the 21 that exist (see
   [Graph schema](../src/reference/schema.md#nodekind)). Every calibration
   constant the *other* accuracy reports document in exhaustive detail —
   `CONF_BARE_MULTI`, `CONF_SAME_FILE`, `CONF_TYPE_QUALIFIED`,
   `KNOW_DOC_COMMENT`, `MAX_NODES`, `DEFAULT_DB`, `PROTOCOL_VERSION`, and
   twenty more sampled here — is **permanently** unresolvable, correctly
   named, and (Rust's `SCREAMING_SNAKE_CASE` constant convention) permanently
   **plain-shaped**: not symbol-shaped by any of the F2 rule's tests.
2. **Struct/JSON fields are not their own nodes either.** `by_fqn`, `by_name`,
   `body_range`, `spec_path`, `calls_total`, `mentions_linked`,
   `start_line`/`end_line`, `imports_in`/`imports_out` — real fields,
   accurately named, permanently unresolvable for the same reason as a
   constant, and (this codebase's `snake_case` field convention)
   **plain-shaped** too, same as category 1.
3. **A `NodeKind`/`EdgeKind` *variant name* is not a node's *own* name.**
   `ApiOperation`, `GraphqlField`, `LambdaFn`, `IamRole`, `HasColumn`,
   `MemberOf` name real Rust enum variants (see
   [Graph schema](../src/reference/schema.md)), but `query` matches node
   *instance* names (`getUser`, `UserRole`) — no node is literally named
   `"ApiOperation"`. Rust's `PascalCase` type-name convention makes every one
   of these **symbol-shaped** (compound-case), so the F2 split does **not**
   move this category — a doc correctly discussing the *kind* still reads
   `stale_doc_mentions`. See [Honest bounds](#honest-bounds-m1-scope-restated-precisely).
4. **External vocabulary and illustrative examples were never symbol
   references** — and this category **splits by shape**. Tree-sitter's own
   `snake_case` AST node-kind strings (`call_expression`, `field_expression`,
   `scoped_identifier`) and Terragrunt's `snake_case` HCL built-ins
   (`read_terragrunt_config`, `find_in_parent_folders`) are plain-shaped, so
   F2 moves them to `unresolved_plain_refs`. `PascalCase`/`camelCase`
   illustrative vocabulary — AWS/.NET SDK types (`PutObjectCommand`,
   `HttpRequestMessage`) and the manual's deliberately-fictional worked-example
   names (`PolicyOperationsFunction`, `getPolicyStats`) — is symbol-shaped, so
   it stays `stale_doc_mentions`, same residual as category 3.

Put together, at the time of that original sample: categories 1+2 (fields and
constants, ~130 of the 162, ~80%) are exactly what F2 targets — bulk
structural-granularity noise, now `unresolved_plain_refs`. Categories 3 and
part of 4 (~10 plus the `PascalCase` half of ~20) are **not** fixed by a
shape test alone, because a `PascalCase` kind name or illustrative type
*looks* exactly like a real, broken symbol reference — that residual is
`stale_doc_mentions`'s honest bound, not a gap in this fix; see
[Honest bounds](#honest-bounds-m1-scope-restated-precisely) below. This
repo's own aggregate mirrors that reasoning at scale: `unresolved_plain_refs`
(2,122) lands close to, and `stale_doc_mentions` (2,420) somewhat above, an
even split of the old undifferentiated total — consistent with a corpus whose
non-drift noise is dominated by the plain-shaped, high-volume categories (1
and 2) but still carries a real `PascalCase`-shaped residual (3 and part of
4). Zero of the 162 originally sampled traced to a confirmed
rename-left-behind either before or after this split: real drift is rare in
a codebase under this much internal cross-referencing discipline, and F2's
job is narrowing the counter toward that rare-but-real signal, not claiming
to have reached it exactly.

## Verified example chains

Four concrete links, each opened and confirmed against the real source at
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

**3. An `unresolved_plain_refs` mention (K7 F2's new bucket, not stale).**
`CONF_BARE_MULTI` (`docs/src/concepts/confidence.md` line 78, and
`docs/accuracy/ts-resolution.md`/`py-extraction.md`) accurately names a real
constant, `pub const CONF_BARE_MULTI: f32 = 0.35;`
(`crates/strata-index/src/build.rs:56`). `strata query CONF_BARE_MULTI`
returns **no match**, and its text — `SCREAMING_SNAKE_CASE`, no lowercase
letter — fails `inline_code_looks_symbol_shaped`
(`crates/strata-index/src/knowledge.rs`), so this reference now lands in
`unresolved_plain_refs`, **not** `stale_doc_mentions`. Confirmed *why* by
reading the source: no `Const` `NodeKind` exists, so a constant can never be
a graph node, ever, regardless of how accurately or how often a doc names
it — but that permanent unresolvability is a **graph-reach bound**, not a
claim the doc is lying, which is exactly the distinction the new counter
exists to make visible. (Before this fix, this exact reference was example
#3's "confirmed genuinely stale" case in this report; it is the concrete,
single-reference proof that the reclassification changed a real citation's
bucket, not just the aggregate counts.)

**4. A `stale_doc_mentions` mention that the shape split does NOT catch
(the honest residual).** `PolicyOperationsFunction`
(`docs/src/concepts/cross-boundary.md` line 139, inline code) is a
deliberately-fictional worked-example Lambda name — never meant to name a
real node in this graph. `strata query PolicyOperationsFunction` returns
**no match**, and its text is `PascalCase` (has both an ASCII lower and
upper letter) — symbol-shaped by the F2 rule, exactly like a genuine broken
reference such as `renamedSymbol` would be — so it stays
`stale_doc_mentions`. This is [Honest bounds](#honest-bounds-m1-scope-restated-precisely)'s
residual made concrete: a shape test cannot distinguish "a real symbol that
got renamed" from "a `PascalCase` illustrative name that was never a symbol
reference" — both look identical to `inline_code_looks_symbol_shaped`. F2
narrows the counter toward genuine drift; it does not claim to reach it
exactly.

## Honest bounds (M1 scope, restated precisely)

- **The `stale_doc_mentions` / `unresolved_plain_refs` split is a SHAPE
  heuristic, not a semantic one (K7 F2).** `inline_code_looks_symbol_shaped`
  asks only "does this text look like a symbol name" (contains `::`/`.`, or
  is compound-case) — it has no idea what the text actually refers to. That
  means it reliably sorts the highest-volume noise category (bare
  `snake_case`/`SCREAMING_SNAKE_CASE` constants, fields, and config keys —
  categories 1/2 in [Reading the numbers](#reading-the-numbers), ~80% of the
  originally sampled misses) out of the drift signal, but it does **not**
  distinguish a genuinely renamed/removed `PascalCase`/`camelCase` symbol
  from a `PascalCase`/`camelCase` reference that was never a symbol to begin
  with — a `NodeKind` variant name (`ApiOperation`), an external SDK type
  (`PutObjectCommand`), or a fictional worked-example name
  (`PolicyOperationsFunction`, [verified example #4](#verified-example-chains))
  all still read `stale_doc_mentions`. `stale_doc_mentions` is therefore
  narrowed toward genuine drift, not purified to it — it is still a
  recall-biased upper bound the same way every other "surfaced, not
  filtered" number in this engine is (§15.6, `will_break_label`'s own
  design). A hypothetical further refinement (out of scope here) would need
  the reference's *kind context* — is it in a "see the X type" sentence vs a
  broken-link sentence — not just its shape.
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
  (This repo's own doc-comment count, 1,408, is entirely outer `///`/JSDoc/
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
