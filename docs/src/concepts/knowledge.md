# The knowledge plane

The fifth plane: the repository's own written knowledge — READMEs, ADRs, guides
under `docs/`, doc comments, and OpenAPI/GraphQL spec descriptions — turned into
graph structure with the same discipline as the other four. "Never confidently
wrong" applies to what the repo says about itself exactly as it applies to a
`Calls` edge: every reference is banded, an unresolvable one is counted rather
than guessed, and a doc that no longer matches the code it describes is
surfaced, not silently trusted.

Retrieval is **structural and lexical, zero ML**: deterministic graph edges
(`Documents`, `Mentions`) plus a full-text index. No embeddings, no models, no
API keys, no generated summaries — the engine serves material; writing the
answer is the agent's job.

Read this alongside [The five planes](planes.md) (where the knowledge plane
sits among the other four) and `docs/accuracy/knowledge-linking.md` (the
measured numbers on this repository).

## Why it exists

An agent about to touch a symbol usually cannot see that an ADR already says
"never bypass the outbox here," or that a README section documents the exact
contract it is about to break. Grep can find *a* match, but it cannot tell you
whether a hit is authoritative, stale, or one of several same-named
candidates — and it cannot follow the reverse direction at all: which docs does
*this* change put at risk? The knowledge plane answers both, with the same
provenance-and-confidence contract the code, contract, and infra planes already
give you:

- an agent about to edit a file is told **what the repo already knows** about
  it (the pre-edit hook's `docs:` line, `guidance`),
- a change's blast radius includes the **documentation it stales**
  (`impact`/`detect_changes`'s "docs to review"), and
- the graph reports which docs are **already lying** — references to symbols
  that no longer exist (`stale_doc_mentions`).

## The model

A new UID namespace, `doc`, alongside `ts`/`py`/`cs`/`rust`/`contract`/`infra`/
`data`. The `Node` struct itself is unchanged — this is a purely additive
schema extension.

### Nodes

| Kind | One per | `name` | `fqn` | `path` / `span` |
|---|---|---|---|---|
| `Doc` | an ingested markdown file | filename | repo-relative path | the file / whole file |
| `DocSection` | a heading section | heading text | `<path>#<anchor>` | the file / heading to next same-or-higher heading |
| `DocSection` (doc comment) | a documented symbol | `doc: <symbol-name>` | `<source-path>#doc:<symbol-fqn>` | the source file / the comment block |

Anchors are GitHub-style slugs; a duplicate anchor within one file takes the
GitHub `-1`, `-2` suffix, so a section's uid stays stable across unrelated
edits elsewhere in the file. `Doc —Contains→ DocSection` is structural
membership, never traversed by `impact` (the same rule as the infra plane's
`AppSyncApi —Contains→` resolver/datasource membership).

An OpenAPI/GraphQL spec description creates **no node**: the `ApiOperation`/
`GraphqlField` node is itself the documented thing. `OperationDef` carries an
additive `description: Option<String>`, re-extracted live from the spec file
and served by `guidance`/indexed for `search_docs` — never stored a second
time as graph text.

### Edges: the confidence bands

| Kind | From → to | When | Provenance / confidence |
|---|---|---|---|
| `Documents` | `DocSection` → symbol | a doc comment syntactically adjacent to its symbol's declaration | Extracted **0.95** |
| `Mentions` | `DocSection` → any node | an exact repo-relative path reference (a markdown link destination, or a path-shaped token) | Extracted **0.95** |
| `Mentions` | `DocSection` → any node | a unique fully-qualified-name match, in a fenced code block **or** inline `` `code` `` | Inferred **0.80** |
| `Mentions` | `DocSection` → any node | a unique bare-name match, inline `` `code` `` **only** — a fenced code-block token never falls through to this tier | Inferred **0.70** |
| `Mentions` | `DocSection` → several candidates | a reference matching more than one node at whichever tier resolved it | Ambiguous **0.35** fan-out, one edge per candidate |
| *(no edge)* | — | a `PathRef`, or a symbol-shaped `InlineCode` reference, that resolves to nothing at any tier | counted as `stale_doc_mentions`, never guessed |
| *(no edge)* | — | a plain-word/`SCREAMING_SNAKE_CASE`-shaped `InlineCode` reference that resolves to nothing | counted as `unresolved_plain_refs`, never "stale" |
| *(no edge)* | — | a `FenceToken` reference that resolves to nothing | not counted anywhere (never an authorial claim) |

Three rules keep this band-honest:

- **Markdown prose never earns `Documents`.** Talking *about* a symbol in a
  paragraph is a mention, not proof of documentation — only a doc comment's
  syntactic adjacency (a parser-observed fact) reaches the Extracted
  `Documents` tier. Markdown always lands on `Mentions` instead, at whichever
  band its reference shape earns.
- **`mentions_linked` and `mentions_ambiguous` are not disjoint sets.**
  `mentions_ambiguous` is the subset of `mentions_linked` whose reference
  fanned out to 2+ candidates rather than resolving to one confident hit;
  `stale_doc_mentions` and `unresolved_plain_refs` are each disjoint from
  both (and from each other) — every unresolvable `PathRef`/`InlineCode`
  reference lands in exactly one of the two.
- **`stale_doc_mentions` is drift; `unresolved_plain_refs` is a graph-reach
  bound, not drift.** An unresolvable `PathRef` (an exact path claim to a
  file that doesn't exist) or a symbol-shaped `InlineCode` miss — contains
  `::`/`.`, or is compound-case like `renamedSymbol`/`DocSection` — reads as
  a real, broken reference and is counted as `stale_doc_mentions`. A
  `SCREAMING_SNAKE_CASE` token (`CONF_BARE_MULTI`) or a bare all-lowercase
  word (`foo`) is schema-invisible: there is no `Const`/field-level
  `NodeKind`, so the graph was never going to resolve it regardless of how
  accurately the doc names it. That is not evidence the doc is lying, so it
  is counted separately as `unresolved_plain_refs` and is expected to be
  **numerous** in a codebase with heavy constant/config-key cross-referencing
  — never folded into the drift signal. A `FenceToken` miss is never counted
  anywhere, at either bucket: incidental code-example vocabulary was never an
  authorial claim to begin with (F1, K2).

The `confidence_bands` guardrail suite covers both edge kinds non-vacuously,
the same discipline the [Confidence and provenance](confidence.md) page
describes for the other four planes.

### Doc comments, per language

`RawSymbol` gains an additive `doc_span: Option<Span>` (serde-default; the
analyzer schema version bumped so `incremental == full` still holds) — the
**span** is captured, never the text.

| Language | Captured form |
|---|---|
| TypeScript / JavaScript | a JSDoc/TSDoc block immediately preceding the declaration |
| Python | a docstring (the first string statement inside the `def`/`class` body) |
| C# | a `///` XML doc block |
| Rust | outer `///` line runs and `/** */` blocks immediately above a declaration |

**The corrected Rust bound.** Rust's grammar distinguishes an *outer* doc
comment (`///`, `/** */` — documents the FOLLOWING item) from an *inner* one
(`//!`, `/*! */` — documents the ENCLOSING scope: the module, file, or block it
sits inside) structurally, via the node's own `outer`/`inner` field. Inner
`//!` comments are **excluded from forward capture entirely** — not merely
deprioritized or "not yet handled." Attributing a `//!` to the next
declaration would mint a confidently-wrong Extracted 0.95 `Documents` edge to
the wrong symbol (`//! Module docs.` above `pub fn first_item()` documents the
*module*, not `first_item`); the never-confidently-wrong rule forbids that
approximation. A real module-doc attribution needs its own container-aware
target (the file's `Module` node, or the enclosing `mod`/`impl` block) and is
deferred, not faked. An intervening, gap-free run of attributes between a doc
comment and its declaration (`/// Doc\n#[derive(Debug)]\nstruct Foo;`) is
transparent — the comment still documents `Foo`, exactly as rustdoc treats it.

## Bodies-from-disk: the freshness rule

**No document body text lives in the graph, the store, or any shared
artifact — spans only.** `guidance` reads a section's body from the *working
tree* at query time (single-repo: `repo_root`; estate: the owning member's
root), sliced by the span the plane computed at index time. Two consequences
fall out of this one decision:

- **`guidance` is never staler than the file on disk.** Edit a doc section and
  `guidance` reflects the edit immediately, with no reindex required — only
  the *reference* (which section links to what, at what confidence) depends
  on the graph; the *content* it returns is always current.
- **Degradation is honest, never silent.** A body file that is absent or
  unreadable (a bare `.duckdb` opened without a resolvable repo root, or a
  file deleted since the last index) returns the ref with `body unavailable`
  as an explicit note — never blank text, never an error.

This is also why shared graph artifacts (an estate manifest, a future
graph-sync payload) carry **links, not prose**: the graph is safe to share
across repos or check into version control without leaking document contents
that live only on someone's disk.

## Serving surfaces and their budgets

Governing rule: **two-stage retrieval.** Cheap references first; bodies only
on demand; every surface capped. The engine never dumps a whole file at an
agent.

| Surface | Returns | Budget |
|---|---|---|
| `context()` → `docs` bucket | refs only: `{uid, name, anchor, path, provenance, confidence}` for every `Documents`/`Mentions` edge into the symbol | uncapped but cheap — a few tokens per entry, no body text |
| `guidance { symbol \| file, budget?, section? }` | a budgeted digest: description (contract targets) or doc comment first, then `Documents`, then `Mentions` by descending confidence; each section's body up to a per-section cap, with an honest `… [truncated — fetch <path>#<anchor>]` marker when cut, and a section past the total budget still appears as a `ref_only: true` entry (nothing goes invisible) | default **4800 chars** total (~1,200 tokens); **1200 chars** per section; `section` (an anchor) fetches that ONE section's full body, uncapped |
| `search_docs { query, limit? }` | tantivy BM25 hits: refs plus a highlighted snippet and the `matched_terms` that actually hit — never a body, never a summary | `limit` default **5**, hard-capped at **25** |
| pre-edit hook (`blast --format agent`) `docs:` line | the top 3 linked sections by confidence, plus a count | hard cap **200 bytes**; `docs: <name> §<anchor> (<conf>) · … · +N more — guidance <file> for detail`; absent entirely when the file has no doc links (silent-when-clean) |

See [MCP tools → guidance](../reference/mcp.md#guidance) and
[→ search_docs](../reference/mcp.md#search_docs) for the exact input/output
shapes, and [Pre-edit blast checks](../guides/pre-edit-blast.md) for the hook.

### The lexical index

`search_docs` reads a tantivy index at `.strata/docs.idx`, built at index time
over section bodies, doc-comment text, and spec descriptions. It is
**local-only** — never part of a shared or synced graph artifact — and it has
its own, distinct freshness note from `guidance`'s: a search hit reflects the
**last `strata index` run** (the post-edit reindex hook keeps this current in
an agent session), while `guidance`'s bodies are read from disk and are always
current regardless of when the index last ran. A missing or corrupt index
(never indexed yet, or corrupted) degrades to an honest
`{ results: [], note: "no docs index — run strata index" }` — never an error,
never a silent empty page pretending nothing exists.

## Docs enter the blast radius, both directions

`Documents` and `Mentions` are reverse-walked by `impact` exactly like any
other dependency edge, with two deliberate consequences:

- **Forward: a code change stales its docs.** `impact <symbol>` lists the
  `DocSection`s that document or mention it. Renderers show a doc-kind
  dependent with a **"needs review"** verdict instead of "WILL BREAK" — a
  stale doc does not fail to compile, it goes stale — but the underlying
  confidence/ambiguity mechanics are identical to every other edge kind.
  `detect_changes` (and the CLI's `blast`) surface the same signal as a
  dedicated **"docs to review"** line, refs only, so a code change's
  pre-commit summary names the documentation it puts at risk.
- **Reverse: a doc can already be lying.** A reference in a doc that resolves
  to nothing — a renamed symbol, a deleted file — produces **no edge** and
  increments `stale_doc_mentions`, surfaced in the `strata index` summary
  line and in coverage. One M1 bound to know: link destinations are resolved
  as **literal repo-relative paths**, so a valid doc-relative link (or one
  carrying a `#anchor`) also counts as stale — treat the counter as an upper
  bound on real drift, not a purified measure of it (the accuracy report at
  `docs/accuracy/knowledge-linking.md` in the repository states the bounds
  precisely). Together, the forward and reverse signals cover both
  directions of staleness deterministically: docs that a change is about to
  stale, and docs that are stale already.

The steering rule this trains into an agent kit (see
[Agent kit](../reference/agent-kit.md)) is conditional, never an unconditional
per-edit call: fetch `guidance` for a file only when the free pre-edit hook's
`docs:` line names a section at ≥ 0.80 confidence that has not been consulted
yet this session, and report `detect_changes`' "docs to review" line at commit
time, offering — never auto-applying — a fix for a stale section.

## Ingestion: what gets collected

The default collection set (`crates/strata-index`'s `is_collected_markdown`):
any `.md` under `docs/` (recursively), a root-level `*.md` (`README.md`,
`CONTRIBUTING.md`, …), or a nested `README.md` at any depth. `CHANGELOG*` is
excluded everywhere it appears — including under `docs/` — because entries
churn on every release and add no durable per-symbol signal; `.strata/` and
vendored dependency bundles are excluded the same way every other plane
excludes them. A repo with no matching markdown builds an empty knowledge
plane (additive), not an error.

Parsing itself (`strata-knowledge`, pure — no filesystem access in the core)
walks a markdown document with `pulldown-cmark` into sections keyed by
heading, extracting references from fenced code blocks, inline `` `code` ``
spans, and markdown links / path-shaped tokens. The indexer feeds it content;
the plane builder (`build_knowledge_plane`) resolves each extracted reference
against the same name/fqn lookup tables the code-plane linker already uses.

## Degradation notes

Every honest-miss path returns a value, never a panic or a silent drop:

- **No markdown in the repo** → the knowledge plane builds empty; `strata
  index`'s summary prints nothing extra (the `knowledge:` line only appears
  when at least one doc was ingested).
- **A reference resolves to nothing** → a `PathRef` or symbol-shaped
  `InlineCode` miss is counted in `stale_doc_mentions`; a plain-word/
  `SCREAMING_SNAKE_CASE`-shaped `InlineCode` miss is counted separately in
  `unresolved_plain_refs` (schema-invisible, not drift); a `FenceToken` miss
  is not counted anywhere. No edge is ever invented for any of the three.
- **A `guidance`/`context` body file is missing or unreadable** → the ref
  still returns, with an explicit `body unavailable` note.
- **No docs index yet, or a corrupted one** → `search_docs` returns `{
  results: [], note: "no docs index — run strata index" }`.
- **The docs index write itself fails** at `strata index` time (a permissions
  error, a full disk) → surfaced as a `[docs] WARNING …` line in the index
  summary, exactly like the infra/data planes' `[infra]`/`[data] FAILED`
  diagnostics — never silent.

## What is not built (M1 scope)

- **No semantic or embedding search.** Retrieval is structural (graph edges)
  and lexical (tantivy BM25) only. A future semantic tier is a separate,
  explicit decision, not an incremental extension of this one.
- **No generated summaries or wiki pages.** The engine serves material —
  refs, snippets, budgeted digests; writing prose from it is the agent's job.
- **No ingestion of agent steering files themselves** (`CLAUDE.md`,
  `.kiro/steering/*`) — editors already inject those directly.
- **No cross-repo doc linking beyond what estate linking already provides**
  for the code/contract/infra nodes a doc happens to reference.
