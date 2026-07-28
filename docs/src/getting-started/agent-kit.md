# Wire up your editor (agent kit)

StrataGraph's biggest payoff is giving your AI coding agent a blast-radius check before
it edits. The `strata init` command wires that up in one shot: it registers the
MCP server, installs steering and skills, and adds lifecycle hooks, all
idempotently and merge-safely, so it is safe to re-run and safe alongside an
existing setup.

Two agents are supported today:

```bash
strata init          # lists supported agents
```

```text
Usage: strata init <agent>
Supported agents: claude, kiro
```

## `strata init claude`

```bash
strata init claude
```

This writes, under the project root:

- **`.mcp.json`**: registers the `strata` MCP server. The launch command is the
  `strata` binary with `mcp --db .strata/graph.duckdb` (or
  `mcp --workspace strata.workspace.toml` if a workspace manifest is present; see
  [Multi-repo estates](estates.md)). The write is a **merge-add**: a foreign
  MCP server already in the file is preserved.
- **`CLAUDE.md`** and **`AGENTS.md`**: the same managed steering block, inserted
  between markers so it can be updated in place without disturbing the rest of
  your file. The block states the impact-before-edit rules, the confidence-band
  trust policy, the dead-surface rule, and a skill-routing table.
- **`.claude/skills/strata/<slug>/SKILL.md`**: four task-routed skills:
  `strata-guide` (first contact / which tool), `strata-exploring` (understand
  architecture), `strata-impact-analysis` (blast radius), and
  `strata-contracts-and-infra` (producers, consumers, dead surface).
- **`.claude/settings.json`**: three scoped hooks (described
  [below](#the-hooks)), each carrying a `strata-hook` marker so a re-run updates
  them in place rather than duplicating.

A summary of exactly what was created / updated / left unchanged is printed when
it finishes.

## `strata init kiro`

```bash
strata init kiro                      # auto-detects the hook format (fresh repo → new .json)
strata init kiro --kiro-version old   # force the legacy `.kiro.hook` format
```

For [Kiro](https://kiro.dev), the kit is the same idea in Kiro's native formats:

- **`.kiro/settings/mcp.json`**: the same merge-add MCP registration.
- **`.kiro/steering/strata.md`**: the managed steering block (Kiro reads
  steering files, so the block ends with steering cross-references and the
  lifecycle hooks rather than a Claude skill table). Because Kiro has no
  commit-specific hook trigger, the "run `detect_changes` before you commit"
  discipline lives here as a rule the agent follows itself, not as a hook.
- **Two lifecycle hooks** (`strata-pre-edit`, `strata-post-edit`), both scoped to
  the file-write tools, written in the format your Kiro reads. `strata init kiro`
  **auto-detects** that format from the repo's existing hooks (`.json` ⇒ new,
  `.kiro.hook` ⇒ old); a fresh repo defaults to **new** (`.json`, `version: "v1"`,
  the schema current Kiro reads). Pass `--kiro-version old|new` to force one.
  There is deliberately no pre-commit hook (Kiro would fire it on every shell
  command); any previously installed one is removed on install.

  Both carry the identical hooks (same prompts, the same `detect_changes`
  pre-commit check, the same reindex command); only the envelope differs.
  Installing one version removes the other format's StrataGraph hook files, so the two
  never coexist.

## The hooks

The hooks are what make the discipline reliable instead of something the agent
has to remember. For Claude Code:

- **PreToolUse** (matches `Edit | Write | MultiEdit`): the **pre-edit blast
  check**. Before the agent edits a file, this hook computes that file's blast
  radius (`strata blast <file> --format agent`) and injects it as context, so the
  agent sees what depends on the file _before_ it changes it. It is
  **non-blocking**: it only adds context and always exits 0; it never blocks or
  loops an edit. It is **silent when clean**: no index, no file path, or an empty
  result means it stays quiet. It is **degrade-safe**: any failure falls through
  to exit 0. (When `jq` is installed it injects the real computed blast; without
  `jq` it still injects a static "run impact first" advisory, so the discipline
  holds either way.)
- **PostToolUse** (matches `Edit | Write | MultiEdit`): **stay fresh**. After an
  edit, it backgrounds an incremental `strata index` so the graph keeps up with
  your changes. Silent when there is no `.strata/` directory.
- **SessionStart**: when the graph is missing, it prints a one-line reminder to
  run `strata index .`, and is silent otherwise.

For Kiro the same lifecycle is expressed as `PreToolUse` / `PostToolUse` hooks:
pre-edit confirms `impact` was run, pre-commit drives the `detect_changes` tool,
and post-commit re-runs `strata index .`.

> The PreToolUse hook is the same report you would get from `strata blast <file>`.
> When you see it in your editor, act on it: report the affected dependents and
> risk, and pause for direction on HIGH/CRITICAL or anything crossing a repo
> boundary.

## Idempotent and merge-safe

`strata init` is safe to run repeatedly. JSON files (`.mcp.json`,
`.claude/settings.json`) are merged, not overwritten: your foreign MCP servers
and foreign hooks survive. The steering blocks are bounded by markers and updated
in place. The hooks are keyed on a `strata-hook` marker, so a second run reports
everything as **unchanged** rather than appending duplicates. You can re-run it
after upgrading StrataGraph to refresh the managed content without losing your own
edits around it.

## Global vs repo install

By default, `strata init claude` installs the kit into the **current repository**
(repo scope). That is the right choice for most teams: the files are committed,
shared with everyone, and the identity line is pinned to this specific repo.

When you work across many repos and want a single personal install that covers
all of them, use **global scope** instead:

```bash
strata init claude --global     # same as --scope user
strata init claude --scope user # explicit form
```

A repo-scoped install uses `--scope project` (the default; that flag is optional):

```bash
strata init claude              # default: repo scope
strata init claude --scope project  # same, explicit
```

Note: `--global` and `--scope user` are equivalent. Combining `--global` with
`--scope project` is an error (the two flags conflict on scope and are rejected).

### What each scope writes

**Repo scope** (`--scope project`, the default) writes into the repo:

| File | Location |
|---|---|
| `.mcp.json` | Repo root |
| `CLAUDE.md`, `AGENTS.md` | Repo root |
| `.claude/skills/strata/…` | Repo root |
| `.claude/settings.json` | Repo root |

These files are committed and shared with the team. The steering block carries
a per-repo identity line naming this repo's node count and planes.

**Global scope** (`--global` / `--scope user`) writes into your home directory:

| File | Location |
|---|---|
| MCP server registration | `~/.claude.json` (managed by `claude mcp add`, never hand-edited) |
| `~/.claude/settings.json` | Hooks (merged in, same merge-safe writer) |
| `~/.claude/skills/strata/…` | Skills (same four SKILL.md files) |
| `~/.claude/CLAUDE.md` | Generic steering block (no per-repo identity line) |

No global `AGENTS.md` is written (Claude Code does not read a global one).

### How the global MCP server resolves the current repo

The global MCP server (`strata mcp`, registered with `claude mcp add --scope user`)
resolves the active project from `$CLAUDE_PROJECT_DIR` at request time, so a
single server entry serves whichever repo you have open, in single-repo or estate
mode depending on that repo's `.strata/` marker.

### How the hooks self-activate

The globally-installed hooks guard on `.strata/`: they are silent and a no-op in
any directory that has not been indexed by StrataGraph. The SessionStart nudge is
also silenced globally outside Strata repos, so they produce no noise in
unrelated projects.

### Prerequisite: `claude` CLI must be on PATH

The global install registers the MCP server by shelling out to the official
`claude` CLI (`claude mcp add strata --scope user -- strata mcp`). If `claude`
is not on PATH, the global install aborts before writing anything (all-or-nothing).
This is already satisfied if you use Claude Code normally.

### Which scope to use (precedence note)

Claude Code runs **both** project-level and user-level (global) hooks. Do not
install a repo-scoped kit and a global kit in the same repo, or you get duplicate
blast injections and double reindexes on every edit.

Pick one:

- **Global** (`--global`): install once, covers every repo, private to you.
  Good for personal machines where you open many repos.
- **Repo** (default): committed, shared with the team, identity pinned.
  Good for projects where every contributor should have the kit automatically.

## Index first, then restart once

The kit needs a graph to serve. If you have not indexed yet, the summary tells
you to run [`strata index .`](first-index.md) first. (You can also pass
`strata init claude --yes` to have `init` build the index for you
non-interactively when none exists.)

After installing the kit, **restart your editor session once** so it picks up the
new MCP server and steering. That is the only manual restart you need,
because:

## Hot-reload keeps the index current

The MCP server **hot-reloads**. After the first start, when the on-disk index
changes (the PostToolUse reindex hook, or a manual `strata index`), the server
swaps in the fresh graph before the next request, with no further restart. It is
degrade-safe: a reindex caught mid-write keeps the previous graph and retries, so
a tool call never blocks or serves a half-loaded graph. The server keys off
`.strata/index.stamp`, falling back to the database's modification time for
indexes written before that marker existed. (An estate served with `--workspace`
reloads the same way on a manifest or per-repo change.)

So the rhythm is: install the kit, index once, restart your editor once, and
from then on your edits stay reflected in the graph automatically.

## Full inventory

For the complete list of every file the kit writes, the exact hook commands, and
the steering/skill contents, see the [agent kit reference](../reference/agent-kit.md).
