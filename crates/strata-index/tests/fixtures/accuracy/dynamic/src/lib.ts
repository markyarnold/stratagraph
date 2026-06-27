// Free functions consumed through a namespace import, plus an OVERLOADED
// function (two signatures, one implementation). Namespace-qualified bare calls
// `NS.fn()` to free functions are the case the method-only unknown-receiver rule
// cannot see (recall miss); SCIP resolves them to the free function here. The
// overloaded `parse` resolves to its single implementation regardless of which
// signature a call matches.

export function load(): number {
  return 1;
}

export function store(n: number): void {
  void n;
}

// Overloaded signatures with one implementation. Both call shapes resolve to the
// same implementation symbol — SCIP confirms a single target.
export function parse(input: string): number;
export function parse(input: number): string;
export function parse(input: string | number): number | string {
  return typeof input === "string" ? input.length : String(input);
}
