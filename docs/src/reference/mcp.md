# MCP server

The reference for StrataGraph's MCP server: how to run it, the protocol it speaks, and
every tool with its input schema and output shape. Grounded in
`crates/strata-mcp/src/server.rs` (the JSON-RPC wiring) and
`crates/strata-mcp/src/tools.rs` (the tool definitions, input schemas, and
`graph_schema_json`).

For the agent kit that registers this server automatically, see
[Agent kit](agent-kit.md).

## Running the server

```
strata mcp --db <repo>/.strata/graph.duckdb
strata mcp --workspace strata.workspace.toml
strata mcp --db <db> --repo <repo>
```

See the [CLI reference](cli.md#mcp) for the flags. Two notes that matter for the
tool surface:

- The **repo root** is what the filesystem-touching tools (`detect_changes`,
  `rename`) need. In single-DB mode it is `--repo` if given, else the
  **grandparent** of `--db` when the DB path ends `.strata/graph.duckdb`. In
  estate mode (auto-resolved, or explicit `--workspace`) it is `--repo` if given,
  else the current working directory (the member repo you launched from). Only
  when no root resolves at all (a non-canonical bare `--db`, or `--workspace`
  with no resolvable working directory) do those two tools return a clear "needs
  a repo root" error rather than guessing.
- `--workspace` (estate) mode serves the linked estate graph, and the
  filesystem-touching tools are estate-aware: `detect_changes` git-diffs the
  member repo (`--repo` or the cwd) and aggregates the blast radius across the
  **whole estate**, so a pre-commit check from one repo surfaces cross-repo
  dependents. `rename` stays repo-local to that member (estate-wide rename is a
  deferred follow-up).

## Protocol

The server is a hand-rolled, fully-synchronous **newline-delimited JSON-RPC 2.0**
server over stdio. It reads one JSON request per line from stdin and writes one
JSON response per line to stdout. Notifications (messages with no `id`) are
processed but produce no response.

- Protocol revision: `2024-11-05` (the `PROTOCOL_VERSION` constant).
- `serverInfo`: `{ "name": "strata-mcp", "version": <package version> }`.
- Advertised capabilities: `tools` and `resources`.

Supported methods:

| Method | Result |
|---|---|
| `initialize` | `{ protocolVersion, capabilities: { tools, resources }, serverInfo }`. |
| `ping` | `{}`. |
| `tools/list` | `{ "tools": [ <descriptor>, … ] }`: the 7 tool descriptors below. |
| `tools/call` | A tool result (see [Tool results](#tool-results)). |
| `resources/list` | The [`strata://schema`](#the-strataschema-resource) resource descriptor. |
| `resources/read` | The resource contents (only `strata://schema` is known). |

Unknown **request** methods return JSON-RPC error `-32601` ("method not found").
A parse error returns `-32700`. A `tools/call` with no `name` returns `-32602`.

### Tool results

`tools/call` always returns a *successful* JSON-RPC response. The result wraps
the tool's JSON payload as a single text content block:

```json
{ "content": [ { "type": "text", "text": "<JSON payload as a string>" } ], "isError": false }
```

A tool-level failure sets `"isError": true` and the text is
`{ "error": "<message>", "code": "<code>" }` where `code` is one of `not_found`,
`ambiguous`, or `bad_args`.

### Ambiguity (candidates)

`context`, `impact`, and `explain` surface an ambiguous symbol as a **candidates
payload** rather than an error, so a client can pin one and re-run:

```json
{ "ambiguous": true, "symbol": "<symbol>", "candidates": [ { "uid", "name", "kind", "path" }, … ] }
```

`explain` additionally tags which end was ambiguous with `"ambiguous_end":
"target"` or `"affected"`. Pin the target with `uid`, the affected end with
`affected_uid`.

## Tools

`tools/list` returns exactly these seven, in this order: `context`, `impact`,
`explain`, `query`, `blast`, `detect_changes`, `rename`. Every node in a payload
uses the compact shape `{ "uid", "name", "kind", "path" }`.

### context

The 360° view of one symbol.

| Arg | Type | Required | Description |
|---|---|---|---|
| `symbol` | string | yes | Identifier (fqn preferred, else name) to inspect. |

Output (resolved to one node): a `node` plus these buckets, each an array of
nodes (all always present):

- **code:** `callers`, `callees`, `imports_in`, `imports_out`, `members`,
  `container` (a single node or `null`).
- **contract:** `producers`, `consumers`, `produces`, `consumes`.
- **infra:** `assumes`, `assumed_by`, `routes_to`, `routed_from`, `runs`,
  `run_by`.
- **data:** `mapped_by`, `maps_to`.

All four contract buckets and all six infra buckets are present even when empty:
`producers (0) / consumers (0)` is the live dead-surface signal.

### impact

Reverse blast radius: everything that depends on `symbol` within `depth` hops.
Contract- and infra-aware by default.

| Arg | Type | Required | Default | Description |
|---|---|---|---|---|
| `symbol` | string | yes |  | Identifier whose dependents to compute. |
| `uid` | string | no |  | Pin one candidate when `symbol` resolves to several nodes. |
| `depth` | integer (`minimum: 0`) | no | `5` | Max reverse-traversal depth. |
| `min_confidence` | number (`0.0`–`1.0`) | no | `0.0` | Drop paths below this confidence. |
| `include_contracts` | boolean | no | `true` | Follow the contract plane (producer → operation → consumer). |
| `include_infra` | boolean | no | `true` | Follow the infra plane (incoming `Assumes`/`Routes`/`Runs`). |

Output:

```json
{
  "target": { "uid", "name", "kind", "path" },
  "affected": [ { "uid", "name", "depth", "confidence", "ambiguous", "will_break" }, … ]
}
```

`will_break` is the derived verdict (`confidence ≥ 0.40` AND not `ambiguous`,
independent of depth). When the target is member-bearing (class/struct/enum/
interface/table), its own `affected` is empty, but its **members** have
dependents, the result additionally carries:

```json
"members_with_dependents": [ { "uid", "name", "kind" }, … ]
```

This field is present **only** on the zero-direct case; pin a listed member and
re-run `impact` on it.

### explain

Why is `affected` in `symbol`'s blast radius? The evidence chain.

| Arg | Type | Required | Default | Description |
|---|---|---|---|---|
| `symbol` | string | yes |  | The changed target (fqn preferred, else name). `target` is accepted as an alias. |
| `affected` | string | yes |  | The affected node whose presence in the blast radius to explain. |
| `uid` | string | no |  | Pin the TARGET when it resolves to several nodes. |
| `affected_uid` | string | no |  | Pin the AFFECTED node when it resolves to several nodes. |
| `depth` | integer (`minimum: 0`) | no | `5` | Max reverse-traversal depth; must match the impact run being explained. |
| `min_confidence` | number (`0.0`–`1.0`) | no | `0.0` | Drop paths below this confidence. |
| `include_contracts` | boolean | no | `true` | Follow the contract plane. |
| `include_infra` | boolean | no | `true` | Follow the infra plane. |

Output when reachable:

```json
{
  "target": { … }, "affected": { … },
  "reachable": true,
  "confidence": <number>,
  "ambiguous": <bool>,
  "will_break": "<label>",
  "hops": [ { "from", "to", "edge_kind", "provenance", "confidence", "running_confidence" }, … ]
}
```

The final hop's `running_confidence` equals the overall `confidence`, which
equals `impact`'s confidence for that node (the consistency invariant). Honest
outcomes: an unreachable affected node returns `{ "reachable": false, "reason":
… }` (no `hops`); `target == affected` returns `reachable: true` with empty
`hops` at confidence `1.0`.

### query

Lexical search over node name, fully-qualified name, and path (case-insensitive
substring).

| Arg | Type | Required | Description |
|---|---|---|---|
| `text` | string | yes | Substring to search for. |

Output: `{ "matches": [ { "uid", "name", "kind", "path" }, … ] }`.

### blast

The pre-edit blast radius of a *file* (not a single symbol): the symbols the file
defines across all planes, the aggregated reverse blast radius of changing them
(the same dedupe/order as `detect_changes`), and the risk level. The analysis is
**graph-only**: it does not touch the filesystem or need the repo root. (The CLI's
[`blast --repo`](cli.md#blast) flag is *only* used to normalize an absolute
`<FILE>` to a repo-relative path before the lookup; it never changes the
analysis, which is why this MCP tool needs no repo root.)

| Arg | Type | Required | Description |
|---|---|---|---|
| `file` | string | yes | The file to assess, repo-relative (e.g. `src/foo.ts`). |

Output: the serialized `strata_index::BlastReport`. A file with no indexed
symbols returns an honest empty report carrying a `note`, never a fabricated
all-clear. Risk levels: LOW (`< 5` affected), MEDIUM (`5–15`), HIGH (`> 15`),
CRITICAL (contract surface or cross-repo).

### detect_changes

The mechanical pre-commit check: git-diff the working tree (or the staged index)
against HEAD, derive the changed symbols per plane (code / contract / infra),
aggregate the reverse blast radius over the loaded graph, and assign a risk
level. **Needs the server to know the repo root** (launch with `--db
<repo>/.strata/graph.duckdb` or `--repo <path>`).

| Arg | Type | Required | Default | Description |
|---|---|---|---|---|
| `staged` | boolean | no | `false` | Diff the staged index (`git diff --cached HEAD`) instead of the working tree. |

Output: the serialized `strata_index::ChangeReport`. Reports, never gates.

### rename

Graph-aware, confidence-tagged multi-file rename: the safe alternative to
find-and-replace. Resolves the symbol to one code node, edits the identifier only
in files the graph implicates (the definition file + files connected by a
call/import edge), and is **dry-run by default**. **Needs the repo root.**

| Arg | Type | Required | Default | Description |
|---|---|---|---|---|
| `symbol` | string | yes |  | The code symbol to rename (fqn preferred, else name). |
| `new_name` | string | yes |  | The new identifier. |
| `apply` | boolean | no | `false` | Write the edits to disk. Default is a dry run that returns the plan only. |
| `uid` | string | no |  | Pin one candidate when the symbol resolves to several code nodes. |
| `force` | boolean | no | `false` | Proceed even if a repo-wide symbol is already named `new_name`. |

> The MCP argument is `new_name`. (The equivalent CLI argument is the positional
> `<NEW>`; see [CLI → rename](cli.md#rename).)

Output: the serialized `strata_index::RenameOutcome`: either a `candidates` list
(ambiguous target) or a `plan` (the edit set; `applied` is true iff written).

## The `strata://schema` resource

`resources/list` advertises one resource:

| Field | Value |
|---|---|
| `uri` | `strata://schema` |
| `name` | `Strata graph schema` |
| `description` | `Node-kind and edge-kind vocabularies of the code graph.` |
| `mimeType` | `application/json` |

`resources/read` of that URI returns a contents entry whose `text` is the JSON
produced by `graph_schema_json`:

```json
{ "node_kinds": [ "Repo", "Package", … ], "edge_kinds": [ "Defines", "MemberOf", … ] }
```

These are the same node/edge variant names documented in the
[Schema reference](schema.md). Any other URI returns error `-32602`.

## Hot-reload

The served graph **hot-reloads**: before each request the server checks whether
the on-disk index changed and, if so, swaps in the freshly-loaded graph. The
request/response behaviour is byte-for-byte identical between reloads.

- It keys off the change signal `.strata/index.stamp` (written last by `strata
  index`), falling back to the `graph.duckdb` mtime for indexes written before
  the stamp existed.
- The reload is **degrade-safe**: a reindex caught mid-write fails the reload, the
  server keeps serving the previous graph, and the change signal is **not**
  advanced, so the next request retries. A half-loaded graph is never served and
  a tool call never blocks.
- No session or server restart is needed (the PostToolUse `strata index` hook, or
  a manual reindex, is picked up automatically).
- Estate (`--workspace`) mode reloads the same way on a manifest or per-repo
  change.
