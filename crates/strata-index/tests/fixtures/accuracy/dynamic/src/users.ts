// Namespace import of free functions + overload calls + typed-receiver method
// calls (AMBIGUOUS over-inclusion SCIP narrows) + an `any`-typed dynamic call
// (genuinely unadjudicable — SCIP emits no first-party ground-truth edge).
import * as NS from "./lib";
import { Rect, Disc } from "./shapes";

// `NS.load()` / `NS.store()` — namespace-qualified calls to FREE functions. The
// method-only unknown-receiver rule cannot see free functions (miss); SCIP
// resolves each through the namespace import to lib.ts.
export function viaNamespace(): number {
  NS.store(5);
  return NS.load();
}

// Overload calls: both signatures resolve to the single `parse` implementation.
export function viaOverload(): number {
  const a = NS.parse("abc");
  const b = NS.parse(7);
  return a + b.length;
}

// Typed-receiver calls on classes sharing `area`/`name`. The unknown-receiver
// heuristic over-includes {Rect.*, Disc.*}; SCIP narrows to the receiver type.
export function areas(): number {
  const r = new Rect();
  const d = new Disc();
  return r.area() + d.area();
}

export function names(): string {
  const r = new Rect();
  const d = new Disc();
  return r.name() + d.name();
}

// An `any`-typed receiver: dynamic property-access call. The heuristic still
// over-includes every same-named method repo-wide, but SCIP cannot resolve a
// call on `any` to a first-party target — an UNADJUDICABLE site, surfaced and
// excluded from precision, never silently scored.
export function dynamic(thing: any): number {
  return thing.area();
}
