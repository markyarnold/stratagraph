// Unit tests for the pure presentation helpers in api.ts.
//
// The will-break verdict label is the GUI's word-for-word echo of the engine's
// §15.6 classification (the same "WILL BREAK" / "may affect" wording the CLI
// prints); pinning the exact strings keeps the three surfaces consistent.

import { describe, expect, it } from "vitest";

import {
  breakVerdict,
  formatHop,
  impactOutcome,
  membersHint,
  type ImpactResult,
  type PathHop,
} from "./api";

describe("breakVerdict", () => {
  it("renders the §15.6 will-break verdict in words", () => {
    expect(breakVerdict(true)).toBe("WILL BREAK");
    expect(breakVerdict(false)).toBe("may affect");
  });
});

describe("formatHop", () => {
  // The evidence-chain line is the same shape the CLI prints: the friendly node
  // NAMES (not uids), the shouted edge kind, the provenance + edge confidence,
  // and the running (accumulated) confidence after the hop.
  const nameOf = (uid: string) =>
    ({ "u|getUser": "getUser", "u|field": "Query.getUser" })[uid] ?? uid;

  it("renders a hop with names, kind, provenance, and running confidence", () => {
    const hop: PathHop = {
      from: "u|getUser",
      to: "u|field",
      edge_kind: "Produces",
      provenance: "Extracted",
      confidence: 0.95,
      running_confidence: 0.95,
    };
    expect(formatHop(hop, nameOf)).toBe(
      "getUser  —PRODUCES (Extracted 0.95)→  Query.getUser    running 0.95",
    );
  });

  it("falls back to the uid when a name is unknown, and decays the running conf", () => {
    const hop: PathHop = {
      from: "u|field",
      to: "u|orphan",
      edge_kind: "Consumes",
      provenance: "Ambiguous",
      confidence: 0.4,
      running_confidence: 0.38,
    };
    // u|orphan has no friendly name → the uid is shown verbatim.
    expect(formatHop(hop, nameOf)).toBe(
      "Query.getUser  —CONSUMES (Ambiguous 0.40)→  u|orphan    running 0.38",
    );
  });
});

describe("membersHint (F1 — zero-direct member surfacing)", () => {
  // Mirrors the CLI render_zero_affected wording: 0 deps on the type itself, but N
  // members do — so the GUI never reprints the misleading "nothing depends on this".
  it("names the members that have dependents (non-empty hint)", () => {
    const hint = membersHint("PolicyService", [
      { uid: "u|a", name: "getStats", kind: "Method" },
      { uid: "u|b", name: "listAll", kind: "Method" },
    ]);
    expect(hint).toBe(
      "0 dependents on PolicyService itself; 2 of its members have dependents: getStats, listAll",
    );
  });

  it("summarises the overflow past MEMBER_HINT_MAX as a count", () => {
    const members = Array.from({ length: 7 }, (_, i) => ({
      uid: `u|${i}`,
      name: `m${i}`,
      kind: "Method",
    }));
    // Lists the first 5, then "… (+2 more)".
    expect(membersHint("Big", members)).toBe(
      "0 dependents on Big itself; 7 of its members have dependents: m0, m1, m2, m3, m4, … (+2 more)",
    );
  });
});

describe("impactOutcome (F1/F2 — shape classification, no throw)", () => {
  it("classifies an ambiguous result as candidates (never a throw)", () => {
    const res: ImpactResult = {
      ambiguous: true,
      symbol: "publish",
      candidates: [
        { uid: "svc/a.ts|publish", name: "publish", kind: "Function", path: "svc/a.ts" },
        { uid: "svc/b.ts|publish", name: "publish", kind: "Function", path: "svc/b.ts" },
      ],
    };
    const outcome = impactOutcome(res);
    expect(outcome.kind).toBe("ambiguous");
    if (outcome.kind === "ambiguous") {
      expect(outcome.symbol).toBe("publish");
      expect(outcome.candidates).toHaveLength(2);
      expect(outcome.candidates[0].uid).toBe("svc/a.ts|publish");
    }
  });

  it("classifies a zero-direct result with member dependents as the members hint", () => {
    const res: ImpactResult = {
      target: { uid: "u|t", name: "T", kind: "Class", path: "t.ts" },
      affected: [],
      members_with_dependents: [{ uid: "u|m", name: "method", kind: "Method" }],
    };
    const outcome = impactOutcome(res);
    expect(outcome.kind).toBe("members");
    if (outcome.kind === "members") {
      expect(outcome.members).toHaveLength(1);
      expect(outcome.members[0].name).toBe("method");
    }
  });

  it("classifies a non-empty affected set as the table", () => {
    const res: ImpactResult = {
      target: { uid: "u|t", name: "T", kind: "Function", path: "t.ts" },
      affected: [
        { uid: "u|d", name: "dep", depth: 1, confidence: 0.95, ambiguous: false, will_break: true },
      ],
    };
    expect(impactOutcome(res).kind).toBe("affected");
  });

  it("classifies a genuinely dead target as empty", () => {
    const res: ImpactResult = {
      target: { uid: "u|t", name: "T", kind: "Function", path: "t.ts" },
      affected: [],
    };
    expect(impactOutcome(res).kind).toBe("empty");
  });
});
