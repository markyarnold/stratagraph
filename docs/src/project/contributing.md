# Contributing

StrataGraph is a Rust workspace with one binary (`strata`), a desktop app, and a hard honesty bar: the code never reports a dependency it cannot stand behind, and your contributions hold the same line. This page is the practical guide: building, running the gate, the test conventions, the honesty discipline, commit conventions, regenerating the accuracy corpora, and adding a doc page.

Read [Architecture](architecture.md) first for the crate map and the pipeline. Read [`docs/strata-design.md`](../../strata-design.md) for the deeper rationale.

## Building

You need a stable Rust toolchain (the workspace is edition 2021) with `clippy` and `rustfmt` components. Clone the repo and build the whole workspace:

```bash
cargo build --workspace
```

The `strata` binary lands at `target/debug/strata`. To build the release binary:

```bash
cargo build --release -p strata-cli
```

Most engine work needs nothing else. The hermetic test suite does **not** require Node, a .NET SDK, or rust-analyzer: the SCIP accuracy fixtures are committed and consumed as bytes. You only need those external tools to *regenerate* the corpora (see below) or to run a live SCIP-backed index.

### The desktop app

The desktop app is built separately and only when you touch it. Its UI (Vite + TypeScript) lives in `apps/strata-desktop/ui`; the Tauri CLI is driven from `apps/strata-desktop`. Install the UI dependencies there, then:

```bash
# from apps/strata-desktop/ui
npm run build      # tsc && vite build
npm run test       # vitest run
```

## Running the full gate

Every change passes the same gate before it merges. Run these unpiped; the exit codes are the contract:

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

Notes:

- **`-D warnings`** makes clippy treat every lint as an error. A warning is a failed gate.
- **`cargo fmt --check`** fails on any unformatted line; run `cargo fmt` to fix, then commit the result.
- **Desktop UI:** when (and only when) you touch `apps/strata-desktop`, also run the `npm run build` and `npm run test` above. An engine-only change does not need the UI toolchain.

If all of these pass, the mechanical bar is met. Correctness and honesty are on you; that is what the conventions below are for.

## Test conventions

StrataGraph is built test-first, and the tests encode the product's promises, not just its behaviour.

- **TDD, red first.** Write the failing test that pins the new behaviour before the implementation. The commit history is organised as red → green → gate slices; keep that shape.
- **Band guardrails.** Every heuristic confidence is a named, doc-commented constant capped to its provenance band (design §4.1). The band-invariant tests assert the **production** constants (they are re-exported from `strata-index` precisely so the guardrail can't drift from a stale local copy). If you add a heuristic, add its constant, document why the ceiling is what it is, and assert it.
- **Accuracy consistency and floors.** The per-language accuracy reports under `docs/accuracy/` carry measured numbers and CI floors. A consistency test asserts the committed report matches what the harness produces; a floor test fails if a change drops measured precision below the floor. Do not hand-edit the numbers in a report; regenerate them (below) and let the test confirm.
- **Never vacuous.** A test must be able to fail for the right reason. An accuracy or coverage assertion over an empty set is not a test: the harness and gates are written so a zero-work result is a failure, not a pass. When you add a corpus or a coverage check, assert it actually measured something.
- **Hermetic.** The normal suite runs with no network and no external language servers. Anything that needs Node or rust-analyzer is `#[ignore]`d and excluded from the gate.

## The honesty discipline

This is the part that makes StrataGraph what it is. Hold it without exception.

- **Never invent.** A link the analyzer cannot make is **counted and surfaced** (an unresolved tally, a diagnostic), never fabricated. A table, column, resource, or operation the parsed input never declared is never created. A malformed file produces a visible diagnostic and is skipped; it never crashes the index and never silently disappears.
- **Confidence bands are law.** Provenance dictates the ceiling: a `RESOLVED` compiler fact outranks an `EXTRACTED` literal, which outranks an `INFERRED` interpolation, which outranks an `AMBIGUOUS` guess. Every edge's confidence is `min(measured, band ceiling)`. You may never emit a confidence above what the evidence earns, and the monotonicity invariant between bands is asserted.
- **Never confident-wrong.** Recall-biased is fine: StrataGraph would rather show a dependency that turns out safe than hide one. Confident-wrong is not: an uncertain result must be marked `Ambiguous` (or fall below the will-break bar) and presented as uncertain. The will-break label is governed by a measured threshold (the `INFERRED` floor, `DEFAULT_WILL_BREAK_CONFIDENCE`), not a guess.
- **Say "unknown" out loud.** When the graph cannot resolve something, the output says so. Dead contract surface (zero producers and zero consumers) is flagged as probably dead rather than treated as live. See [Confidence and provenance](../concepts/confidence.md) and [Honest limitations](../accuracy/limitations.md).

A change that improves a number by relaxing this discipline will be rejected even if every mechanical gate passes.

## Commit and branch conventions

- **Work on a branch.** The default branch is `develop`; do not commit directly to it. Branch with a short, scoped name (e.g. `feat/orm-linking`, `fix/sql-stmt-split`, `feat/docs`).
- **Conventional-style messages.** Commit subjects follow `type(scope): summary`, for example `feat(data): link ORM models to declared tables`, `fix(orm): capture annotated __tablename__`, `feat(core): OrmModelHint signal + EdgeKind MapsTo`. Bump the analyzer schema version in the same commit when you change `AnalyzedFile`'s shape.
- **Small, gated slices.** A slice is red → green → gate, then a commit (or a couple). Run the full gate before each commit, not just at the end.
- **The graph is a contributor, too.** Before changing a symbol, run `impact` (or `detect_changes` before committing) and act on the blast radius it reports: pause on HIGH/CRITICAL, and never rename with find-and-replace. See [Pre-commit change checks](../guides/detect-changes.md) and [Rename a symbol safely](../guides/rename.md).

## Regenerating the accuracy corpora

The differential accuracy harness compares StrataGraph's Tree-sitter heuristics against a precise SCIP index treated as ground truth. Those `index.scip` files are **committed fixtures**; the gate consumes them hermetically. You only regenerate them when you change a corpus project or the extraction that the corpus measures.

The regenerators are the `#[ignore]`d tests in `crates/strata-index/tests/gen_scip.rs`. They need the matching indexer on your PATH and a network:

- **TypeScript** corpora use `scip-typescript` (the runner in `strata-scip` invokes it; Node required).
- **Python** corpora use `@sourcegraph/scip-python` via `npx` (Node required).
- **Rust** corpora use `rust-analyzer scip` (`rustup component add rust-analyzer`).

Run them explicitly with the live env var; they are excluded from the normal suite:

```bash
# all of them
STRATA_SCIP_LIVE=1 cargo test -p strata-index --test gen_scip -- --ignored

# one language / one project
STRATA_SCIP_LIVE=1 cargo test -p strata-index --test gen_scip -- --ignored generate_py
STRATA_SCIP_LIVE=1 cargo test -p strata-index --test gen_scip -- --ignored generate_rust_shapes_index
```

After regenerating, re-derive the report numbers and floors, then run the normal gate: the consistency and floor tests confirm the committed `.md`/`.json` reports match the freshly generated fixtures. The live coverage printers for the other planes are also `#[ignore]`d (run with `-- --ignored --nocapture` to print live numbers). See [How accuracy is measured](../accuracy/methodology.md) for the full methodology.

## Adding a doc page (this mdBook)

These docs are an [mdBook](https://rust-lang.github.io/mdBook/) rooted at `docs/` (`docs/book.toml`), with sources under `docs/src/`. To add a page:

1. Create the Markdown file under the right section directory (e.g. `docs/src/guides/my-page.md`).
2. Add it to `docs/src/SUMMARY.md` under the correct heading: `SUMMARY.md` is the table of contents and the build is configured with `create-missing = false`, so a page that is not listed there is not built.
3. Follow the house style: one H1 per page (ATX `#` headings throughout), fenced code blocks with a language tag, tables for structured facts, a mermaid block for a diagram where it helps, and **relative links** between pages. Use real, runnable commands only.
4. Preview locally with `mdbook serve docs` (or `mdbook build docs`) if you have `mdbook` installed; the rendered book lands in `docs/book/` (gitignored).

Keep the honesty bar in the docs too: describe behaviour the code actually has, mark anything not yet shipped as a [roadmap](roadmap.md) item, and cross-link to the [concepts](../concepts/graph.md), [reference](../reference/cli.md), and [accuracy](../accuracy/methodology.md) pages where they carry the detail.
