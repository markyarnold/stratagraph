# Upgrading

Three things can go stale when the StrataGraph binary moves forward: the
**graph** each repo carries, the **agent kits** installed into your editors,
and the **desktop app**. Upgrading is the same short sequence every time, in
this order. (Each [changelog](../project/changelog.md) entry also states the
specific action it needs, if any.)

## 1. Upgrade the binary

Building from source:

```bash
git pull && cargo build --release -p strata-cli
```

(or `cargo install --path crates/strata-cli`). Verify what you are now
running:

```bash
strata --version
```

This prints the version **and the engine id** (a build hash). The same engine
id appears on the `engine:` line of every `strata index` summary — that pair
is your version-skew detector: if they differ, something is still running the
old build (commonly a long-lived MCP server started before the upgrade; a
session restart fixes it).

## 2. Reindex each repo

```bash
strata index .          # or: strata index --workspace <estate.toml>
```

There are **no migrations, ever**: the graph is rebuilt from source truth, and
an analyzer schema-version bump invalidates stale parse caches automatically.
New planes and capabilities light up on the first reindex — for example, the
knowledge plane appears as a `knowledge:` line in the summary and writes the
docs search index (`.strata/docs.idx`). Until a repo is reindexed, new
features honestly degrade (tools report "no docs index — run `strata index`"
rather than guessing).

The MCP server hot-reloads the graph when the on-disk index changes, so a
reindex needs no session restart. The one exception is the binary itself: a
server launched before the upgrade is still the old build until the editor
session restarts.

## 3. Re-run `strata init` in every kitted repo

The steering blocks, skills, and hook files that `strata init` installs are
**written text — they do not update themselves** when the binary does. In
each repo with a kit:

```bash
strata init claude      # and/or: strata init kiro
```

And if you use the user-scope global kit:

```bash
strata init claude --global
```

Re-running is safe by design and is the *single* upgrade step for a kit:

- **Idempotent and merge-safe.** Managed blocks (`<!-- strata:begin/end -->`)
  are replaced; everything else in `CLAUDE.md`/`AGENTS.md`/steering — including
  other tools' blocks — is preserved byte-for-byte (test-pinned).
- **Hook files are upgraded too.** Claude Code's hook commands are stable
  (they just invoke `strata`, so new blast output such as the `docs:` line
  flows through automatically) — but Kiro's hook *files* can change between
  releases: the 0.2.0 → next transition retired the pre-commit hook and
  auto-detects Kiro's hook format, and the re-run applies exactly that,
  removing retired files.
- **Steering and skills catch up.** A kit installed under an older release
  will not mention newer tools (for example `guidance`/`search_docs`) or newer
  rules (the doc-kind "needs review" verdict; the conditional `docs:`-line
  guidance trigger). The re-run brings the installed text in line with the
  binary you now run.

Editors load hooks and the MCP server **at session start** — restart the
Claude Code / Kiro session in each repo after re-running `init`.

## 4. Rebuild the desktop app

The desktop app bundles its own engine, so it upgrades by rebuilding: see
[The desktop app](desktop.md) for the build command, then replace the
installed `.app`.

## How to tell something is stale

| Symptom | Meaning | Fix |
| --- | --- | --- |
| `strata --version` engine id ≠ the `engine:` line in a fresh `strata index` run | An old binary is still being served somewhere | Restart the editor session / rebuild |
| The index summary is missing a plane line the changelog announced (e.g. no `knowledge:` line) | The repo has not been reindexed since the upgrade | `strata index .` |
| The steering block does not mention a tool the changelog announced (e.g. `guidance`, `search_docs`) | The kit predates the binary | `strata init claude` / `strata init kiro` (+ `--global` if used) |
| The desktop app behaves like the previous release | The bundled engine is the old build | Rebuild the app |
