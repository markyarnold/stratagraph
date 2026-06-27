# Index your first repository

Before StrataGraph can answer anything, it has to build the graph. That is one
command: `strata index`. This page walks through what it does, where it writes,
how to read the summary it prints, and what it leaves out.

## Run it

Point `strata index` at a repository root:

```bash
strata index /path/to/your/repo
```

or, from inside the repo:

```bash
strata index .
```

StrataGraph walks the repository, parses every supported source file, contract spec
(GraphQL / OpenAPI / gRPC), infrastructure template (CloudFormation / SAM /
Terraform / Terragrunt), and SQL schema it finds, builds the cross-plane graph,
and persists it.

## What it writes

The graph is stored in a single file under a `.strata/` directory at the repo
root:

```text
<repo>/.strata/graph.duckdb     the graph (a DuckDB database)
<repo>/.strata/index.stamp      a tiny "this index changed" marker
```

`graph.duckdb` is the whole graph: nodes, edges, provenance, confidence. Every
read command (`query`, `context`, `impact`, `explain`) opens this file; the
default location is `.strata/graph.duckdb` relative to the current directory, so
once you have indexed a repo you can run the read commands from its root with no
`--db` flag.

`index.stamp` is written **last**, only after the graph is fully persisted. It is
how the long-running MCP server notices a fresh index and hot-reloads it without a
restart (see [Wire up your editor](agent-kit.md)). You never edit it.

To put the database somewhere else, pass `--db`:

```bash
strata index . --db /tmp/myrepo.duckdb
```

## Read the summary

`strata index` prints a per-plane summary. On a code-only repository it is short:

```text
Indexed .
  engine:        405a1ba2dedd-dirty
  files indexed: 312
  files parsed:  41
  files reused:  271
  nodes:         2751
  edges:         22699
```

Reading it:

- **engine**: the engine id that produced this graph (the same id
  `strata --version` prints). Surfaced here so a graph built by a stale binary is
  visible.
- **files indexed**: total source files that went into the graph.
- **files parsed**: how many were parsed fresh this run.
- **files reused**: how many were served from the incremental cache, unchanged
  since the last index (see [Incremental re-index](#incremental-re-index)).
- **nodes / edges**: the size of the resulting graph.

When the repository contains contract, infrastructure, or data surface, extra
per-plane lines appear. They are **additive**: a plane that is not present prints
nothing, so a plain code repo never shows empty infra/data noise. For example, a
repo with CloudFormation and a SQL schema also prints lines like:

```text
  infra:         3 template(s), 0 failed, 41 resource(s); 6/7 resolvers linked, 4 Lambda(s) → handler
  data:          2 schema(s), 0 failed, 18 table(s), 134 column(s); 11/12 foreign keys linked
  data:          9/10 reads, 5/5 writes, 3/3 ORM model(s) linked to tables
```

Two diagnostics to watch for:

- **`failed`**: a template or schema file that could not be parsed. Each failure
  is also printed on its own `[infra] FAILED …` / `[data] FAILED …` line *before*
  the summary, so a real template can never be silently skipped. A non-zero
  `failed` means some surface is missing from the graph; the line tells you which
  file.
- **`unparseable statement(s) skipped`** (data plane): individual SQL statements
  inside a file that *did* parse but that StrataGraph's SQL parser could not read (for
  example a PL/pgSQL function body). This is informational, not a failure: the
  tables around the skipped statement were still extracted. The line just makes
  the skip visible.

The `linked/total` counts (resolvers linked, foreign keys linked, reads/writes,
ORM models) are deliberately honest accounting: when a link could not be resolved,
the unresolved count is visible rather than hidden. StrataGraph never invents a link to
make the numbers look complete.

## Incremental re-index

Re-running `strata index .` after editing a few files is cheap. StrataGraph caches
per-file analysis keyed on content, so files that have not changed are reused
rather than re-parsed; that is the `files reused` line. A second index of an
unchanged repo reuses everything; after editing one file, only that file (and
what its analysis touches) is re-parsed.

You can re-index as often as you like; it is the normal way to keep the graph
current. (The agent kit automates this: a PostToolUse hook re-indexes after each
edit. See [Wire up your editor](agent-kit.md).)

## What gets pruned

The walk is `.gitignore`-aware: anything your `.gitignore` excludes is excluded
from the index too (this applies even before the directory is a git checkout). On
top of that:

- **`.git/`** is always skipped.
- **Dependency / build / cache roots** are excluded: `node_modules/`, Python
  `venv/` / `.venv/` / `__pycache__/` / `site-packages/`, the Rust `target/`
  directory, and similar. These hold third-party or generated code, not your
  first-party source.
- **Vendored dependency bundles** (a committed `pip install -t .` bundle,
  detected by its `*.dist-info` metadata) are pruned by file so they do not
  inflate the graph. A first-party file that happens to share a name with a
  vendored one is still reached and indexed.
- **`.strataignore`**: an optional file you add (same syntax as `.gitignore`)
  for extra excludes under your control.

If you genuinely want a committed third-party bundle in the graph, re-run with
`--include-vendored`.

## Next

Now ask the graph something: [Your first queries](first-queries.md).
