# Knowledge plane (the intelligent repo) — design

- **Date:** 2026-07-13
- **Status:** Approved design; implementation plan to follow.
- **Owner decisions locked during brainstorming:**
  1. Strategy sequencing: the knowledge plane ships **before** the shared-graph
     service; a companion [graph-sync protocol note](2026-07-13-graph-sync-protocol-note.md)
     keeps the two compatible.
  2. Retrieval is **structural + lexical, zero ML**: deterministic graph edges
     plus a full-text index. No embeddings, no models, no API keys. Any future
     semantic tier is a separate, explicit decision.
  3. M1 sources: **markdown files, inline doc comments, spec descriptions**.
     Agent steering files are deferred to M2.
  4. Architecture: **fifth plane, bodies-from-disk** (approach A) — the graph
     stores structure and spans, never document body text.
  5. **Token economy is a hard requirement**, not a nicety: every serving
     surface has an explicit budget, enforced by test.

## 1. Goal

Make the repository's own knowledge — READMEs, ADRs, manuals, doc comments,
API descriptions — first-class citizens of the StrataGraph graph, so that:

- an agent about to touch a symbol is told *what the repo already knows* about
  it ("this file is covered by ADR-12 §outbox: never bypass the outbox"),
- a change's blast radius includes the **documentation it stales**, and
- the graph reports which docs are **already lying** (references to symbols
  that no longer exist).

All of it deterministic, banded, and honest — the same contract as every other
plane. "Never confidently wrong" applies to what the repo says about itself.

### Non-goals (M1)

- No semantic/embedding search (decision 2). No generated summaries or wiki —
  generation is the agent's job; the engine serves material.
- No ingestion of agent steering files (deferred; editors already inject them).
- No cross-repo doc linking beyond what estate linking already provides for
  the nodes docs point at.
- No storage of document body text in the graph or in shared artifacts.

## 2. Model and vocabulary

New UID namespace **`doc`**, alongside `ts`/`py`/…/`contract`/`infra`/`data`.

### Nodes (additive; the `Node` struct is unchanged)

| Kind | One per | `name` | `fqn` | `path` / `span` |
|---|---|---|---|---|
| `Doc` | ingested markdown file | filename | repo-relative path | the file / whole file |
| `DocSection` | heading section | heading text | `<path>#<anchor>` | the file / heading to next same-or-higher heading |
| `DocSection` (doc comment) | documented symbol | first line of the comment, truncated | `<source-path>#doc:<symbol-fqn>` | the source file / the comment block |

Anchors are GitHub-style slugs; duplicate anchors within a file take the
GitHub `-1`, `-2` suffixes so section UIDs are **stable across unrelated
edits** (a graph-sync obligation).

`Doc —Contains→ DocSection`. `Contains` is never traversed by `impact`,
consistent with the existing ApiId rule.

Spec descriptions create **no nodes**: the `ApiOperation`/`GraphqlField` node
is itself the documented thing. `OperationDef` gains an additive
`description: Option<String>`; the text is indexed for search and served live
by `guidance`.

### Edges

| Kind | From → to | When | Provenance / confidence |
|---|---|---|---|
| `Documents` | DocSection → symbol | a doc comment adjacent to its symbol (syntactic fact) | Extracted **0.95** |
| `Mentions` | DocSection → any node | exact repo-relative path reference | Extracted **0.95** |
| `Mentions` | DocSection → any node | unique fqn in a code fence or inline `code` | Inferred **0.80** |
| `Mentions` | DocSection → any node | unique bare name in inline `code` | Inferred **0.70** |
| `Mentions` | DocSection → several candidates | multi-candidate name | Ambiguous **0.35** fan-out, one edge per candidate |
| *(no edge)* | — | unresolvable reference | counted as `stale_doc_mentions` |

Markdown **never** earns `Documents` in M1: prose about a symbol is a mention,
not proof of documentation. Only syntactic adjacency (doc comments) reaches
the Extracted `Documents` tier.

Bands are law: the `confidence_bands` guardrail suite is extended to cover
both new edge kinds non-vacuously.

### Impact semantics — docs enter the blast radius

`Documents` and `Mentions` are reverse-walked by `impact` like any other edge.
Consequences, both deliberate:

- `impact <symbol>` lists affected DocSections. Renderers show doc nodes with
  a **"needs review"** verdict wording instead of "WILL BREAK" (a doc does not
  break; it goes stale). The confidence/ambiguity mechanics are unchanged.
- `detect_changes` gains a **"docs to review"** line (refs only), so a code
  change surfaces the documentation it stales at pre-commit time.

### Doc drift — the mirror image

A reference in a doc that resolves to nothing (renamed symbol, deleted file)
produces **no edge** and increments `stale_doc_mentions`, surfaced in the
index summary and coverage. The repo reports which docs are lying. Together
with impact-on-docs this covers both directions of staleness,
deterministically.

## 3. Ingestion

### `strata-knowledge` crate (new; mirrors `strata-data`'s shape)

Pure, heavily tested core: markdown parsing via **pulldown-cmark** into
`DocModel { sections: [ { heading, anchor, span, refs } ] }`. Ref extraction
from:

- fenced code blocks (identifier tokens),
- inline `code` spans (fqn or bare-name candidates),
- markdown links and path-shaped tokens (repo-relative file references).

No filesystem access in the core; the indexer feeds it content.

### Collection (indexer)

- Default set: `docs/**\/*.md`, root-level `*.md`, nested `README.md`.
- Default excludes: `CHANGELOG*` (noise; entries churn), `node_modules`/vendored
  trees (existing pruning applies), `.strata/`.
- Estate mode: per-repo collection, exactly like the other planes.

### Doc comments (analyzers)

`RawSymbol` gains `doc_span: Option<Span>` (serde-default; analyzer schema
bump; the incremental==full invariant holds). Per-language capture:

- Rust: outer `///` and `//!` blocks.
- TS/JS: JSDoc/TSDoc block immediately preceding the declaration.
- Python: docstring (first string statement inside the def/class body).
- C#: `///` XML doc block.

The **span** is captured, never the text — bodies stay on disk.

### Plane builder

`build_knowledge_plane` resolves refs against the same name/fqn lookup tables
the consumer linker uses, emits nodes/edges per the table above, and reports:

```
knowledge: 34 docs, 210 sections; 480 mentions linked (12 ambiguous), 9 stale; 1,102 doc comments
```

## 4. Serving surfaces — token budgets are part of the contract

Governing rule: **two-stage retrieval**. Cheap references first; bodies only
on demand; everything capped. The engine never dumps a file at an agent.

| Surface | Returns | Budget |
|---|---|---|
| `context()` → new `docs` bucket | refs only: `{uid, name, anchor, provenance, confidence}` | a few tokens per entry |
| `guidance { symbol \| file, budget?, section? }` | a **budgeted digest**: doc comment first, then `Documents`, then `Mentions` by descending confidence; each section's first lines up to a per-section cap, with an honest `… [truncated — fetch <path>#<anchor>]` marker | default **~1,200 tokens** total; `section` fetches one full section |
| `search_docs { query, limit }` | tantivy BM25 hits: refs + a line or two of **term-match highlights**, never bodies; every hit names the matched terms | `limit` default 5 |
| pre-edit hook (blast) | one line: top 3 sections by confidence + count | hard character cap; `docs: ADR-12 §outbox (0.95) · retries.md §policy (0.80) · +3 more — guidance <file> for detail` |

- **Bodies-from-disk:** `guidance` reads section bodies from the working tree
  at query time (single-repo: `repo_root`; estate: the owning member's root).
  Guidance is therefore never staler than the file, and shared graph
  artifacts carry **links, not prose** (see the protocol note).
- **Degradation:** body file absent (bare `.duckdb` open, deleted file) →
  refs returned with `body unavailable`, never silence, never an error.
- **Lexical index:** tantivy at `.strata/docs.idx`, built at index time over
  section bodies, doc-comment text, and spec descriptions. Local-only —
  never part of shared artifacts. Honest freshness note: search hits reflect
  the **last index** (the post-edit reindex hook keeps that current in agent
  sessions), while `guidance` bodies are read from disk and are always
  current.
- **Enforcement:** a guardrail test asserts the default `guidance` digest
  stays under budget against a deliberately fat fixture, alongside the band
  guardrails.

## 5. Agent kit changes (Claude Code + Kiro)

The plane only matters if the kits teach agents to use it. Governing principle,
per the token-economy requirement: **the kits add no unconditional per-edit
tool calls.** Awareness is pushed free (one line in a hook that already
fires); detail is pulled only when the agent chooses.

### Steering (both kits — CLAUDE.md/AGENTS.md and `.kiro/steering/strata.md`)

- **Tool list** gains `guidance` and `search_docs` with one-line usage rules:
  `search_docs` replaces manual doc-grepping for "how do we…?" questions;
  `guidance` before acting on a symbol *when coverage exists* (see below).
- **New conditional MUST:** "MUST act on the `docs:` line the pre-edit blast
  injects: when it lists a section at ≥ 0.80 covering the file and you have
  not consulted it this session, fetch `guidance` for the file before
  editing." Conditional on the free hook line — never a blanket call.
- **Honesty rule extended:** doc guidance is *repo knowledge, not ground
  truth* — docs can be stale; the graph marks drift (`stale_doc_mentions`)
  and a mention below 0.40/ambiguous is UNKNOWN, same trust policy as
  every other band.
- **Commit rule extended:** `detect_changes`' "docs to review" line must be
  reported in the pre-commit summary; offer (never auto-apply) doc updates
  for stale sections.
- The generic `AGENTS.md` carries the same block, so Cursor/other
  AGENTS.md-reading assistants inherit the behaviour without a bespoke kit.

### Claude Code specifics

- **Pre-edit hook:** no settings change — the payload is `blast --format
  agent`, which now includes the capped `docs:` line (engine-side, §4).
- **Skills:** extend `strata-guide` (tool table + routing row: repo
  conventions / "is there guidance on X?" → `search_docs`/`guidance`) and
  `strata-exploring` (docs bucket in `context`, guidance-first exploration of
  unfamiliar areas). **No fifth skill** — more skills means more listing
  tokens in every session; the content fits the existing two.

### Kiro specifics

- Kiro reads steering, not skills: the steering additions above carry the
  whole behaviour. The pre-edit askAgent prompt gains one clause — consult
  the blast `docs:` line / fetch `guidance` when covered — kept within the
  prompt's existing length discipline.
- The post-edit reindex hook is unchanged in shape; note that `strata index .`
  now also refreshes `.strata/docs.idx`, which keeps `search_docs` current in
  exactly the sessions that edit files.

### Kit token audit (the additions, in full)

| Surface | Added cost |
|---|---|
| Pre-edit hook payload | ≤ 1 line (hard char cap, §4) |
| Steering block | ~8 lines once per session context |
| Skills | edits to existing skills; no new skill |
| Per-edit tool calls | **zero unconditional**; `guidance` only when the free hook line shows unconsulted ≥ 0.80 coverage |

## 6. Testing and accuracy

- Red-first fixtures per source: markdown ref shapes (fence/inline/path/
  ambiguous/stale), each language's doc-comment forms, OpenAPI + GraphQL
  descriptions.
- `confidence_bands` extended: `Documents`/`Mentions` in-band, non-vacuous.
- Drift fixture: a mention of a deleted symbol yields no edge + a counted
  stale ref (never an invented edge).
- Token-budget guardrail (above).
- Impact/detect_changes rendering tests: doc nodes carry "needs review"
  wording; "docs to review" line lists refs only.
- Dogfood: the strata repo itself (rich `docs/` tree), then the usual
  real-repo verification pass. Published report:
  `docs/accuracy/knowledge-linking.md` with measured counts.

## 7. Slices

Each through the gated implement → independent review → fix → merge cycle:

- **K1** — `strata-knowledge` crate: markdown model + ref extraction (pure).
- **K2** — plane builder: Doc/DocSection nodes, `Mentions` edges, coverage +
  drift counts; impact traversal + "needs review" rendering.
- **K3** — doc comments: `doc_span` across the four analyzers (schema bump) +
  `Documents` edges.
- **K4** — spec descriptions: `OperationDef.description` (OpenAPI + GraphQL),
  indexed + served.
- **K5** — tantivy index + `search_docs`.
- **K6** — `guidance` + `context` docs bucket + pre-edit hook line +
  `detect_changes` "docs to review" + the budget guardrail.
- **K7** — agent kits per §5 (steering rules + tool list in both kits, the
  Kiro pre-edit prompt clause, `strata-guide`/`strata-exploring` skill
  extensions, kit token audit verified against the table), then manual docs,
  website mirror, changelog.

## 8. Graph-sync compatibility (obligations pinned here)

Per the [protocol note](2026-07-13-graph-sync-protocol-note.md): section UIDs
are anchor-stable; artifacts never contain body text (true by construction);
the tantivy index is always built locally and never synced.

## 9. Open questions (deliberately small)

- Default `guidance` budget value: start at ~1,200 tokens and tune against
  dogfood transcripts.
- Whether nested `README.md` collection needs an opt-out for monorepos with
  hundreds of packages (decide with real numbers at K2 dogfood).
