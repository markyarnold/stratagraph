# Your first queries

With a graph built, you can interrogate it. This is a guided tour of the four
read commands against a freshly-indexed repository (StrataGraph's own repository, so
you can reproduce every command here). The pattern is always the same: **`query`
to find the symbol, then `context`, `impact`, or `explain` to learn about it.**

Run these from the repo root (the default `--db .strata/graph.duckdb` is picked up
automatically).

## `query`: find a symbol

`query` is a case-insensitive lexical search over each node's name,
fully-qualified name, and path. Use it first, to get the exact symbol (and its
uid) before drilling in.

```bash
strata query cmd_impact
```

```text
8 match(es) for "cmd_impact":
  cmd_impact_workspace [Function] crates/strata-cli/src/lib.rs
    rust|strata|crates/strata-cli/src/lib.rs|cmd_impact_workspace|
  cmd_impact [Function] crates/strata-cli/src/lib.rs
    rust|strata|crates/strata-cli/src/lib.rs|cmd_impact|
  ...

Next: `strata context <name>` for relationships, `strata impact <name>` for blast radius.
```

Each hit shows the name, the node **kind** (`Function`, `Class`, `Method`,
`Table`, `GraphqlField`, …), the file, and on the second line the node's **uid**:
the stable, fully-qualified identity. The uid matters when a plain name is
ambiguous: the impact/context/explain commands accept `--uid` to pin exactly the
node you mean.

## `context`: the 360° view

`context` shows everything StrataGraph knows *about* one symbol: who calls it, what it
calls, its imports, its members and container, and (when the symbol lives on the
contract / infra / data planes) who produces it, who consumes it, and what it
maps to.

```bash
strata context cmd_blast
```

```text
Context for cmd_blast (Function) — crates/strata-cli/src/lib.rs
  uid: rust|strata|crates/strata-cli/src/lib.rs|cmd_blast|
  container: lib.rs (crates/strata-cli/src/lib.rs)
  producers (0):
  consumers (0):
  produces (0):
  consumes (0):
  assumes (0):
  assumed_by (0):
  routes_to (0):
  routed_from (0):
  runs (0):
  run_by (0):
  mapped_by (0):
  maps_to (0):
  callers (1):
    - main (crates/strata-cli/src/main.rs)
  callees (6):
    - blast_rel_path (crates/strata-cli/src/lib.rs)
    - load_existing_graph (crates/strata-cli/src/lib.rs)
    - render_blast_agent (crates/strata-cli/src/lib.rs)
    - render_blast_text (crates/strata-cli/src/lib.rs)
    - repo_root_from_db (crates/strata-cli/src/lib.rs)
    - blast_for_file (crates/strata-index/src/changes.rs)
  imports_in (0):
  imports_out (0):
  members (0):
```

Every bucket is always printed, with its count, so an empty bucket reads `(0)`
rather than vanishing. That is deliberate: for a contract field, `producers (0)`
and `consumers (0)` together is the signal that the field is **probably dead**.
For a plain code function like `cmd_blast`, the contract/infra/data buckets are
all `(0)` (it is not on those planes) and the interesting buckets are `callers`
and `callees`.

The buckets, by plane:

- **Code:** `callers`, `callees`, `imports_in`, `imports_out`, `members`,
  `container`.
- **Contract:** `producers` (who implements this field/operation), `consumers`
  (who queries it), and the reverse `produces` / `consumes` for a code symbol.
- **Infra:** `assumes` / `assumed_by` (IAM), `routes_to` / `routed_from`,
  `runs` / `run_by` (a Lambda and its handler).
- **Data:** `maps_to` (an ORM model → its table) and `mapped_by` (a table → the
  models that map to it).

See [Plane walkthroughs](../guides/plane-walkthroughs.md) for worked examples on
each plane.

## `impact`: the blast radius

`impact` is the reverse blast radius: everything that depends on a symbol, so you
know what a change to it could break. It is contract- and infra-aware by default
(it follows producer → operation → consumer across the contract plane and the
infra edges), so cross-plane and cross-repo dependents show up too.

```bash
strata impact classify_risk
```

```text
Impact of classify_risk (crates/strata-index/src/changes.rs) — 76 affected:
  depth  conf  amb  verdict     name (path)
      1  0.95   no  WILL BREAK  detect_changes (crates/strata-index/src/changes.rs)
      1  0.95   no  WILL BREAK  blast_for_file (crates/strata-index/src/changes.rs)
      2  0.76   no  WILL BREAK  tool_blast (crates/strata-mcp/src/tools.rs)
      4  0.69   no  WILL BREAK  call_tool (crates/strata-mcp/src/tools.rs)
      ...
```

How to read each column:

- **depth**: how many hops away the dependent is. `d=1` is a direct dependent,
  `d=2` indirect, and so on. Depth is *distance*, nothing more.
- **conf**: the calibrated confidence of the weakest edge on the path to that
  dependent. It is the number you trust the result by.
- **amb**: `yes` when the path runs through a candidate StrataGraph could not
  uniquely resolve.
- **verdict**: `WILL BREAK` or `may affect`. This is a **per-row** call, not a
  function of depth: a row is `WILL BREAK` only when its `conf ≥ 0.40` **and** it
  is not ambiguous. A `d=1` row that is ambiguous or below 0.40 is `may affect`,
  never a certain break.

### Reading confidence and ambiguity

Treat confidence as a trust policy:

| Confidence | What to do |
|---|---|
| ≥ 0.90 | Act on it. |
| 0.40 – 0.89 | Verify in the source before relying on it. |
| < 0.40 or ambiguous | Treat as **UNKNOWN**: say so; do not present it as certain. |

This is the core of StrataGraph's "never confident-wrong" posture: the difference
between a 0.95 and a 0.50 edge is measured, not decorative. The full bands and how
they are calibrated live in [Confidence and provenance](../concepts/confidence.md).

If `impact` reports `0 affected` on a *type* whose methods do have dependents, it
will not say a bare "nothing depends on this"; it tells you which members carry
the dependents and gives you a runnable next step, because claiming nothing
depends on something that is actually used would be exactly the kind of confident
error StrataGraph refuses to make.

## `explain`: the evidence chain

When you want to know *why* a symbol is in the blast radius, `explain` shows the
path: every edge, its kind and provenance, its own confidence, and the running
(accumulated) confidence that produced `impact`'s number.

```bash
strata explain classify_risk call_tool
```

```text
Why classify_risk affects call_tool (conf 0.69, WILL BREAK):
  classify_risk     —CALLS (Extracted 0.95)→  blast_for_file        running 0.95
  blast_for_file    —CALLS (Inferred  0.80)→  tool_blast            running 0.76
  tool_blast        —CALLS (Extracted 0.95)→  call_tool_ctx         running 0.72
  call_tool_ctx     —CALLS (Extracted 0.95)→  call_tool             running 0.69
```

Each hop names the edge kind (`CALLS`, `PRODUCES`, `CONSUMES`, …), its provenance
(`Extracted`, `Inferred`, `Resolved`, `Ambiguous`) with that edge's confidence,
and the running confidence after the hop. The header's overall confidence equals
the number `impact` printed for that row; `explain` runs the same reverse walk,
so the two never disagree. This is the visible form of never-confident-wrong: you
can audit exactly where a result's confidence came from.

If the affected symbol is *not* in the target's blast radius, `explain` says so
plainly ("… is not in …'s blast radius (nothing to explain)") rather than
returning an empty success.

## Disambiguating

When a name resolves to several nodes, `impact` / `context` / `explain` list the
candidates with their uids and ask you to pick one. Re-run with `--uid`:

```bash
strata impact MyType --uid 'rust|strata|crates/foo/src/lib.rs|MyType|'
```

`explain` additionally takes `--affected-uid` to pin the *affected* end when it,
too, is ambiguous.

## Where to go next

- Full workflows (what breaks if I change this, is this field dead, rename
  safely, pre-commit checks) are in the [Guides](../guides/impact.md).
- The confidence bands and provenance kinds are explained in
  [Confidence and provenance](../concepts/confidence.md).
- The complete command and flag reference is in the [CLI reference](../reference/cli.md).
- To put this blast-radius check inside your AI editor, see
  [Wire up your editor](agent-kit.md).
