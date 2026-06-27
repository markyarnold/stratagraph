// A normal code file that must still produce a code-plane graph even though a
// sibling spec is malformed (R2 graceful degradation).
export function helper() {
  return compute();
}

export function compute() {
  return 42;
}
