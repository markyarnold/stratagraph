// Higher-order calls: callbacks passed in and invoked. The invocation `fn(x)` is
// a bare call to a PARAMETER, not a named function — the heuristic finds no
// local/imported definition (recall miss with no edge); SCIP also treats a
// parameter call as having no first-party target (unadjudicable), so these
// exercise the no-candidate / unadjudicable path honestly. The NAMED functions
// passed as arguments and called directly resolve cleanly.

export function double(n: number): number {
  return n * 2;
}

export function triple(n: number): number {
  return n * 3;
}

// `fn` is a callback parameter; `fn(x)` is a bare call to a parameter (no
// first-party definition). The heuristic emits no edge; SCIP has no first-party
// target — an unadjudicable site, surfaced not assumed.
export function applyOnce(fn: (n: number) => number, x: number): number {
  return fn(x);
}

// `map`-style HOF: the callback parameter `f` invoked per element.
export function applyAll(f: (n: number) => number, xs: number[]): number[] {
  return xs.map((x) => f(x));
}

// Pass NAMED free functions as callbacks AND call a named one directly. The
// direct `double(2)` is a BareSingle hit; `applyOnce`/`applyAll` are bare calls
// to unique local functions — also hits.
export function run(): number[] {
  const a = applyOnce(double, 2);
  const b = applyAll(triple, [1, 2]);
  return [double(2), a, ...b];
}
