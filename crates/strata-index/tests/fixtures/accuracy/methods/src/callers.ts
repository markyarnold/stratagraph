import { Circle, Square } from "./shapes";
import { format } from "./util";

// Typed-receiver calls. The receiver is a local identifier, so the heuristic
// treats it as an unknown receiver (rule 3) and emits edges to EVERY method
// named `save`/`render` repo-wide (Circle.* and Square.*). SCIP, knowing the
// receiver's type, resolves each to the single correct method — so these are
// SCIP-covered UnknownReceiver sites where the heuristic over-includes 2→1.
export function drawCircle() {
  const c = new Circle();
  c.save();
  c.render();
}

export function drawSquare() {
  const s = new Square();
  s.save();
  s.render();
}

// More typed-receiver calls so the UnknownReceiver class has enough covered
// sites to calibrate (≥ 5). Each over-includes both same-named methods; SCIP
// narrows to the receiver's type.
export function drawBoth() {
  const c = new Circle();
  const s = new Square();
  c.save();
  s.render();
}

// A bare call to an imported free function with a UNIQUE name: exactly one
// heuristic candidate (the import), SCIP confirms it. BareSingle, precision 1.
export function useFormat() {
  return format("x");
}
