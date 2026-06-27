// A free function with a unique name (the BareSingle target in callers.ts) and a
// `helper` that collides with a local `helper` in shadow.ts (the BareMulti case).
export function format(s: string): string {
  return s;
}

export function helper(): number {
  return 1;
}
