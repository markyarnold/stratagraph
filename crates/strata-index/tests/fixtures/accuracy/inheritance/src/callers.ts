// Polymorphic typed-receiver calls across the hierarchy. `speak` is defined on
// Animal and overridden on Dog — two same-named methods repo-wide — so the
// unknown-receiver heuristic (no type info on a local identifier) over-includes
// BOTH; SCIP, knowing each receiver's concrete type, narrows to exactly one.
import { Animal, Dog, Puppy } from "./hierarchy";

export function speakAll(): string {
  const a = new Animal();
  const d = new Dog();
  const p = new Puppy();
  // Each `.speak()` over-includes {Animal.speak, Dog.speak}; SCIP picks the one
  // the receiver's static type resolves to (Animal→Animal, Dog→Dog, Puppy→Dog).
  return a.speak() + d.speak() + p.speak();
}

export function describeAll(): string {
  const d = new Dog();
  const p = new Puppy();
  // `describe` is defined ONCE (on Animal) — a single repo-wide candidate, so
  // the unknown-receiver heuristic emits exactly one edge and SCIP confirms it.
  return d.describe() + p.describe();
}
