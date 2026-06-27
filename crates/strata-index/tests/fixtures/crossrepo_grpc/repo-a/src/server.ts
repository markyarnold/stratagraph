// Minimal source so the repo indexes to a real code graph. No producer/consumer
// code linking is attempted for gRPC at M1 (that is M2) — this only ensures the
// repo has a graph store for the estate pass.
export function start(): void {
  console.log("repo-a order service");
}
