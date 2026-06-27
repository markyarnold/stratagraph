// @ts-nocheck — this file intentionally has a redeclaration: a bare call with
// BOTH a local definition and an imported one of the same name. That is the
// over-inclusion the heuristic's BareMulti branch handles (it emits edges to
// both candidates); scip-typescript still indexes the occurrences and resolves
// the call to the in-scope (local) definition, so it is a covered BareMulti site
// with precision 0.5 (tp=1 local, fp=1 import). Real codebases contain such
// shadowing; the metric must score it.
import { helper } from "./util";

function helper(): number {
  return 2;
}

export function useHelper(): number {
  return helper();
}
