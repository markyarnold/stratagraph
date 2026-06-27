// The real implementations. These are re-exported through a barrel chain
// (core.ts -> index.ts) and consumed in consumer.ts. The heuristic binds a call
// on the *local* imported name; when that name is reached through a re-export
// hop it has no local/import candidate (recall miss), but SCIP follows the
// chain to the original symbol — so each is a SCIP-adjudicable site.

export function alpha(): number {
  return 1;
}

export function beta(): number {
  return 2;
}

// A default export — consumed via a default import (a different binding form
// the heuristic's name match must still handle; SCIP resolves it to here).
export default function gamma(): number {
  return 3;
}
