# Rename a symbol safely

**Goal:** rename a function, method, class, or interface across the whole codebase without corrupting cross-file or cross-plane references, and without touching unrelated identifiers that merely share the name.

`strata rename` is graph-aware: it edits only files the graph actually implicates, tags every edit with a confidence, and is a dry run by default so you see the plan before anything is written.

## Why not grep-and-replace?

A `sed -i 's/oldName/newName/g'` (or your editor's "replace in all files") matches text. It will happily rewrite a comment, an unrelated local variable, a string literal, and a same-named symbol in a completely different module. It will *also* miss anything the graph knows is connected but the text doesn't obviously show, and it has no idea a name crosses a plane boundary. That's exactly the failure the steering rule forbids:

> NEVER rename a symbol with find-and-replace. Run `impact` first, then update exactly the dependent set the graph reports; grep-and-replace silently corrupts cross-file and cross-plane references.

`strata rename` is the tool that replaces that footgun.

## Steps

### 1. Dry-run the rename

Just name the old symbol and the new identifier. Without `--apply`, nothing is written:

```console
$ strata rename classify_risk assess_risk
Rename (dry run): classify_risk → assess_risk
  18 implicated file(s): crates/strata-cli/src/init/mod.rs, crates/strata-cli/src/reload.rs, ..., crates/strata-index/src/changes.rs, ...
  10 edit(s):
    conf   line:col  file
    0.95   289:15  crates/strata-index/src/changes.rs
    0.95   700:3  crates/strata-index/src/changes.rs
    0.95   928:15  crates/strata-index/src/changes.rs
    ...
    0.95   1439:23  crates/strata-index/src/changes.rs
  (dry run — re-run with --apply to write these edits)
```

### 2. Read the plan

Two distinct things are reported, and the difference matters:

- **Implicated files**: the candidate scope, namely the symbol's definition file plus every file owning a node connected to it by a `Calls`/`Imports` edge, in either direction. These are the *only* files `rename` will ever look in. A same-named identifier in a file the graph does **not** implicate is never touched: that's the core safety guarantee.
- **Edits**: the actual identifier tokens equal to the old name, found by re-parsing each implicated file with the same grammar the indexer used. Each edit carries a **confidence**: the definition site is a fact (`0.95`), and a caller-site edit inherits the confidence of the edge that implicated its file.

In the example above, 18 files are in scope but the 10 real edits all land in `changes.rs` (where `classify_risk` is defined and called). The wide implicated list is the neighborhood StrataGraph *checked*; the edit list is what it will actually change.

### 3. Apply

When the plan looks right, write it:

```console
$ strata rename classify_risk assess_risk --apply
```

Each file is written atomically (temp file + rename), and `rename` recommends a reindex afterward so the graph reflects the new names. (With the agent kit installed, the post-edit hook reindexes for you.)

## Reading the confidence column

Apply the band policy to each edit, the same as anywhere in StrataGraph:

- **0.95 (definition site)**: a fact; apply it.
- **0.40–0.89 (caller site)**: the edit inherits an inferred edge's confidence; glance at the line before trusting it.
- **anything lower**: review explicitly.

This matters because of an **honest bound**: token matching is lexical *within* an implicated file. If an implicated file happens to contain a local variable also named `classify_risk`, that token would be collected too, tagged with that file's edge confidence, so a `< 0.9` edit is your cue to look. What `rename` guarantees today is **scope** (only graph-implicated files) plus **confidence tagging**, not per-token semantic precision. Per-occurrence resolution (SCIP/pyright/Roslyn) tightens this later; for now, scan edits below 0.9 before `--apply`.

## When the name is ambiguous

If the old name resolves to several code nodes, `rename` refuses to guess and lists them:

```console
$ strata rename reload refresh
ambiguous symbol reload: 4 code candidates — re-run with --uid <uid>:
  - reload [Method] (crates/strata-cli/src/reload.rs)
    rust|strata|crates/strata-cli/src/reload.rs|SingleDbReloader::reload|
  - reload [Method] (crates/strata-cli/src/reload.rs)
    rust|strata|crates/strata-cli/src/reload.rs|WorkspaceReloader::reload|
  - reload [Method] (crates/strata-mcp/src/server.rs)
    rust|strata|crates/strata-mcp/src/server.rs|GraphReloader::reload|
  - reload [Method] (crates/strata-mcp/src/server.rs)
    rust|strata|crates/strata-mcp/src/server.rs|tests::ScriptedReloader::reload|
```

Pin the exact one with its uid:

```console
$ strata rename reload refresh --uid 'rust|strata|crates/strata-cli/src/reload.rs|SingleDbReloader::reload|'
```

## When the new name already exists

If a symbol is already named `<new>` anywhere in the repo, `rename` refuses, because renaming into an existing name is how you create silent collisions:

```console
$ strata rename cmd_impact cmd_context
error: rename would collide: 1 existing symbol(s) already named `cmd_context` — re-run with force to proceed anyway:
  - cmd_context (crates/strata-cli/src/lib.rs)
```

If you genuinely intend it (e.g. you're merging two functions), re-run with `--force`. Otherwise pick a different name.

## Code symbols only

`rename` is for the code plane: Function, Method, Class, Interface. Point it at a contract field or infra resource and it tells you so rather than half-renaming a schema:

```console
$ strata rename users accounts
error: rename supports code symbols (Function/Method/Class/Interface); `users` is a Table — contract/infra rename is queued
```

To rename a GraphQL field, an API operation, or a table, you're editing the contract/schema directly: run [`impact` / `context`](dead-surface.md) on it first to find every producer and consumer, then change the declaration and the implicated code by hand.

## What to do next

- Always dry-run first; read the implicated-files scope and the per-edit confidence.
- Scan edits below 0.9 before `--apply`.
- After applying, reindex (`strata index .`), or let the agent kit's post-edit hook do it.
- Want the blast radius before you rename, not just the edit set? Run [`impact`](impact.md) on the symbol first. The full flag list is in the [CLI reference](../reference/cli.md).
