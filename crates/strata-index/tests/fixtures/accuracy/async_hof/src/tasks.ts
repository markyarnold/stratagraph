// Async/await + Promise-chain call sites. Each callee is a free async function
// with a unique name, so the bare-call heuristic emits exactly one candidate and
// SCIP confirms it — BareSingle hits through `await` and `.then()` positions.

export async function fetchValue(): Promise<number> {
  return 41;
}

export async function increment(n: number): Promise<number> {
  return n + 1;
}

// `await fetchValue()` and `await increment(...)` are bare calls to unique free
// functions — heuristic single-candidate, SCIP confirms.
export async function pipeline(): Promise<number> {
  const v = await fetchValue();
  return await increment(v);
}

// A Promise `.then()` chain. The bare `fetchValue()` is the BareSingle hit; the
// `.then(cb)` callback is passed by reference (not a call site here). `transform`
// is a method on the chained Promise — a typed receiver SCIP resolves.
export function chained(): Promise<number> {
  return fetchValue().then((v) => v * 2);
}
