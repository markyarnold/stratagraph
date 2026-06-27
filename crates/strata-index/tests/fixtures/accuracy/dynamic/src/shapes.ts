// Two classes sharing method names (`area`, `name`) — the unknown-receiver
// over-inclusion the AMBIGUOUS band measures. Called through TYPED receivers in
// users.ts (SCIP narrows 2→1), and through an `any`-typed receiver (SCIP cannot
// adjudicate — the honest unadjudicable case).

export class Rect {
  area(): number {
    return 4;
  }
  name(): string {
    return "rect";
  }
}

export class Disc {
  area(): number {
    return 3;
  }
  name(): string {
    return "disc";
  }
}
