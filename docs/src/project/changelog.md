# Changelog

User-visible changes, newest first, grouped by release. Between releases,
changes merged to `main` appear under **Unreleased**. Each entry names the
change the way you would meet it — a command, a report line, a recovered file.
(The engine build behind any answer is always visible: `strata --version` and
the `engine:` line of every index summary print the exact build id.)

## 0.2.0 — 2026-07-12

**Data plane: ClickHouse support.** `.sql` files are no longer Postgres-only.
A recovery ladder handles ClickHouse DDL: the ClickHouse dialect as a fallback,
then a column-list recovery that strips ClickHouse-only decoration
(`ENGINE`/`PARTITION BY`/`TTL`/`SETTINGS` tails; `CODEC`/`ALIAS`/`MATERIALIZED`
column modifiers; inline `INDEX`/`PROJECTION` entries; aggregate-state type
parameters) — every recovery re-validated by the parser, so the declared column
set is exact and nothing is guessed. RBAC and maintenance statements, and
clone/CTAS tables whose shape lives elsewhere, are recognized and skipped with
counts instead of failing their file. On a real ClickHouse-heavy repository this
took failed schema files from 54 to 3 and more than doubled the recovered
tables.

**`detect-changes`: breaking/additive labels on contract changes.** A removed or
modified operation key is tagged **`[BREAKING]`** (it breaks consumers) and
escalates the risk to CRITICAL with an explicit reason; an added key is tagged
**`[additive]`** and no longer escalates by itself — new surface has no existing
consumers. The MCP result carries the same label as a `contract_change` field.

**Within-repo collision honesty.** When two spec files in ONE repository declare
the same operation key, a consumer of that key now fans out with one `Ambiguous`
edge per owning spec — exactly like the estate rule across repositories — instead
of being silently bound to whichever spec sorted first.

**Estate manifests: duplicate paths rejected.** Two `[[repos]]` entries naming
one directory (including lexical aliases like `svc` and `./svc`) would overwrite
each other's graph and estate marker; the manifest now fails to parse with a
clear error instead.

**Data plane: `UPDATE … FROM` reads.** The FROM sources of an
`UPDATE t … FROM s` (joins included) are now recorded as Reads alongside the
target's Write.

**Contract consumers: more HTTP clients.** TypeScript/JavaScript now recognises
`got`, `ky` and `superagent` receivers plus the `got(url)`/`ky(url)` bare-call
forms (with `fetch`/`axios` as before; `node-fetch` is imported as `fetch` and
was already covered). Python adds `aiohttp`'s direct module forms. The
known-receiver-only rule is unchanged: a client the analyzer cannot identify is
skipped, never guessed.

**Kiro agent kit: hooks rescoped, loop fixed.** The pre-commit hook previously
fired on every shell command (a mis-matched tool name fell back to
always-match) and could intercept the very `detect_changes` run it requested.
It now targets the real shell tool and its prompt is applicability-gated —
non-commit commands proceed immediately and silently, and strata's own
invocations are exempt, so the loop cannot recur. The post-commit reindex is
replaced by a **post-edit** reindex riding the write tools; re-running
`strata init kiro` upgrades the hook files and removes the retired one.

**Desktop.** The tool dispatch now carries the opened project's root, so
`detect_changes` and `rename` work from the app the way they do from the CLI
(views for them arrive with the UI overhaul).

## 0.1.0 — 2026-06-28 — initial public release

The first public, source-available release (FSL-1.1-ALv2; each release converts
to Apache 2.0 two years after it ships): the cross-plane graph over code,
contracts, infrastructure and data; five languages (TypeScript, JavaScript,
Python, Rust, C#); OpenAPI, GraphQL and gRPC contracts; the AWS infrastructure
vertical (SAM/CloudFormation, Terraform, Terragrunt) with IAM grants; the SQL
data plane; calibrated per-edge confidence with measured accuracy reports; the
`strata` CLI, the MCP server with hot reload, the desktop app, and the
one-command agent kits for Claude Code and Kiro.

The launch build includes the **estate-aware agent kit** (indexing an estate
writes a membership marker into each member repo, so `blast`, `detect-changes`,
`rename` and the MCP server auto-resolve the estate from any member) and the
**global agent-kit install** (`strata init claude --global` installs the Claude
kit once into `~/.claude`, applying to every repository you open).
