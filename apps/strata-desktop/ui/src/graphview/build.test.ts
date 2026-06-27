// Unit tests for the pure visual-encoding transform (build.ts).
//
// The graph view's *correctness contract* lives here: a WebGL canvas can't be
// asserted on headless, but every encoding rule that feeds it can. These tests
// pin plane→colour, confidence→width/opacity monotonicity, the reserved
// AMBIGUOUS colour (never reused by a plane), impact depth-tint ordering, the
// degree→size clamp, the impact join (engine results, not recomputed), and the
// truncated passthrough.

import { describe, expect, it } from "vitest";

import type { SubgraphDto, SubgraphEdge, SubgraphNode } from "../api";
import {
  AMBIGUOUS_EDGE_COLOR,
  EDGE_MAX_OPACITY,
  EDGE_MAX_WIDTH,
  EDGE_MIN_OPACITY,
  EDGE_MIN_WIDTH,
  NODE_MAX_SIZE,
  NODE_MIN_SIZE,
  PLANE_COLORS,
  TARGET_COLOR,
  buildImpact,
  buildLegend,
  buildNeighborhood,
  confidenceToOpacity,
  confidenceToWidth,
  degreeToSize,
  depthToTint,
  planeColor,
  reservedAmbiguousColor,
  withAlpha,
  type ImpactJoin,
} from "./build";

// ── fixtures ─────────────────────────────────────────────────────────────────

function node(uid: string, plane: string, opts: Partial<SubgraphNode> = {}): SubgraphNode {
  return {
    uid,
    name: opts.name ?? uid,
    kind: opts.kind ?? "Function",
    path: opts.path ?? `${uid}.ts`,
    plane,
  };
}

function edge(src: string, dst: string, opts: Partial<SubgraphEdge> = {}): SubgraphEdge {
  return {
    src,
    dst,
    kind: opts.kind ?? "Calls",
    provenance: opts.provenance ?? "Inferred",
    confidence: opts.confidence ?? 0.9,
  };
}

function dto(
  nodes: SubgraphNode[],
  edges: SubgraphEdge[],
  truncated = false,
): SubgraphDto {
  return { nodes, edges, truncated };
}

// ── plane → colour ───────────────────────────────────────────────────────────

describe("plane → colour", () => {
  it("maps each plane to its fixed palette colour", () => {
    expect(planeColor("code")).toBe(PLANE_COLORS.code);
    expect(planeColor("contract")).toBe(PLANE_COLORS.contract);
    expect(planeColor("infra")).toBe(PLANE_COLORS.infra);
    expect(planeColor("data")).toBe(PLANE_COLORS.data);
  });

  it("the four plane colours are mutually distinct", () => {
    const colors = new Set([
      PLANE_COLORS.code,
      PLANE_COLORS.contract,
      PLANE_COLORS.infra,
      PLANE_COLORS.data,
    ]);
    expect(colors.size).toBe(4);
  });

  it("falls back to a non-plane grey for an unknown plane", () => {
    const c = planeColor("quantum");
    expect(c).not.toBe(PLANE_COLORS.code);
    expect(c).not.toBe(PLANE_COLORS.contract);
    expect(c).not.toBe(PLANE_COLORS.infra);
    expect(c).not.toBe(PLANE_COLORS.data);
  });

  it("a built node carries its plane's colour", () => {
    const g = buildNeighborhood(dto([node("a", "contract")], []));
    expect(g.nodes[0].attributes.color).toBe(PLANE_COLORS.contract);
    expect(g.nodes[0].attributes.plane).toBe("contract");
  });
});

// ── AMBIGUOUS colour reservation ─────────────────────────────────────────────

describe("AMBIGUOUS edge colour is reserved", () => {
  it("is never equal to any plane colour", () => {
    expect(AMBIGUOUS_EDGE_COLOR).not.toBe(PLANE_COLORS.code);
    expect(AMBIGUOUS_EDGE_COLOR).not.toBe(PLANE_COLORS.contract);
    expect(AMBIGUOUS_EDGE_COLOR).not.toBe(PLANE_COLORS.infra);
    expect(AMBIGUOUS_EDGE_COLOR).not.toBe(PLANE_COLORS.data);
    expect(reservedAmbiguousColor()).toBe(AMBIGUOUS_EDGE_COLOR);
  });

  it("an AMBIGUOUS-provenance edge is drawn in the reserved colour", () => {
    const g = buildNeighborhood(
      dto(
        [node("a", "code"), node("b", "code")],
        [edge("a", "b", { provenance: "Ambiguous", confidence: 1 })],
      ),
    );
    const e = g.edges[0];
    expect(e.attributes.ambiguous).toBe(true);
    // Colour is the reserved hue (with full-opacity alpha at confidence 1).
    expect(e.attributes.color).toBe(withAlpha(AMBIGUOUS_EDGE_COLOR, EDGE_MAX_OPACITY));
  });

  it("a non-ambiguous edge never uses the reserved colour", () => {
    const g = buildNeighborhood(
      dto(
        [node("a", "code"), node("b", "code")],
        [edge("a", "b", { provenance: "Inferred", confidence: 1 })],
      ),
    );
    const e = g.edges[0];
    expect(e.attributes.ambiguous).toBe(false);
    expect(e.attributes.color).not.toContain("224, 179, 65"); // the amber rgb
  });

  it("the legend always reserves an ambiguous-edge row with that colour", () => {
    const rows = buildLegend(false);
    const amb = rows.find((r) => r.label === "ambiguous edge");
    expect(amb?.color).toBe(AMBIGUOUS_EDGE_COLOR);
    // And it is the only row using that colour (no plane reuses it).
    expect(rows.filter((r) => r.color === AMBIGUOUS_EDGE_COLOR)).toHaveLength(1);
  });
});

// ── confidence → width / opacity monotonicity ────────────────────────────────

describe("confidence → width is monotonic & clamped", () => {
  it("hits the bounds at 0 and 1", () => {
    expect(confidenceToWidth(0)).toBeCloseTo(EDGE_MIN_WIDTH, 6);
    expect(confidenceToWidth(1)).toBeCloseTo(EDGE_MAX_WIDTH, 6);
  });

  it("is non-decreasing across the unit interval", () => {
    let prev = -Infinity;
    for (let c = 0; c <= 1.0001; c += 0.05) {
      const w = confidenceToWidth(c);
      expect(w).toBeGreaterThanOrEqual(prev);
      prev = w;
    }
  });

  it("clamps out-of-range confidence", () => {
    expect(confidenceToWidth(-1)).toBeCloseTo(EDGE_MIN_WIDTH, 6);
    expect(confidenceToWidth(5)).toBeCloseTo(EDGE_MAX_WIDTH, 6);
  });
});

describe("confidence → opacity is monotonic & clamped", () => {
  it("hits the bounds at 0 and 1", () => {
    expect(confidenceToOpacity(0)).toBeCloseTo(EDGE_MIN_OPACITY, 6);
    expect(confidenceToOpacity(1)).toBeCloseTo(EDGE_MAX_OPACITY, 6);
  });

  it("keeps a 0-confidence edge faintly visible (floor > 0)", () => {
    expect(confidenceToOpacity(0)).toBeGreaterThan(0);
  });

  it("is non-decreasing across the unit interval", () => {
    let prev = -Infinity;
    for (let c = 0; c <= 1.0001; c += 0.05) {
      const o = confidenceToOpacity(c);
      expect(o).toBeGreaterThanOrEqual(prev);
      prev = o;
    }
  });

  it("a higher-confidence edge renders both wider and more opaque", () => {
    const g = buildNeighborhood(
      dto(
        [node("a", "code"), node("b", "code"), node("c", "code")],
        [
          edge("a", "b", { confidence: 0.2, kind: "Calls" }),
          edge("a", "c", { confidence: 0.95, kind: "Imports" }),
        ],
      ),
    );
    const lo = g.edges.find((e) => e.attributes.kind === "Calls")!;
    const hi = g.edges.find((e) => e.attributes.kind === "Imports")!;
    expect(hi.attributes.size).toBeGreaterThan(lo.attributes.size);
    expect(hi.attributes.opacity).toBeGreaterThan(lo.attributes.opacity);
  });
});

// ── degree → size clamp ──────────────────────────────────────────────────────

describe("degree → size is clamped & monotonic", () => {
  it("respects the min and max bounds", () => {
    expect(degreeToSize(0)).toBeGreaterThanOrEqual(NODE_MIN_SIZE);
    expect(degreeToSize(0)).toBeCloseTo(NODE_MIN_SIZE, 6);
    expect(degreeToSize(100000)).toBeLessThanOrEqual(NODE_MAX_SIZE);
    expect(degreeToSize(100000)).toBeCloseTo(NODE_MAX_SIZE, 6);
  });

  it("is non-decreasing in degree", () => {
    let prev = -Infinity;
    for (const d of [0, 1, 2, 4, 8, 16, 64, 256, 1024]) {
      const s = degreeToSize(d);
      expect(s).toBeGreaterThanOrEqual(prev);
      prev = s;
    }
  });

  it("sizes a hub larger than a leaf", () => {
    // a is a hub (3 edges), b/c/d are leaves (1 each).
    const g = buildNeighborhood(
      dto(
        [node("a", "code"), node("b", "code"), node("c", "code"), node("d", "code")],
        [edge("a", "b"), edge("a", "c"), edge("a", "d")],
      ),
    );
    const a = g.nodes.find((n) => n.key === "a")!;
    const b = g.nodes.find((n) => n.key === "b")!;
    expect(a.attributes.degree).toBe(3);
    expect(b.attributes.degree).toBe(1);
    expect(a.attributes.size).toBeGreaterThan(b.attributes.size);
  });
});

// ── focus accent (neighbourhood) ─────────────────────────────────────────────

describe("focus node accent", () => {
  it("paints the focus node in the target accent, not its plane colour", () => {
    const g = buildNeighborhood(
      dto([node("a", "code"), node("b", "code")], [edge("a", "b")]),
      { focusUid: "a" },
    );
    const a = g.nodes.find((n) => n.key === "a")!;
    const b = g.nodes.find((n) => n.key === "b")!;
    expect(a.attributes.isTarget).toBe(true);
    expect(a.attributes.color).toBe(TARGET_COLOR);
    expect(b.attributes.isTarget).toBe(false);
    expect(b.attributes.color).toBe(PLANE_COLORS.code);
  });
});

// ── impact depth tints ───────────────────────────────────────────────────────

describe("impact depth tint ordering", () => {
  it("is 1.0 at the target and non-increasing with depth", () => {
    expect(depthToTint(0, 3)).toBeCloseTo(1, 6);
    const t0 = depthToTint(0, 3);
    const t1 = depthToTint(1, 3);
    const t2 = depthToTint(2, 3);
    const t3 = depthToTint(3, 3);
    expect(t0).toBeGreaterThan(t1);
    expect(t1).toBeGreaterThan(t2);
    expect(t2).toBeGreaterThan(t3);
  });

  it("keeps the deepest ring visible (tint floor > 0)", () => {
    expect(depthToTint(99, 3)).toBeGreaterThan(0);
  });

  function impactFixture(): ImpactJoin {
    // target t (depth 0) → a (depth 1) → b (depth 2); plus an unrelated node z.
    const sub = dto(
      [
        node("t", "code"),
        node("a", "code"),
        node("b", "contract"),
        node("z", "infra"),
      ],
      [
        edge("a", "t", { kind: "Calls", confidence: 0.9 }),
        edge("b", "a", { kind: "Calls", confidence: 0.4 }),
        edge("z", "t", { kind: "Calls", confidence: 0.9 }),
      ],
    );
    return {
      targetUid: "t",
      affected: [
        { uid: "a", depth: 1, confidence: 0.9, ambiguous: false },
        { uid: "b", depth: 2, confidence: 0.4, ambiguous: false },
      ],
      subgraph: sub,
    };
  }

  it("emits only the impacted set (target + affected), dropping unrelated nodes", () => {
    const g = buildImpact(impactFixture());
    const keys = g.nodes.map((n) => n.key).sort();
    expect(keys).toEqual(["a", "b", "t"]);
    // The edge to the dropped node z must not appear.
    expect(g.edges.every((e) => e.source !== "z" && e.target !== "z")).toBe(true);
  });

  it("orders node tints by engine depth (target brightest, deepest faded)", () => {
    const g = buildImpact(impactFixture());
    const t = g.nodes.find((n) => n.key === "t")!;
    const a = g.nodes.find((n) => n.key === "a")!;
    const b = g.nodes.find((n) => n.key === "b")!;
    expect(t.attributes.depth).toBe(0);
    expect(a.attributes.depth).toBe(1);
    expect(b.attributes.depth).toBe(2);
    expect(t.attributes.depthTint!).toBeGreaterThan(a.attributes.depthTint!);
    expect(a.attributes.depthTint!).toBeGreaterThan(b.attributes.depthTint!);
  });

  it("accents the target node and marks it depth 0", () => {
    const g = buildImpact(impactFixture());
    const t = g.nodes.find((n) => n.key === "t")!;
    expect(t.attributes.isTarget).toBe(true);
    expect(t.attributes.color).toBe(TARGET_COLOR);
    expect(t.attributes.depthTint).toBeCloseTo(1, 6);
  });

  it("does not recompute impact — only the engine's affected uids are tinted", () => {
    // The subgraph contains `z` adjacent to the target, but the engine did NOT
    // mark it affected, so it must be absent from the impact view.
    const g = buildImpact(impactFixture());
    expect(g.nodes.some((n) => n.key === "z")).toBe(false);
  });

  it("adds the impact-target row to the legend", () => {
    const rows = buildLegend(true);
    expect(rows.some((r) => r.label === "impact target" && r.color === TARGET_COLOR)).toBe(true);
  });
});

// ── truncated passthrough ────────────────────────────────────────────────────

describe("truncated flag is surfaced", () => {
  it("passes a truncated neighbourhood through", () => {
    const g = buildNeighborhood(dto([node("a", "code")], [], true));
    expect(g.truncated).toBe(true);
  });

  it("is false when not truncated", () => {
    const g = buildNeighborhood(dto([node("a", "code")], [], false));
    expect(g.truncated).toBe(false);
  });

  it("passes a truncated impact subgraph through", () => {
    const join: ImpactJoin = {
      targetUid: "t",
      affected: [],
      subgraph: dto([node("t", "code")], [], true),
    };
    expect(buildImpact(join).truncated).toBe(true);
  });
});

// ── withAlpha helper ─────────────────────────────────────────────────────────

describe("withAlpha", () => {
  it("produces an rgba string from a hex colour", () => {
    expect(withAlpha("#4ea1ff", 1)).toBe("rgba(78, 161, 255, 1.000)");
    expect(withAlpha("#000000", 0.5)).toBe("rgba(0, 0, 0, 0.500)");
  });

  it("clamps alpha into [0,1]", () => {
    expect(withAlpha("#ffffff", 5)).toBe("rgba(255, 255, 255, 1.000)");
    expect(withAlpha("#ffffff", -2)).toBe("rgba(255, 255, 255, 0.000)");
  });
});
