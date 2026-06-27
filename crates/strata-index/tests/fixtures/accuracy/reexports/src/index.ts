// Second re-export hop: the package barrel. A star re-export of the core barrel
// (which itself re-exports impls) — a two-level chain — plus the default lifted
// to a named export. Consumers import everything from here.
export * from "./core";
export { default as gamma } from "./impls";
