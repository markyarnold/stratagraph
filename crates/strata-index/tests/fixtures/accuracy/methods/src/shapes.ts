// Two unrelated classes that both define `save` and `render`. The heuristic's
// unknown-receiver rule (rule 3) over-includes BOTH same-named methods repo-wide;
// SCIP, with the receiver's concrete type, narrows to exactly one.
export class Circle {
  save() {}
  render() {}
}

export class Square {
  save() {}
  render() {}
}

// A base/derived pair for the `this.method()` inheritance case: `Derived.run`
// calls `this.base()`, but `base` is defined on `Base`, not on `Derived`. The
// heuristic's this-rule only looks in the *enclosing* class (Derived) and finds
// nothing (a recall miss); SCIP resolves it to `Base.base`.
export class Base {
  base() {}
}

export class Derived extends Base {
  run() {
    this.base();
  }
  // `this.own()` is in the enclosing class — heuristic + SCIP agree.
  own() {}
  callOwn() {
    this.own();
  }
}
