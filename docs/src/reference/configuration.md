# Configuration

The reference for StrataGraph's configuration surfaces: the workspace manifest, the
resolution modes, vendored pruning, and the on-disk `.strata/` layout. Grounded in
`crates/strata-index/src/estate.rs` (the manifest type), `resolve_mode.rs`, the
vendored-discovery code in `lib.rs`, and `stamp.rs`.

## The workspace manifest

A workspace manifest (`strata.workspace.toml`) lists a set of local repositories
that together form an **estate**. It is the input to `--workspace` on every
command that supports it (see [CLI](cli.md)). Parsed and validated by
`WorkspaceManifest::parse_file` / `parse_str`.

### Structure

```toml
# strata.workspace.toml

[workspace]
name = "my-estate"          # required, non-empty

[[repos]]
name = "frontend"           # required, non-empty, unique across the estate
path = "../frontend"        # relative to the directory containing this manifest

[[repos]]
name = "backend"
path = "../backend"

# Optional (manifest v2): declare the API identities this repo's specs belong to.
[[repos.apis]]
id = "orders"               # slug-safe: lowercase ascii alphanumeric + dash
spec = "service.proto"      # repo-relative path of the spec that defines this api
```

The types (`estate.rs`):

| Table / field | Type | Required | Meaning |
|---|---|---|---|
| `[workspace].name` | string | yes | The estate name. Used as each graph's `repo_name` qualifier and in canonical contract UIDs. Must be non-empty. |
| `[[repos]]` | array of tables | no (defaults empty) | The repos in the estate. |
| `[[repos]].name` | string | yes | The repo name. Must be non-empty and **unique** across the manifest. |
| `[[repos]].path` | string | yes | The repo root, **relative to the manifest's directory**. |
| `[[repos.apis]]` | array of tables | no (defaults empty) | Manifest **v2**: explicit API-identity declarations for this repo. |
| `[[repos.apis]].id` | string | yes (when the table is present) | The API id. Must be **slug-safe** (lowercase ascii alphanumeric + dash). |
| `[[repos.apis]].spec` | string | yes (when the table is present) | The repo-relative spec path that defines this API, matched against each operation's `spec_path`. |

> `[[repos.apis]]` declares only `id` and `spec`. There is **no** `format` or
> `key` field on the manifest: an operation's format (`openapi`/`graphql`/`grpc`)
> is inferred from the spec, and its `key` comes from the operation itself.

### Validation

`WorkspaceManifest::validate` enforces:

- `workspace.name` is non-empty.
- Each repo `name` is non-empty.
- Repo names are unique (a duplicate is `EstateError::DuplicateRepo`).
- Each `[[repos.apis]].id` is slug-safe.

A malformed manifest is `EstateError::Manifest`; an unreadable file is
`EstateError::Io`.

### v1 vs v2, and the API-merge behaviour

A **v1** manifest (no `[[repos.apis]]`) parses unchanged: `apis` defaults empty
and every operation's `api_id` defaults to the repo name. A **v2** manifest opts
in to explicit API identity:

- Declaring the **same `id` in two repos** merges a shared real API: its
  operations collapse to one canonical node across the estate (so an id is
  deliberately not required to be estate-unique).
- Declaring **several apis in one repo** lets one repo host multiple APIs.

The id composes into the canonical contract UID's `{api_id}/{format}`
discriminator (see [Schema → UID format](schema.md#uid-format)). Two unrelated
APIs that share an operation key string, even of the same format, stay on
distinct canonical nodes.

## Resolution modes

The `--resolve <MODE>` flag on [`strata index`](cli.md#index) selects the
precise-resolution mode (`ResolveMode`, parsed case-insensitively by
`ResolveMode::parse`). Precise resolution runs `scip-typescript` to upgrade
heuristic links to resolved ones.

| Mode | Behaviour |
|---|---|
| `auto` *(default)* | Run SCIP when prerequisites are present; **any** failure degrades to the heuristic, so indexing still succeeds. A structured diagnostic is printed on degrade. |
| `on` | SCIP is **required**; any failure is a hard error (propagated). |
| `off` | Never run SCIP: pure heuristic (not a degradation). |

### What SCIP resolution needs

Resolution is gated cheaply by `scip_runnable`: SCIP can run without a network
install when `typescript` is already present in `node_modules`. Network installs
are off by default (`allow_install` is false), so ordinary indexing never blocks
on the network: `auto` runs SCIP only when `typescript` is already installed.

- **`auto`** with TS/JS sources but no `typescript` installed → degrades silently
  (no error, no network probe).
- **`on`** with no TS/JS sources → an error.
- **`on`** with prerequisites missing (no `typescript`, installs disabled) → a
  hard error with an actionable message (no network probe).

The produced `index.scip` is cached keyed by a blake3 hash of the (sorted) TS/JS
source **content**, under `<temp-dir>/strata-scip-cache/<hash>.scip`, so an
unchanged source set skips re-running `scip-typescript`.

## Vendored pruning

By default StrataGraph prunes committed third-party dependency bundles so they do not
inflate the graph. The `--include-vendored` flag on `strata index` indexes them
anyway (`IndexOptions::include_vendored`, default false).

How a vendored bundle is detected (`discover_vendored_paths`,
`recorded_files` in `lib.rs`):

- The targeted anti-pattern is a committed `pip install -t .` bundle, recognised
  by its `*.dist-info` directory.
- For each `*.dist-info`, every path listed in its `RECORD` (the wheel install
  manifest) is resolved to the installed file under the dist-info's parent and
  pruned **only when it exists as a real file**. A same-named first-party file the
  `RECORD` does not list survives: the never-lose-first-party guarantee.
- The whole `*.dist-info` directory itself is pruned (by the collectors' name
  check).
- A missing/unreadable `RECORD`, or an out-of-tree entry (absolute `/…`, or `../`
  script), prunes nothing extra: only the dist-info dir. The conservative
  fallback risks inflation, never first-party loss.
- Only `*.dist-info` is a vendoring marker; `*.egg-info` is deliberately ignored
  (a first-party source tree legitimately ships its own `*.egg-info`).
- Vendored contract/infra specs inside a pruned bundle are excluded too, not just
  code.

The walk is gitignore- and `.strataignore`-aware and does not descend into the
name-based skip dirs (e.g. `.git`, `node_modules`, `site-packages`,
`__pycache__`). The `.strataignore` file (gitignore syntax) is honoured
regardless of `--include-vendored` and is the escape valve for the rare
legacy-egg or other special case.

## The `.strata/` layout

By default each indexed repo carries a `.strata/` directory at its root:

| Path | Written by | Purpose |
|---|---|---|
| `.strata/graph.duckdb` | `strata index` | The on-disk graph database (the `DEFAULT_DB` constant, `.strata/graph.duckdb`). The default for `--db`; the repo root is its **grandparent**. |
| `.strata/index.stamp` | `strata index` (last, after every persist) | The hot-reload change signal (`STAMP_FILE = "index.stamp"`), carrying the node/edge counts. Written last so a reader only learns of the new graph once it is fully persisted. |

The `index.stamp` is the signal the MCP server keys its hot-reload off; see
[MCP → Hot-reload](mcp.md#hot-reload). A reader that races a still-in-flight
reindex degrades safely (the stamp is written last; a stamp-write failure is a
non-fatal warning). For indexes written before the stamp existed, readers fall
back to the `graph.duckdb` mtime.
