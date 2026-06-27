# Confidence and provenance

This is the most important page in the manual. Everything StrataGraph reports carries
a record of **how it was derived** (provenance) and **how much to trust it**
(confidence), and the product's central discipline is that *an inference can never
masquerade as a fact*. A tool that confidently tells you "nothing depends on this"
when something does is worse than useless: it is dangerous. StrataGraph is built so
that when it is unsure, it **says so**, and when it would have to guess, it skips
instead. This page defines the provenance tags, the confidence bands, the
**WILL BREAK** label, and the trust policy you (and your agent) should apply to
every result.

## Provenance: how a relationship was derived

Every edge and every derived node carries one of these provenance tags
(`strata-core`'s `Provenance`; design doc §4.1):

| Provenance | Meaning | Typical confidence |
| --- | --- | --- |
| `Extracted` | Read directly from a deterministic source: an AST node, a spec file, SQL DDL, a resolved plan. | 0.95 – 1.0 |
| `Resolved` | Derived by a compiler or language server with full symbol resolution (SCIP). | 0.9 – 1.0 |
| `Observed` | Seen in runtime data (query logs, traces, CloudTrail). *Reserved, not emitted yet.* | 0.9 – 1.0 |
| `Inferred` | Derived heuristically: a naming convention, a framework pattern, a string-built SQL match. | 0.4 – 0.8 |
| `Ambiguous` | Several candidate targets, none confidently selected. | below 0.4 |
| `Model` | Produced by an LLM/vision pass (the future [knowledge plane](planes.md)). | tagged separately, **never gates impact** |

Provenance is the *kind* of evidence; confidence is its *strength*. The two are
linked by a rule that the whole system enforces.

## The bands, and why each is capped

Confidence is not a vibe. Each provenance maps to a **band**, and a stored
confidence is `min(measured precision, the band's ceiling)`. Calibration can move
a number *within* its band; it can never break the band. This is the rule that
guarantees an inference cannot impersonate a fact:

| Band | Range | What lands here |
| --- | --- | --- |
| `Resolved` (compiler-grade) | ~0.97 | SCIP-resolved call/symbol edges (TypeScript today). |
| `Extracted` | 0.95 – 1.0 | Structural facts: a `Defines` edge, a spec operation node, a `CREATE TABLE`, a same-file binding, a `Resource`-graded infra reference. |
| `Inferred` | 0.40 – 0.80 | Confident heuristics: a convention-matched producer, an interpolation-recovered infra ref, a `this.`/`self.` method, a unique repo-wide name. |
| `Ambiguous` | below 0.40 | "Could be any of these": fan-outs over several same-named candidates, unknown-receiver calls. |

> **Why the Inferred band reads `0.40–0.80` here but `0.40–0.89` in the trust
> policy.** `0.80` is the *emit ceiling*: the highest confidence the linkers
> ever store on an Inferred edge (a guess can never reach the Extracted floor).
> The [trust policy](#the-trust-policy) below buckets all sub-`0.90` evidence
> together as `0.40–0.89` because that is the band a *reader* applies ("verify
> before relying on it") regardless of exactly where in `[0.40, 0.80]` the stored
> number landed. Both numbers describe the same band from two angles: what the
> engine emits, and how you should treat it.

The cap is load-bearing in two directions:

- **A heuristic edge is capped *down* to its ceiling even when it measured
  perfectly.** In the TypeScript linker, a bare single-candidate call
  (`CONF_BARE_SINGLE`) measured **1.00** precision against SCIP, but it is stored
  at **0.80**, the Inferred ceiling, because a measured 1.00 would let a guess
  outrank or equal a `Resolved` (0.97) or `Extracted` (1.0) fact. An
  unknown-receiver call (`CONF_UNKNOWN_RECEIVER`) measured 0.50, above the
  Ambiguous ceiling, so it is capped to **0.39**.
- **A same-file binding is `0.95`, not `1.0`.** Even the strongest static signal
  the heuristic has (`CONF_SAME_MODULE`/`CONF_SAME_FILE`) sits at the Extracted
  *floor*, never the top: it must never outrank a true compiler-resolved fact,
  and it leaves headroom for the theoretical miss (a re-binding the heuristic
  cannot see).

The real constants, straight from the linkers, make the discipline concrete:

| Constant | Plane / language | Band | Value | Note |
| --- | --- | --- | --- | --- |
| `RESOLVED_CONFIDENCE` | code (TS, SCIP) | Resolved | 0.97 | A SCIP hit supersedes the heuristic edge. |
| `CONF_SAME_MODULE` / `CONF_SAME_FILE` | code (py/rust/cs) | Extracted | 0.95 | Same-file binding: band floor, not 1.0. |
| `CONF_BARE_SINGLE` (TS) | code | Inferred | 0.80 | Measured 1.00, **capped** to the ceiling. |
| `CONF_THIS_METHOD` / `CONF_SELF_METHOD` | code | Inferred | 0.80 | `this.`/`self.` to an enclosing-type method. |
| `CONF_IMPORT_MATCHED` / `CONF_*_UNIQUE` / `CONF_TYPE_QUALIFIED` | code | Inferred | 0.80 | Import-matched, unique repo-wide, or `Type::method()`. |
| `CONF_UNKNOWN_RECEIVER` (TS) | code | Ambiguous | 0.39 | Measured 0.50, **capped** below 0.40. |
| `CONF_BARE_MULTI` / `CONF_AMBIGUOUS` | code | Ambiguous | 0.35 | Fan-out over several candidates. |
| `CONF_PRODUCES_SINGLE` | contract | Inferred | 0.80 | Convention-matched producer (one match). |
| `CONF_PRODUCES_MULTI` | contract | Ambiguous | 0.35 | Several candidate operations. |
| `CONF_REF_RESOURCE` | infra | Extracted | 0.95 | Same-template `Ref`/`GetAtt`, a fact. |
| `CONF_REF_INFERRED` | infra | Inferred | 0.70 | `Sub`/`Join`/`Fn::If`-recovered id. |
| `CONF_RUNS` | infra | Extracted | 0.95 | Lambda→handler module, unique match. |
| `CONF_DATA_FACT` / `CONF_ORM_EXPLICIT` | data | Extracted | 0.95 | DDL, FK, raw-SQL match, explicit-name ORM. |

> These are doc-commented constants in `strata-index/src/build.rs` (TS),
> `strata-lang-{py,rust,cs}/src/link.rs`, and
> `strata-index/src/{contract,data,infra}.rs`. A band-invariant test in each
> crate fails the build if any edge ever stores a confidence outside its
> provenance band, so the discipline cannot silently rot. The Inferred numbers
> are *calibrated* against measured precision; see
> [How accuracy is measured](../accuracy/methodology.md) and the
> `docs/accuracy/*-resolution.md` reports.

## The never-confident-wrong discipline

The bands are the mechanism; the discipline is the intent. Three rules follow
from "an inference can never masquerade as a fact", and they show up everywhere in
the code:

- **Skip over guess.** When the evidence does not support a confident pick, StrataGraph
  fans out at the Ambiguous band, or emits nothing at all. A Python
  `getattr(...)()`, a Rust macro, a C# reflective `Invoke`, a JS computed member
  access: the analyzer drops the callee rather than inventing a target.
- **Never invent an endpoint.** Cross-plane links are only emitted when both ends
  exist. A foreign key, a `Reads`/`Writes`, or an ORM `MapsTo` whose table is not
  in the parsed DDL produces **no edge**: it is counted as unresolved and
  surfaced, never pointed at a phantom node. The same holds for an infra reference
  to a parameter or cross-stack import.
- **Surface the miss.** What StrataGraph could not resolve is *reported* (the coverage
  counts in every accuracy report), not hidden. An absence is information: a
  producer with no matching operation tells you the route implements something the
  spec does not declare.

A direct consequence you should rely on: a contract field or operation with
**0 producers and 0 consumers** is probably **dead surface**: call it out rather
than treating it as live. (See the guide [Is this schema field dead?](../guides/dead-surface.md).)

## The WILL BREAK label

When `impact` reports a dependent, it stamps each one with a verdict:
**`WILL BREAK`** or **"may be affected, review"**. The rule is exact and lives in
one place (`strata-core`'s `will_break_label`):

> A dependent is **WILL BREAK** if and only if the best reaching path's
> accumulated confidence is **≥ 0.40** *and* the path is **not ambiguous**.
> Otherwise it is "may be affected, review".

"may be affected, review" is the canonical label; the CLI's compact impact table
renders it as the short form **"may affect"** in the `verdict` column. They are the
same verdict, the not-a-certain-break label, abbreviated to fit the row.

Three properties of this label matter:

- **The threshold is `0.40`, and it was measured, not chosen.** It is
  `DEFAULT_WILL_BREAK_CONFIDENCE`, the lowest design-§4.1 band whose empirical
  precision crosses the will-break bar. Against SCIP, the `Inferred` band measured
  **1.00** precision; the `Ambiguous` band measured **0.53**, too noisy to call a
  break. So the boundary sits at the Inferred floor, 0.40. It is re-derived as the
  corpus grows.
- **Ambiguity is excluded by *provenance*, not by a numeric race.** An ambiguous
  path is never WILL BREAK regardless of its confidence: the `!ambiguous` guard
  drops it. (The bands cooperate: every Ambiguous edge is also capped below 0.40,
  so it would fail the numeric test anyway, but the provenance guard is the
  primary reason.)
- **It is depth-independent.** WILL BREAK is about the *quality of the evidence*
  along the path, not how many hops away the dependent is. A confident
  `Extracted`/`Resolved` chain five hops out is still WILL BREAK; a one-hop
  Ambiguous edge is not. The accumulated confidence is the multiplicative product
  of every edge on the best path, so a long chain of strong edges can stay above
  0.40, and a single weak edge can sink it.

The label is a **classification, never a filter**. `impact` is recall-biased by
default (`min_confidence = 0.0`): it surfaces *everything*, including Ambiguous
paths, and marks them. You see the full blast radius; the label tells you which
parts to trust. (You can pass a hard `min_confidence` to filter if you want, but
that is opt-in.)

You can see all of this in one command. On this repository:

```text
$ strata impact will_break_label
Impact of will_break_label (crates/strata-core/src/traverse.rs) — 161 affected:
  depth  conf  amb  verdict     name (path)
      1  0.95   no  WILL BREAK  impact (crates/strata-core/src/traverse.rs)
      2  0.90   no  WILL BREAK  members_with_dependents (...)
      ...
```

Each row carries the depth, the accumulated `conf`, the ambiguity flag, and the
verdict derived from them. `explain <target> <affected>` unfolds the evidence
chain behind any single row: every edge, its kind, its provenance, its
confidence, and the running product that produces impact's number. That command
is the visible form of never-confident-wrong: it shows you *why* a dependent is in
the blast radius, so the verdict is auditable rather than asserted.

## The trust policy

Apply this policy to every confidence you see: it is the same one the agent kit
encodes for AI agents, and it is the right one for a human reviewer too:

| Confidence | Provenance | Policy |
| --- | --- | --- |
| **≥ 0.90** | `Extracted` / `Resolved` | **Act on it.** This is a fact or a compiler-grade resolution. |
| **0.40 – 0.89** | `Inferred` | **Verify before relying on it.** A confident heuristic: check the source to confirm. Still labelled WILL BREAK, so it counts as a break, but it earns a look. |
| **< 0.40, or `ambiguous: true`** | `Ambiguous` | **Treat as UNKNOWN. Say so explicitly.** Never present an uncertain impact as certain. It is surfaced so you do not miss it, not so you can trust it. |

The discipline cuts both ways. A WILL BREAK at 0.95 is something you should act
on before you change the target. An Ambiguous result at 0.35 is a lead to
investigate, reported honestly as a lead, and the one thing you must never do is
launder it into a confident claim. StrataGraph gives you the grade so you do not have
to guess how much to trust the answer; the trust policy is how you spend it.
