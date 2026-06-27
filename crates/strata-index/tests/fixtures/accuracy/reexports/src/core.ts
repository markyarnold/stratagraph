// First re-export hop: a barrel that re-exports the named impls and renames one.
// `delta` is `beta` under a new exported name — the classic alias-through-barrel
// the heuristic cannot follow (it binds on the local call name), SCIP can.
export { alpha } from "./impls";
export { beta as delta } from "./impls";
