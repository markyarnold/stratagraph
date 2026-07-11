# Agent kit

The complete inventory of what `strata init <agent>` writes: every file and
managed block, the skills, the MCP registration, every hook with its exact
trigger and command, and the merge-safety guarantees. Grounded in
`crates/strata-cli/src/init/{mod,content,claude,kiro,writers}.rs`.

For the command flags, see [CLI → init](cli.md#init).

## Invocation

```
strata init                          # list supported agents
strata init claude                   # install the Claude Code kit (repo scope)
strata init claude --global          # install into ~/.claude (global scope)
strata init claude --scope user      # same as --global
strata init claude --scope project   # same as the default (repo scope)
strata init kiro                     # install the Kiro kit
strata init <agent> --path <DIR> --yes
```

Supported agents: `claude`, `kiro`. An unknown agent is an error naming the
supported set; bare `init` prints the same list.

## Context detection

Before writing anything, `init` inspects the project root (`detect_context`) to
choose the MCP launch args and the steering identity line. Three cases apply:

- **Estate root** — `strata.workspace.toml` is present in the directory (you
  ran `init` at the workspace root) → MCP args
  `["mcp", "--workspace", "strata.workspace.toml"]`; the identity is loaded
  from the linked estate when every repo is indexed, else "not indexed".
- **Estate member** — `.strata/estate.toml` marker is present (you ran `init`
  inside a repo that has already been enrolled in an estate via
  `strata index --workspace`) → bare MCP args `["mcp"]`; the server
  auto-resolves the estate from the marker at runtime.
- **Single repo** — neither file is present → single-DB MCP args
  `["mcp", "--db", ".strata/graph.duckdb"]`; the identity is loaded from that
  DB if it exists, else "not indexed".

The identity line is either real (`This repo/estate is indexed by StrataGraph as
**<name>** (<N> nodes, <M> edges; planes present: …)`) or the honest "not yet
indexed" placeholder. Contract is reported present when GraphQL fields / API
operations exist; infra when Lambdas / cloud resources / IAM / AppSync exist.

With `--yes` and no index yet, `init` runs a single-repo `strata index` first so
the identity line carries real counts.

After install, `init` prints a per-file summary (each line tagged `created`,
`updated`, or `unchanged`) and a next-steps line.

## Merge-safety and idempotency

Four writers (`writers.rs`) guarantee foreign bytes are preserved exactly and a
re-run reports all-`unchanged`:

| Writer                 | Used for                                             | Behaviour                                                                                                                                                                                                                                                                                                                                                                                                                 |
| ---------------------- | ---------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `upsert_managed_block` | `CLAUDE.md`, `AGENTS.md`, `.kiro/steering/strata.md` | Owns only the text **between** the markers `<!-- strata:begin -->` and `<!-- strata:end -->`. File missing → create; markers absent → append after existing content; markers present → replace only the inter-marker region. Every byte outside the markers is preserved verbatim (a foreign `gitnexus` block survives).                                                                                                  |
| `merge_json`           | `.mcp.json`, `.kiro/settings/mcp.json`               | Deep-merges **only our keys** into existing JSON, preserving every foreign key/server/hook. Output is pretty-printed (2-space) with a trailing newline, rewritten only if bytes change. **Malformed** existing JSON is an actionable error: the file is left untouched, never clobbered.                                                                                                                                  |
| `edit_json`            | `.claude/settings.json`                              | Parses existing JSON and applies a structural edit (the hook merge below), preserving every foreign key/server/hook; pretty-printed with a trailing newline, rewritten only if bytes change. Malformed existing JSON is an actionable error: the file is left untouched. (`.claude/settings.json` needs the structural `upsert_hook` merge, not a plain key deep-merge, so it uses this writer rather than `merge_json`.) |
| `write_owned`          | skills, Kiro hook files                              | For files StrataGraph wholly owns: write/overwrite freely, reporting `created` / `updated` / `unchanged` by byte-compare. Parent directories are created.                                                                                                                                                                                                                                                                      |

Claude hook entries are merged **structurally**: every StrataGraph-owned hook command
carries the marker token `strata-hook` (in a trailing comment). On re-run,
`upsert_hook` drops any prior entry containing the token and appends the current
one, never duplicating, never disturbing foreign hooks (which lack the token).

## Install scopes

`strata init claude` supports two scopes, selected by `--global` / `--scope`.

| Flag | Scope | Installs into |
|---|---|---|
| (none) / `--scope project` | Repo (default) | The current repository root |
| `--global` / `--scope user` | Global (user-wide) | `~/.claude` |

Pick one scope per repo. Claude Code runs both project-level and user-level
hooks, so installing both in the same repo causes duplicate blast injections and
double reindexes. See [getting-started: Global vs repo install](../getting-started/agent-kit.md#global-vs-repo-install).

## `strata init claude` (repo scope)

Writes eight files (`claude::install`, scope `Project`):

| File                                                        | Writer                 | Contents                                 |
| ----------------------------------------------------------- | ---------------------- | ---------------------------------------- |
| `.mcp.json`                                                 | `merge_json`           | Adds `mcpServers.strata`.                |
| `CLAUDE.md`                                                 | `upsert_managed_block` | The managed steering block.              |
| `AGENTS.md`                                                 | `upsert_managed_block` | The same managed steering block.         |
| `.claude/skills/strata/strata-guide/SKILL.md`               | `write_owned`          | Skill: which tool to use.                |
| `.claude/skills/strata/strata-exploring/SKILL.md`           | `write_owned`          | Skill: understand architecture.          |
| `.claude/skills/strata/strata-impact-analysis/SKILL.md`     | `write_owned`          | Skill: blast radius.                     |
| `.claude/skills/strata/strata-contracts-and-infra/SKILL.md` | `write_owned`          | Skill: producers/consumers/dead surface. |
| `.claude/settings.json`                                     | `edit_json`            | The three scoped hooks.                  |

### MCP registration

`.mcp.json` gains:

```json
{ "mcpServers": { "strata": { "command": "strata", "args": [ <detected mcp args> ] } } }
```

The args are the estate or single-DB form from [context detection](#context-detection).

### Steering block

The block written between the markers (`content::render_steering_block`) carries,
in order: the identity line, an **Always Do (MUST)** section, a **Never Do**
section, a **Tools (MCP)** reference (`impact`, `explain`, `context`, `query`,
`detect_changes`), an auto-reload note, and the **Skill routing** table mapping
task types to the four skills. The block and each skill stay ≤120 lines.

### Skills

The four skills (`content::skills`) are static, agent-independent SKILL.md bodies,
each with `name`/`description` frontmatter and the shared confidence-band and
blast-radius/risk tables:

| Slug                         | Purpose                                                                                                 |
| ---------------------------- | ------------------------------------------------------------------------------------------------------- |
| `strata-guide`               | First contact: which tool, the plane model, the band policy, the safe-change protocol.                  |
| `strata-exploring`           | "How does X work?": `query` → `context` → follow buckets across planes.                                 |
| `strata-impact-analysis`     | "What breaks if I change X?": `impact`, the `will_break` verdict, `members_with_dependents`, `explain`. |
| `strata-contracts-and-infra` | Schema/API/infra: producers, consumers, dead-surface discovery.                                         |

### Hooks

`.claude/settings.json` gains three matcher-groups under `hooks`, each command
carrying the `strata-hook` marker. All are `exit 0` on every branch and
silent-when-clean.

#### PreToolUse: pre-edit blast

- **Matcher:** `Edit|Write|MultiEdit`.
- **Behaviour:** computes the edited file's blast radius and injects it as
  `hookSpecificOutput.additionalContext` (event `PreToolUse`) so the agent sees
  what depends on the file before changing it. It is **non-blocking**: it never
  emits a `permissionDecision`, so it cannot halt or loop an edit.
- **Silent-when-clean / degrade-safe:** exits 0 with no output when there is no
  `.strata/graph.duckdb`, no `file_path` in the stdin JSON, an empty blast, or any
  `strata` failure.
- **`jq`-optional:** when `jq` is present it parses `tool_input.file_path` from
  stdin and runs `strata blast "$f" --format agent` (no `--db`, so it
  auto-resolves estate-vs-single from the repo's marker), emitting the computed
  blast; when `jq` is absent it injects a **static advisory**
  `additionalContext` (the run-impact-first instruction) so the discipline holds
  without a JSON parser.

#### PostToolUse: stay-fresh reindex

- **Matcher:** `Edit|Write|MultiEdit`.
- **Behaviour:** silent (exit 0) when `.strata/` is absent; otherwise backgrounds
  an incremental reindex: `strata index "$d" --db "$d/.strata/graph.duckdb"`
  (the `--db` is pinned to the project dir, since the hook's cwd is not
  guaranteed to be the project root).

#### SessionStart: index guidance

- **Matcher:** `""` (empty, matches every session start).
- **Behaviour:** prints one guidance line **only** when the graph DB is missing
  (`StrataGraph: no index yet — run `strata index .` …`); silent otherwise.

## `strata init claude --global` (global scope)

Writes into `~/.claude` (`claude::install`, scope `User`). The target map
differs from the repo scope:

| Target | Writer | Contents |
|---|---|---|
| `~/.claude.json` (MCP registration) | `claude mcp add --scope user` (CLI subprocess) | Registers `strata` as a user-scoped MCP server; `~/.claude.json` is managed by Claude Code, never hand-edited by this tool. |
| `~/.claude/settings.json` | `edit_json` (same merge-safe writer) | The three scoped hooks, merged in. |
| `~/.claude/skills/strata/strata-guide/SKILL.md` | `write_owned` | Skill: which tool to use. |
| `~/.claude/skills/strata/strata-exploring/SKILL.md` | `write_owned` | Skill: understand architecture. |
| `~/.claude/skills/strata/strata-impact-analysis/SKILL.md` | `write_owned` | Skill: blast radius. |
| `~/.claude/skills/strata/strata-contracts-and-infra/SKILL.md` | `write_owned` | Skill: producers/consumers/dead surface. |
| `~/.claude/CLAUDE.md` | `upsert_managed_block` | Generic steering block (no per-repo identity line). |

No `AGENTS.md` is written globally (Claude Code does not read a global one).
No `.mcp.json` is written (the MCP server is registered via `claude mcp add`,
not a per-project JSON file).

### MCP registration (global)

The global MCP server is registered by running:

```
claude mcp add strata --scope user -- strata mcp
```

This delegates ownership of `~/.claude.json` to Claude Code. StrataGraph never
hand-edits that file. If `claude` is not on PATH, the global install aborts
before writing any file (all-or-nothing).

The registered server command is `strata mcp` with no explicit `--db` or
`--workspace`. At request time the server resolves the active project from
`$CLAUDE_PROJECT_DIR` (set by Claude Code to the open project directory),
selects estate or single-repo mode from the repo's `.strata/` marker, and
serves the appropriate graph. One global server entry therefore covers every
repo you open.

### Steering block (global)

The `~/.claude/CLAUDE.md` block is the same managed content as the repo-scope
block (same markers, same merge-safety), with one difference: there is no
per-repo identity line. The block states the impact-before-edit rules, the
confidence-band trust policy, the dead-surface rule, and the skill-routing
table, all in generic terms applicable to any repo.

### Hooks (global)

The same three hooks are written into `~/.claude/settings.json`:

- **PreToolUse (pre-edit blast):** guards on `.strata/graph.duckdb` in the
  current directory; silent and a no-op in any repo that has not been indexed.
- **PostToolUse (stay-fresh reindex):** silent when `.strata/` is absent; only
  triggers an incremental reindex in indexed repos.
- **SessionStart (index guidance):** silenced outside Strata repos (guards on
  `.strata/`); no noise in unrelated projects.

The same `strata-hook` marker is used, so a re-run is fully idempotent.

### Prerequisite: `claude` CLI on PATH

The `claude` CLI must be on PATH before running `strata init claude --global`.
If it is absent, the command aborts before writing anything. This is already
satisfied when using Claude Code normally.

## `strata init kiro`

Writes five files (`kiro::install`):

| File                                  | Writer                 | Contents                                                             |
| ------------------------------------- | ---------------------- | -------------------------------------------------------------------- |
| `.kiro/settings/mcp.json`             | `merge_json`           | Adds `mcpServers.strata` (`{ "command": "strata", "args": [ … ] }`). |
| `.kiro/steering/strata.md`            | `upsert_managed_block` | The managed steering block (Kiro routing).                           |
| `.kiro/hooks/strata-pre-edit.*`       | `write_owned`          | Pre-edit impact check (`.kiro.hook` by default, `.json` with `--kiro-version new`). |
| `.kiro/hooks/strata-pre-commit.*`     | `write_owned`          | Pre-commit scope check (prompt-gated: applies only to commit commands). |
| `.kiro/hooks/strata-post-edit.*`      | `write_owned`          | Post-edit reindex.                                                  |

The steering block is the same content as Claude's, but its routing section is the
Kiro cross-references (Kiro reads steering files, not skills): it names the three
lifecycle hooks and the `query → context → impact → detect_changes` flow. Each
hook file is pretty-printed JSON with a trailing newline.

### Kiro hooks

`strata init kiro` defaults to **`--kiro-version old`** (Kiro changed its hook
schema between releases; installing one version removes the other's StrataGraph hook
files). Both versions carry identical hook data; only the envelope differs:

- **`old` (default), `*.kiro.hook`:** `{ enabled, name, description, version: "1",
  when: { type: "preToolUse" | "postToolUse", toolTypes: [ … ] }, then: { type:
  "askAgent" | "runCommand", … } }`.
- **`new`, `*.json`:** a top-level `version: "v1"` wrapping a `hooks` array whose
  single entry carries a `name`, `description`, `trigger` (PascalCase), an
  optional `matcher` (a regex tested against the tool name), and an `action`
  (`type: "agent" | "command"`).

The per-hook trigger/matcher in the table below use the `new`-format names; the
`old` format expresses the same via `when.type` (`preToolUse`/`postToolUse`) and
`when.toolTypes`.

| File                      | `trigger`     | `matcher`                          | `action`  | Detail                                                                                                                                                     |
| ------------------------- | ------------- | ---------------------------------- | --------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `strata-pre-edit.json`    | `PreToolUse`  | `fs_write\|str_replace\|fs_append` | `agent`   | A STOP-style prompt confirming blast-radius assessment (`blast`/`impact`/`context`) across planes before any file write.                                   |
| `strata-pre-commit.json`  | `PreToolUse`  | `execute_bash\|executeBash`        | `agent`   | An applicability-gated prompt: for a command that creates a git commit, it drives the `detect_changes` tool (pass `staged:true` for a partial commit) for the per-plane changed symbols, blast radius, and risk; any other command (including strata's own invocations, so the hook can never loop on its own remediation) proceeds untouched. Kiro matchers scope by tool name only, so the prompt carries this gate. |
| `strata-post-edit.json`   | `PostToolUse` | `fs_write\|str_replace\|fs_append` | `command` | `strata index .` with `timeout: 120`: reindex after a file edit (the MCP server hot-reloads the fresh index). Replaces the retired `strata-post-commit` hook, which is removed on install.                                                                                              |

## Estates

When you index an estate with `strata index --workspace <manifest>`, StrataGraph
records a membership marker, `.strata/estate.toml`, inside each member repo. The
marker holds the manifest path, the estate name, and the repo's declared name.

At runtime, the agent kit reads this marker to resolve the estate automatically:

- The **MCP server** (`strata mcp`, bare) serves the linked estate graph rather
  than the local single-repo DB.
- The **pre-edit blast hook** computes blast radius against the full estate
  graph, so edits to a producer surface cross-repo consumers before the change
  lands.
- **`strata detect-changes`** aggregates changed symbols across the estate for
  the pre-commit scope check.
- **`strata index <member>`** (the post-commit reindex hook) reindexes the repo
  with its estate-qualified identity and keeps the marker, so the estate stays
  fresh after each commit without re-running `--workspace`.

### Re-running `strata init` is not required

`strata init` writes the MCP registration and hooks once. Because estate
resolution happens at runtime (by reading the marker), enrolling a new repo in
an estate after `init` was first run does not require re-running `strata init`
in that repo: the next session picks up the marker automatically.

If you run `strata init` again in a member repo after enrollment, the writers
are idempotent: the MCP args will be updated to the bare `["mcp"]` estate form
(the server auto-resolves the estate from the marker at runtime) and the hooks
will be refreshed, but no existing foreign content is disturbed.

### Explicit overrides

- **`--db <path>`**: pins any command to a single-repo DB, ignoring the marker.
  Use this for isolated single-repo queries.
- **`--workspace <manifest>`**: forces estate mode, regardless of the marker or
  working directory.

### Bounds

A repo is estate-aware only after `strata index --workspace` has recorded its
marker. Before that, commands fall back to single-repo mode (degrade-safe).

`rename` is repo-local for now: it updates references within the current repo
only. Estate-wide rename is a deferred follow-up.
