# Wire up your editor (agent kit)

StrataGraph's biggest payoff is giving your AI coding agent a blast-radius check before
it edits. The `strata init` command wires that up in one shot: it registers the
MCP server, installs steering and skills, and adds lifecycle hooks, all
idempotently and merge-safely, so it is safe to re-run and safe alongside an
existing setup.

Three agents are supported today:

```bash
strata init          # lists supported agents
```

```text
Usage: strata init <agent>
Supported agents: claude, kiro, kiro-crew
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

## `strata init kiro-crew`

```bash
strata init kiro-crew
```

A **superset** of `strata init kiro`: the same MCP registration, steering block
and lifecycle hooks, plus the repo-committed **project layer** that
[Kiro Crew](https://kiro.dev/docs/crew/), the Kiro CLI and the Kiro IDE all read.
Run `kiro` and then `kiro-crew` on the same repo and the shared layer is left
byte-identical; only the project layer is added.

Two directories do the work, and both are version-controlled, so the whole team
gets the same setup from a checkout:

- **`.kiro/skills/<slug>/SKILL.md`**: the same four task-routed skills the Claude
  kit installs, as **project skills**. They sit exactly one level under
  `.kiro/skills/` because that is the only depth Kiro's project-skill walk
  reaches. The bodies are written unchanged — the frontmatter already carries the
  `name` and `description` a project skill needs — so there is one copy of the
  skill text for every surface, with nothing to drift.
- **`.kiro/agents/strata.json`**: a **project agent spec** — a workspace-local
  custom agent, resolved before your `~/.kiro/agents/` ones. Select it with the
  agent picker, or `kiro-cli --agent strata`. It declares the `strata` MCP server
  on the spec itself (with `includeMcpJson: true`, so your workspace and user
  `mcp.json` servers are kept as well — this adds the engine's server without
  narrowing what you had), points `resources` at the steering block and the
  project skills, and carries three hooks.

  It pins no `model` and narrows no `tools`: a committed file must not override
  your own choice for every session in the repo.

### The project agent's three hooks

| Event                              | What it does                                                                                                   |
| ---------------------------------- | -------------------------------------------------------------------------------------------------------------- |
| `agentSpawn`                       | prints the one-line "no index yet" reminder when the repo has `.strata/` but no graph, and is silent otherwise |
| `postToolUse` (matcher `fs_write`) | after a write, backgrounds an incremental `strata index` so the graph keeps up                                 |
| `stop`                             | when a turn ends, names the `detect_changes` pre-commit check                                                  |

The `stop` hook closes a real gap. As noted above, Kiro has no commit-specific
trigger, so the "run `detect_changes` before you commit" discipline has had to be
a rule the agent remembers. `stop` fires when a turn ends, which is the closest
mechanical hook to "before you commit" the surface offers — still advisory, but
no longer purely a matter of memory.

There is deliberately **no `preToolUse` hook**. That event _fails closed_ in Kiro
Crew: exit `2` denies the tool call, and so does any other non-zero exit, a
timeout, or a crash. The engine's edit-time advice is advisory — it informs, it
never blocks — so putting it on an event where a transient failure would start
blocking edits would be the wrong trade. The pre-edit blast check therefore stays
where it already works: the standalone `strata-pre-edit` hook and, for Claude
Code, the `PreToolUse` hook described below.

Three further details, each of which would otherwise leave a hook that silently
never fires:

- **The matcher is a glob, not a regex.** Kiro Crew matches tool events with
  filename-style globbing, so the standalone hooks'
  `fs_write|str_replace|fs_append` alternation would match no tool here. The bare
  name `fs_write` is both correct and sufficient — Crew resolves `str_replace`,
  `fs_append` and `delete_file` onto that same row.
- **The guards are relative to the working directory.** A Crew hook subprocess
  gets a strict environment allowlist with no project-directory variable in it,
  unlike the `$CLAUDE_PROJECT_DIR` the Claude hooks use. Crew spawns the hook in
  the session workspace, so each command tests `./.strata` instead.
- **Every command is doubly guarded and always exits 0.** No `.strata` directory,
  or no `strata` on `PATH`, means silence rather than a per-turn error.

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

| File                      | Location  |
| ------------------------- | --------- |
| `.mcp.json`               | Repo root |
| `CLAUDE.md`, `AGENTS.md`  | Repo root |
| `.claude/skills/strata/…` | Repo root |
| `.claude/settings.json`   | Repo root |

These files are committed and shared with the team. The steering block carries
a per-repo identity line naming this repo's node count and planes.

**Global scope** (`--global` / `--scope user`) writes into your home directory:

| File                        | Location                                                          |
| --------------------------- | ----------------------------------------------------------------- |
| MCP server registration     | `~/.claude.json` (managed by `claude mcp add`, never hand-edited) |
| `~/.claude/settings.json`   | Hooks (merged in, same merge-safe writer)                         |
| `~/.claude/skills/strata/…` | Skills (same four SKILL.md files)                                 |
| `~/.claude/CLAUDE.md`       | Generic steering block (no per-repo identity line)                |

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
