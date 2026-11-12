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
strata init kiro-crew                # the Kiro kit + the committed project layer
strata init <agent> --path <DIR> --yes
```

Supported agents: `claude`, `kiro`, `kiro-crew`. An unknown agent is an error naming the
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
| `write_owned`          | skills, Kiro hook files                              | For files StrataGraph wholly owns: write/overwrite freely, reporting `created` / `updated` / `unchanged` by byte-compare. Parent directories are created.                                                                                                                                                                                                                                                                 |

Claude hook entries are merged **structurally**: every StrataGraph-owned hook command
carries the marker token `strata-hook` (in a trailing comment). On re-run,
`upsert_hook` drops any prior entry containing the token and appends the current
one, never duplicating, never disturbing foreign hooks (which lack the token).

## Install scopes

`strata init claude` supports two scopes, selected by `--global` / `--scope`.

| Flag                        | Scope              | Installs into               |
| --------------------------- | ------------------ | --------------------------- |
| (none) / `--scope project`  | Repo (default)     | The current repository root |
| `--global` / `--scope user` | Global (user-wide) | `~/.claude`                 |

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
in order: the identity line, an **Always Do (MUST)** section (including the
knowledge-plane rules: act on the pre-edit hook's `docs:` line conditionally,
treat doc guidance as repo knowledge rather than ground truth, report
`detect_changes`' "docs to review" line at commit time), a **Never Do**
section, a **Tools (MCP)** reference (`impact`, `explain`, `context`, `query`,
`detect_changes`, `guidance`, `search_docs`), an auto-reload note, and the
**Skill routing** table mapping task types to the four skills. The block and
each skill stay ≤120 lines.

### Skills

The four skills (`content::skills`) are static, agent-independent SKILL.md bodies,
each with `name`/`description` frontmatter and the shared confidence-band and
blast-radius/risk tables:

| Slug                         | Purpose                                                                                                                                  |
| ---------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------- |
| `strata-guide`               | First contact: which tool, the plane model, the band policy, the safe-change protocol, repo-knowledge lookup (`guidance`/`search_docs`). |
| `strata-exploring`           | "How does X work?": `guidance`/`search_docs` first on unfamiliar areas, then `query` → `context` → follow buckets across planes.         |
| `strata-impact-analysis`     | "What breaks if I change X?": `impact`, the `will_break` verdict, `members_with_dependents`, `explain`.                                  |
| `strata-contracts-and-infra` | Schema/API/infra: producers, consumers, dead-surface discovery.                                                                          |

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

| Target                                                        | Writer                                         | Contents                                                                                                                    |
| ------------------------------------------------------------- | ---------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------- |
| `~/.claude.json` (MCP registration)                           | `claude mcp add --scope user` (CLI subprocess) | Registers `strata` as a user-scoped MCP server; `~/.claude.json` is managed by Claude Code, never hand-edited by this tool. |
| `~/.claude/settings.json`                                     | `edit_json` (same merge-safe writer)           | The three scoped hooks, merged in.                                                                                          |
| `~/.claude/skills/strata/strata-guide/SKILL.md`               | `write_owned`                                  | Skill: which tool to use.                                                                                                   |
| `~/.claude/skills/strata/strata-exploring/SKILL.md`           | `write_owned`                                  | Skill: understand architecture.                                                                                             |
| `~/.claude/skills/strata/strata-impact-analysis/SKILL.md`     | `write_owned`                                  | Skill: blast radius.                                                                                                        |
| `~/.claude/skills/strata/strata-contracts-and-infra/SKILL.md` | `write_owned`                                  | Skill: producers/consumers/dead surface.                                                                                    |
| `~/.claude/CLAUDE.md`                                         | `upsert_managed_block`                         | Generic steering block (no per-repo identity line).                                                                         |

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

| File                             | Writer                 | Contents                                                             |
| -------------------------------- | ---------------------- | -------------------------------------------------------------------- |
| `.kiro/settings/mcp.json`        | `merge_json`           | Adds `mcpServers.strata` (`{ "command": "strata", "args": [ … ] }`). |
| `.kiro/steering/strata.md`       | `upsert_managed_block` | The managed steering block (Kiro routing).                           |
| `.kiro/hooks/strata-pre-edit.*`  | `write_owned`          | Pre-edit impact check (fires on the file-write tools).               |
| `.kiro/hooks/strata-post-edit.*` | `write_owned`          | Post-edit reindex (fires on the file-write tools).                   |

The steering block is the same content as Claude's, but its routing section is the
Kiro cross-references (Kiro reads steering files, not skills): it names the two
lifecycle hooks and the `query → context → impact → detect_changes` flow. Each
hook file is pretty-printed JSON with a trailing newline.

> **No pre-commit hook on Kiro.** Kiro can only trigger a hook by tool **name**
> (the `matcher` is a regex over the tool name, not the command text), and there
> is no "git commit" tool — a commit runs through the same shell tool as every
> build and `git log`. A pre-commit hook would therefore fire on all shell use.
> So the commit-time check lives in the **steering** instead (the `MUST run
detect_changes before committing` rule Kiro reads), and both hooks ride the
> mechanically scoped write tools. (Claude Code's matcher can inspect the command
> text, so it _does_ run a real pre-commit hook — this is Kiro-specific.) Any
> previously installed `strata-pre-commit` / `strata-post-commit` hook is removed
> on install.

### Kiro hooks

`strata init kiro` **auto-detects** the hook format from the repo's existing
`.kiro/hooks` (a `.json` hook ⇒ `new`, a `.kiro.hook` ⇒ `old`); a fresh repo
defaults to **`new`**, the `.json` schema current Kiro reads. Pass
`--kiro-version old|new` to force one (Kiro changed its hook schema between
releases; installing one format removes the other's StrataGraph hook files). Both
formats carry identical hook data; only the envelope differs:

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

| File                    | `trigger`     | `matcher`                          | `action`  | Detail                                                                                                                   |
| ----------------------- | ------------- | ---------------------------------- | --------- | ------------------------------------------------------------------------------------------------------------------------ |
| `strata-pre-edit.json`  | `PreToolUse`  | `fs_write\|str_replace\|fs_append` | `agent`   | A STOP-style prompt confirming blast-radius assessment (`blast`/`impact`/`context`) across planes before any file write. |
| `strata-post-edit.json` | `PostToolUse` | `fs_write\|str_replace\|fs_append` | `command` | `strata index .` with `timeout: 120`: reindex after a file edit (the MCP server hot-reloads the fresh index).            |

## `strata init kiro-crew`

Everything `strata init kiro` writes, plus five more files (`kiro_crew::install`
runs `kiro::install` first, then appends its own):

| File                                               | Writer        | Contents                                                                |
| -------------------------------------------------- | ------------- | ----------------------------------------------------------------------- |
| `.kiro/skills/strata-guide/SKILL.md`               | `write_owned` | The `strata-guide` skill body, verbatim.                                |
| `.kiro/skills/strata-exploring/SKILL.md`           | `write_owned` | The `strata-exploring` skill body, verbatim.                            |
| `.kiro/skills/strata-impact-analysis/SKILL.md`     | `write_owned` | The `strata-impact-analysis` skill body, verbatim.                      |
| `.kiro/skills/strata-contracts-and-infra/SKILL.md` | `write_owned` | The `strata-contracts-and-infra` skill body, verbatim.                  |
| `.kiro/agents/strata.json`                         | `write_owned` | The project agent spec (below), pretty-printed with a trailing newline. |

The skill bodies are the exact same text `strata init claude` writes under
`.claude/skills/strata/<slug>/SKILL.md` — `content::skills()` is the single
source for both. They are written unchanged rather than re-frontmattered: the
existing frontmatter already carries the `name` and `description` a Kiro project
skill needs, `triggers` is optional (consulted only when trigger injection is
enabled) and `always` defaults to `false`, which is what the kit wants — the
always-on channel for this content is the steering block.

Each slug is a **direct child** of `.kiro/skills/`. Kiro walks
`.kiro/skills/*/SKILL.md` — exactly one level — so the `strata/<slug>` nesting the
Claude kit uses under `.claude/skills/` would never be discovered here.

### The project agent spec

`.kiro/agents/strata.json` is a workspace-local custom agent. Kiro resolves
`.kiro/agents/` before `~/.kiro/agents/`, with no upward walk.

| Key                 | Value                                                                                                                     |
| ------------------- | ------------------------------------------------------------------------------------------------------------------------- |
| `name`              | `strata` — selects the agent (`kiro-cli --agent strata`, or the agent picker).                                            |
| `mcpServers.strata` | `{ "command": "strata", "args": [ … ] }` — the same detected args as the `mcp.json` registration.                         |
| `includeMcpJson`    | `true` — also starts the enabled servers of the user-level and workspace `mcp.json` that the spec does not define itself. |
| `resources`         | `file://.kiro/steering/**/*.md` and `skill://.kiro/skills/**/SKILL.md`.                                                   |
| `permissions`       | One rule allowing `strata/*` MCP tools.                                                                                   |
| `hooks`             | `agentSpawn`, `postToolUse` (matcher `fs_write`), `stop`.                                                                 |

No `model`, `tools` or `excludedTools`: a committed file must not override the
user's own choice for every session in the repo.

### Kiro Crew hooks

These are the agent spec's own `hooks`, which the Kiro CLI reads itself and which
Kiro Crew fires from its turn loop for backends whose harness cannot. They are a
different mechanism from the standalone `.kiro/hooks/*` files above, and use the
camelCase event names:

| Event         | `matcher`  | Action  | Detail                                                                                                                                              |
| ------------- | ---------- | ------- | --------------------------------------------------------------------------------------------------------------------------------------------------- |
| `agentSpawn`  | —          | command | Prints the "no index yet" line when `.strata/` exists but `graph.duckdb` does not; silent otherwise. Stdout on this event enters the model context. |
| `postToolUse` | `fs_write` | command | Backgrounds `strata index . --db ./.strata/graph.duckdb` after a write.                                                                             |
| `stop`        | —          | command | Prints the `detect_changes` pre-commit reminder. Plain stdout, never the `{"decision":"block"}` form that would request another turn.               |

Three properties of these commands are load-bearing:

- **No `preToolUse` entry.** That event fails closed in Kiro Crew — exit `2`
  denies the tool call, and so does any other non-zero exit, a timeout or a
  crash. The engine's edit-time advice is advisory, so it never sits there; the
  pre-edit blast check stays on the standalone `strata-pre-edit` hook and, for
  Claude Code, `PreToolUse`.
- **The matcher is a glob, not a regex.** Unlike the standalone hooks' `matcher`
  (a regex over the tool name), a Crew tool-event matcher is matched with
  filename-style globbing, so `fs_write|str_replace|fs_append` would match
  nothing. `fs_write` alone covers every write: Crew resolves `str_replace`,
  `fs_append` and `delete_file` onto that same row.
- **The guards are cwd-relative.** A Crew hook subprocess receives a strict
  environment allowlist (`PATH`, `HOME`, `KIROCREW_HOME`, locale and TLS keys)
  with no project-directory variable, unlike `$CLAUDE_PROJECT_DIR`. Crew spawns
  the hook in the session workspace, so each command tests `./.strata`, checks
  `command -v strata`, and always exits 0.

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

### Estate enrollment does not require re-running `strata init`

`strata init` writes the MCP registration and hooks once. Because estate
resolution happens at runtime (by reading the marker), enrolling a new repo in
an estate after `init` was first run does not require re-running `strata init`
in that repo: the next session picks up the marker automatically.

If you run `strata init` again in a member repo after enrollment, the writers
are idempotent: the MCP args will be updated to the bare `["mcp"]` estate form
(the server auto-resolves the estate from the marker at runtime) and the hooks
will be refreshed, but no existing foreign content is disturbed.

### Updating the kit after an engine upgrade

The one case where re-running **is** the required step: upgrading the binary.
The installed steering, skills, and hook files are written text — they do not
update themselves. After an upgrade, re-run `strata init claude` /
`strata init kiro` in each kitted repo (and `strata init claude --global` if
you use the global kit), then restart the editor session so the new hooks and
server load. The re-run is idempotent and merge-safe as above; for Kiro it
also upgrades or removes hook files that changed between releases. See
[Upgrading](../getting-started/upgrading.md) for the full sequence.

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
