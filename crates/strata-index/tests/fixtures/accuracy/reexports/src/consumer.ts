// Calls through the two-level re-export barrel (index.ts -> core.ts -> impls.ts)
// plus a default import and a namespace import. Every callee is a free function
// SCIP resolves to its ORIGINAL definition in impls.ts, no matter how many
// re-export hops the name took — so these are SCIP-adjudicable bare-call sites.
import { alpha, delta, gamma } from "./index";
import defaultGamma from "./impls";
import * as pkg from "./index";

// Bare calls on names imported through the barrel. `alpha` survives a plain
// re-export; `delta` is `beta` renamed at the first hop; `gamma` is the default
// lifted to a name at the second hop. The heuristic binds on the local name and
// finds no definition in the *barrel* module (it only re-exports); SCIP follows
// the chain to impls.ts.
export function viaBarrel(): number {
  return alpha() + delta() + gamma();
}

// A default import bound to a local name `defaultGamma`. SCIP resolves it to the
// default export (gamma) in impls.ts.
export function viaDefault(): number {
  return defaultGamma();
}

// Namespace-qualified calls `pkg.alpha()` — a property access on a namespace
// import of the barrel. SCIP resolves each through the re-export to impls.ts.
export function viaNamespace(): number {
  return pkg.alpha() + pkg.delta();
}
