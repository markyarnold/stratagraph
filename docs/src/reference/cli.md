# CLI

The complete reference for the `strata` command-line interface: every
subcommand, every flag, its default, and what it does. Grounded in the clap
definitions in `crates/strata-cli/src/main.rs` and each command's `--help`
output.

For task-oriented walkthroughs, see the [Guides](../guides/impact.md); this page
states the surface exactly.

## Synopsis

```
strata <COMMAND>
```

Top-level options:

| Flag | Meaning |
|---|---|
| `-h`, `--help` | Print help. |
| `-V`, `--version` | Print the version: the package version plus the compiled engine id, e.g. `0.1.0 (abc123def456)`. |

## Commands

| Command | Purpose |
|---|---|
| [`index`](#index) | Build or refresh the code graph for a repository (or a workspace estate). |
| [`impact`](#impact) | Show the reverse blast radius (dependents) of a symbol. |
| [`explain`](#explain) | Explain *why* one symbol is in another's blast radius (the evidence chain). |
| [`context`](#context) | Show the 360° context of a symbol. |
| [`query`](#query) | Lexical search over node name, fully-qualified name, and path. |
| [`mcp`](#mcp) | Serve the code graph to an MCP client over stdio. |
| [`detect-changes`](#detect-changes) | Report the changed symbols, blast radius, and risk vs HEAD. |
| [`blast`](#blast) | Report the pre-edit blast radius of a *file*. |
| [`rename`](#rename) | Graph-aware multi-file rename of a code symbol. |
| [`init`](#init) | Install an agent-integration kit. See [Agent kit](agent-kit.md). |
| `help` | Print help for `strata` or a subcommand. |

### Common options

Several commands share these options. Where a command supports them they are
listed in its table below.

- `--db <PATH>`: Graph database path. Default: `.strata/graph.duckdb` (the
  constant `DEFAULT_DB`, relative to the current directory). On the query
  commands this is **mutually exclusive** with `--workspace`.
- `--workspace <MANIFEST>`: Path to a workspace manifest
  (`strata.workspace.toml`). Runs the command over the linked estate graph.
  **Mutually exclusive** with `--db`. See [Configuration](configuration.md).
- `--repo <PATH>`: Repository root for the filesystem-touching commands.
  Default: the **grandparent** of `--db` when the DB path ends in
  `.strata/graph.duckdb`, else (for `detect-changes`/`rename`) the current
  directory.

## index

Build or refresh the code graph for a repository (or a workspace estate).

```
strata index [OPTIONS] [PATH]
```

| Argument / flag | Default | Description |
|---|---|---|
| `[PATH]` |  | Path to the repository root to index. **Not required when `--workspace` is provided** (it errors with exit 1 if both `PATH` and `--workspace` are absent). |
| `--db <PATH>` | `.strata/graph.duckdb` | Graph database path. Mutually exclusive with `--workspace`. |
| `--workspace <MANIFEST>` |  | Path to a workspace manifest. Indexes all repos in the estate. Mutually exclusive with `--db`. |
| `--resolve <MODE>` | `auto` | Resolution mode: `auto`, `on`, or `off` (case-insensitive). See [resolve modes](configuration.md#resolution-modes). An unparseable value falls back to `auto`. |
| `--include-vendored` | off | Index committed third-party dependency bundles instead of pruning them. By default a vendored `pip install -t .` bundle (detected via its `*.dist-info`) is excluded so it does not inflate the graph. See [vendored pruning](configuration.md#vendored-pruning). |

After a successful index, the engine writes a hot-reload stamp at
`.strata/index.stamp`; see [Configuration → .strata layout](configuration.md#the-strata-layout).

## impact

Show the reverse blast radius (dependents) of a symbol: everything that depends
on it.

```
strata impact [OPTIONS] <SYMBOL>
```

| Argument / flag | Default | Description |
|---|---|---|
| `<SYMBOL>` |  | Symbol to analyse (fully-qualified name preferred, else name). |
| `--uid <UID>` |  | Pin one candidate when `<SYMBOL>` resolves to several nodes. An ambiguous symbol lists its candidates' uids; re-run with one here. |
| `--db <PATH>` | `.strata/graph.duckdb` | Graph database path. Mutually exclusive with `--workspace`. |
| `--workspace <MANIFEST>` |  | Run impact over the estate graph. Mutually exclusive with `--db`. |
| `--depth <DEPTH>` | `5` | Maximum reverse-traversal depth. |
| `--min-confidence <MIN_CONFIDENCE>` | `0` | Drop paths below this accumulated confidence. |
| `--no-contracts` | off | Code-only blast radius: do **not** follow the contract plane (producer → operation → consumer). Contracts are **included by default**, so cross-plane/cross-repo consumers of a producer are surfaced. |
| `--no-infra` | off | Do **not** follow the infra plane (`Assumes`/`Routes`/`Runs`). Infra is **included by default**, so an `IamRole` reaches the Lambdas that assume it (and their reach), and a handler module reaches its Lambda. |

The CLI surfaces ambiguity as a candidate listing (re-run with `--uid`). When a
member-bearing target's own blast radius is empty but its members have
dependents, the output reports those members rather than a misleading "nothing
depends on this".

## explain

Explain *why* one symbol is in another's blast radius: the evidence chain, with
each edge's kind, provenance, and confidence, plus the running confidence that
produces `impact`'s number.

```
strata explain [OPTIONS] <TARGET> <AFFECTED>
```

| Argument / flag | Default | Description |
|---|---|---|
| `<TARGET>` |  | The changed symbol (the impact target; fqn preferred, else name). |
| `<AFFECTED>` |  | The affected symbol whose presence in the blast radius to explain. |
| `--uid <UID>` |  | Pin the **TARGET** when it resolves to several nodes. |
| `--affected-uid <UID>` |  | Pin the **AFFECTED** end when it resolves to several nodes (mirrors the MCP `explain` tool's `affected_uid`). |
| `--db <PATH>` | `.strata/graph.duckdb` | Graph database path. Mutually exclusive with `--workspace`. |
| `--workspace <MANIFEST>` |  | Explain over the estate graph. Mutually exclusive with `--db`. |
| `--depth <DEPTH>` | `5` | Maximum reverse-traversal depth (must match the impact run explained). |
| `--min-confidence <MIN_CONFIDENCE>` | `0` | Drop paths below this accumulated confidence. |
| `--no-contracts` | off | Do **not** follow the contract plane (producer → operation → consumer). |
| `--no-infra` | off | Do **not** follow the infra plane (`Assumes`/`Routes`/`Runs`). |

## context

Show the 360° context of a symbol (callers, callees, imports, members, and the
contract/infra/data buckets).

```
strata context [OPTIONS] <SYMBOL>
```

| Argument / flag | Default | Description |
|---|---|---|
| `<SYMBOL>` |  | Symbol to inspect (fully-qualified name preferred, else name). |
| `--db <PATH>` | `.strata/graph.duckdb` | Graph database path. Mutually exclusive with `--workspace`. |
| `--workspace <MANIFEST>` |  | Run context over the estate graph. Mutually exclusive with `--db`. |

The buckets context surfaces match the [MCP `context` tool](mcp.md#context).

## query

Lexical search over node name, fully-qualified name, and path (case-insensitive
substring).

```
strata query [OPTIONS] <TEXT>
```

| Argument / flag | Default | Description |
|---|---|---|
| `<TEXT>` |  | Substring to search for. |
| `--db <PATH>` | `.strata/graph.duckdb` | Graph database path. Mutually exclusive with `--workspace`. |
| `--workspace <MANIFEST>` |  | Search the estate graph. Mutually exclusive with `--db`. |

## mcp

Serve the code graph to an MCP client over stdio. This is a long-running server,
not a one-shot command. See [MCP](mcp.md).

```
strata mcp [OPTIONS]
```

| Argument / flag | Default | Description |
|---|---|---|
| `--db <PATH>` | `.strata/graph.duckdb` | Graph database path. Mutually exclusive with `--workspace`. |
| `--workspace <MANIFEST>` |  | Serve the linked estate graph over MCP. Mutually exclusive with `--db`. |
| `--repo <PATH>` | grandparent of `--db` when it ends `.strata/graph.duckdb` | Repository root for the `detect_changes` tool. Mutually exclusive with `--workspace`. |

The served graph **hot-reloads** when the on-disk index changes (see
[MCP → Hot-reload](mcp.md#hot-reload)).

## detect-changes

Report the changed symbols, blast radius, and risk vs HEAD: the mechanical
pre-commit check. It **reports; it never gates** and always exits 0.

```
strata detect-changes [OPTIONS]
```

| Argument / flag | Default | Description |
|---|---|---|
| `--db <PATH>` | `.strata/graph.duckdb` | Graph database path. Forces single-repo mode (no estate). Mutually exclusive with `--workspace`. |
| `--workspace <MANIFEST>` |  | Estate manifest. Forces estate mode: git-diffs the member repo and aggregates the blast radius over the full estate graph. Mutually exclusive with `--db`. |
| `--repo <PATH>` | grandparent of `--db` when it ends `.strata/graph.duckdb`, else the current directory | Repository root: the working tree to diff. |
| `--staged` | off | Diff the staged index (`git diff --cached HEAD`) instead of the working tree. |

With **neither** `--db` nor `--workspace`, `detect-changes` auto-resolves the
context: if the current repo carries an estate marker (written by `strata index
--workspace`), it runs over the full estate graph; otherwise it runs single-repo.
See [Multi-repo estates](../getting-started/estates.md).

## blast

Report the pre-edit blast radius of a *file*: the symbols it defines, the reverse
blast radius of changing them, and the risk. It **reports; it never gates** and
always exits 0. This command powers the [pre-edit hook](agent-kit.md#pretooluse-pre-edit-blast).

```
strata blast [OPTIONS] <FILE>
```

| Argument / flag | Default | Description |
|---|---|---|
| `<FILE>` |  | The file to assess (repo-relative, or absolute under the repo root). |
| `--db <PATH>` | `.strata/graph.duckdb` | Graph database path. Forces single-repo blast (no estate). Mutually exclusive with `--workspace`. |
| `--workspace <MANIFEST>` |  | Estate manifest. Forces estate blast: the file's reverse blast radius over the full estate graph, so a change to a producer surfaces consumers in other repos. Mutually exclusive with `--db`. |
| `--repo <PATH>` | grandparent of `--db` when it ends `.strata/graph.duckdb` | Repository root. Used to make an absolute `<FILE>` repo-relative. |
| `--format <FORMAT>` | `text` | Output format: `text` (human summary) or `agent` (the terse, token-lean block the pre-edit hook injects). An unrecognised value falls back to `text`. |

With **neither** `--db` nor `--workspace`, `blast` auto-resolves the context from
the file's repo: an estate marker (written by `strata index --workspace`) means
the radius is computed over the full estate graph; otherwise it is single-repo.
This is what makes the [pre-edit hook](agent-kit.md#pretooluse-pre-edit-blast)
estate-aware with no per-repo configuration.

## rename

Graph-aware multi-file rename of a code symbol. **Dry-run by default**; pass
`--apply` to write. Edits land only in graph-implicated files.

```
strata rename [OPTIONS] <OLD> <NEW>
```

| Argument / flag | Default | Description |
|---|---|---|
| `<OLD>` |  | The current symbol name (fully-qualified name preferred, else name). |
| `<NEW>` |  | The new identifier. |
| `--db <PATH>` | `.strata/graph.duckdb` | Graph database path. |
| `--repo <PATH>` | grandparent of `--db` when it ends `.strata/graph.duckdb`, else the current directory | Repository root. |
| `--apply` | off | Write the edits to disk. Default is a dry run that lists edits only. |
| `--force` | off | Proceed even if a repo-wide symbol is already named `<NEW>`. |
| `--uid <UID>` |  | Pin one candidate when `<OLD>` resolves to several code nodes. |

This command takes no `--workspace`: it is single-repo only.

## init

Install a strictly-governed agent-integration kit (MCP registration, steering,
skills, scoped hooks), idempotent and merge-safe. Bare `init` lists the
supported agents. Fully documented in [Agent kit](agent-kit.md).

```
strata init [OPTIONS] [AGENT]
```

| Argument / flag | Default | Description |
|---|---|---|
| `[AGENT]` |  | Which agent to set up: `claude` or `kiro`. Omit to list supported agents. An unknown agent is an error naming the supported set. |
| `--path <DIR>` | `.` | Project root to install into. |
| `--yes` | off | Run any needed `strata index` non-interactively (no prompts). With `--yes` and no index yet, `init` indexes first so the steering identity line carries real counts. |

## Exit codes

`main` maps the handler result to a process exit code
(`crates/strata-cli/src/lib.rs`, `CliError::exit_code`):

| Exit code | Meaning |
|---|---|
| `0` | Success. |
| `1` | Generic failure: no index found, symbol not found, IO/store/index error, or `index` invoked with neither `<PATH>` nor `--workspace`. |
| `2` | **Ambiguous symbol**: the symbol matched more than one node and the caller must disambiguate (re-run with `--uid`). |

`detect-changes` and `blast` always exit 0 by design (they report, never gate).
