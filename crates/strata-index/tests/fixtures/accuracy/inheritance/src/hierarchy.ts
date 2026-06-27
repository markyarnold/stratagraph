// A three-level class hierarchy exercising inheritance + method override.
// The `this.method()` heuristic only looks in the ENCLOSING class, so a call to
// an inherited (not overridden) method is a recall miss the heuristic makes and
// SCIP resolves by climbing the chain. An overridden method called via `this`
// resolves to the nearest definition (the override), which the heuristic also
// finds — a hit on both sides.

export class Animal {
  // Defined only on the base; subclasses inherit it.
  describe(): string {
    return "animal";
  }
  // Overridden down the chain.
  speak(): string {
    return "...";
  }
  // Calls an inherited method + an overridden one through `this`.
  greet(): string {
    // `this.describe()` is inherited from here (enclosing class) — heuristic hit.
    // `this.speak()` is defined here too — hit.
    return this.describe() + this.speak();
  }
}

export class Dog extends Animal {
  // Override: nearest `speak` for a Dog `this`.
  speak(): string {
    return "woof";
  }
  // `this.describe()` is INHERITED (defined on Animal, not Dog) — the
  // enclosing-class-only heuristic finds nothing here (recall miss); SCIP climbs
  // to Animal.describe. `super.speak()` reaches the BASE override explicitly.
  bark(): string {
    return this.describe() + super.speak() + this.speak();
  }
}

export class Puppy extends Dog {
  // Two levels up: `this.describe()` is on Animal (grandparent) — heuristic miss,
  // SCIP resolves through Dog to Animal. `this.speak()` resolves to Dog.speak
  // (nearest), which the heuristic cannot see from Puppy either (miss); SCIP
  // resolves it.
  yip(): string {
    return this.describe() + this.speak();
  }
}
