// The pure visual-encoding core of the graph view.
//
// This module is deliberately free of Sigma, Graphology, and the DOM: it turns a
// backend payload (a neighbourhood `SubgraphDto`, or an impact join) into plain
// attribute bags a Graphology graph can be populated from. Keeping it pure is
// what makes the visual encoding *testable* (see build.test.ts) — every rule
// below (plane→colour, confidence→width/opacity, the reserved AMBIGUOUS colour,
// impact depth tints) is pinned by a unit test rather than eyeballed on a canvas.
//
// Honest encoding note — dashed edges: Sigma v3 has no native dashed-line edge
// program. The contract is "AMBIGUOUS edges must be visually distinct"; we honour
// it with a **reserved colour** (never reused by any plane) plus a legend entry,
// rather than a stroke pattern. `AMBIGUOUS_EDGE_COLOR` is the single source of
// that colour and `reservedAmbiguousColor()` exposes it to the legend.

import type { SubgraphDto, SubgraphEdge, SubgraphNode } from "../api";

// ── palette (the one source of plane colours) ────────────────────────────────

/** Plane → base node colour. Three perceptually distinct hues. */
export const PLANE_COLORS = {
  code: "#4ea1ff", // blue    — program structure
  contract: "#c08cff", // violet  — interface operations
  infra: "#4cc38a", // green   — cloud resources
  data: "#e0699f", // rose    — database tables/columns (Slice 16, D3)
} as const;

export type Plane = keyof typeof PLANE_COLORS;

/** Fallback for an unrecognised plane string (forward-compat / bad data). */
export const UNKNOWN_PLANE_COLOR = "#8a93a3"; // muted grey

/**
 * The reserved colour for AMBIGUOUS edges. Amber — chosen to sit outside the
 * plane palette so it can never be confused with a plane. Sigma has no dashed
 * edges, so this colour *is* the ambiguity encoding (paired with a legend entry).
 */
export const AMBIGUOUS_EDGE_COLOR = "#e0b341";

/** Default (non-ambiguous) edge colour, before the confidence alpha is applied. */
export const EDGE_BASE_COLOR = "#6b7585";

/** The accent colour for the focus / impact target node. */
export const TARGET_COLOR = "#ff8a4c"; // warm orange, distinct from every plane

// ── encoding bounds (all clamped, all monotonic) ─────────────────────────────

export const NODE_MIN_SIZE = 4;
export const NODE_MAX_SIZE = 18;
/** The focus/target node is drawn at least this big regardless of degree. */
export const TARGET_MIN_SIZE = 12;

export const EDGE_MIN_WIDTH = 0.5;
export const EDGE_MAX_WIDTH = 5;

/** Confidence→opacity floor: even a 0-confidence edge stays faintly visible. */
export const EDGE_MIN_OPACITY = 0.2;
export const EDGE_MAX_OPACITY = 1;

// ── output shapes (Graphology-ready) ─────────────────────────────────────────

export interface BuiltNode {
  key: string;
  attributes: {
    label: string;
    /** Initial layout position (caller may overwrite to reuse prior layout). */
    x: number;
    y: number;
    size: number;
    color: string;
    // ── metadata carried for interactions / tooltips / impact styling ──
    plane: string;
    kind: string;
    path: string;
    /** Degree used to size the node (after clamping inputs). */
    degree: number;
    /** True for the focus node (neighbourhood) or impact target. */
    isTarget: boolean;
    /** Impact depth (0 = target). `undefined`/absent outside impact mode. */
    depth?: number;
    /**
     * Impact depth tint in [0,1]: 1 = strongest (target/shallow), →0 = faded
     * (deep). `undefined` outside impact mode. Monotonic *non-increasing* in
     * depth (deeper ⇒ smaller), which the renderer maps to alpha.
     */
    depthTint?: number;
  };
}

export interface BuiltEdge {
  key: string;
  source: string;
  target: string;
  attributes: {
    size: number;
    color: string;
    /** Raw confidence→opacity in [EDGE_MIN_OPACITY, EDGE_MAX_OPACITY]. */
    opacity: number;
    kind: string;
    provenance: string;
    confidence: number;
    /** True when provenance is AMBIGUOUS (drawn in the reserved colour). */
    ambiguous: boolean;
  };
}

export interface LegendEntry {
  label: string;
  color: string;
}

export interface BuiltGraph {
  nodes: BuiltNode[];
  edges: BuiltEdge[];
  legend: LegendEntry[];
  truncated: boolean;
}

// ── impact join input ────────────────────────────────────────────────────────

/** One affected node from a `tool("impact")` result (depth + confidence). */
export interface AffectedRef {
  uid: string;
  depth: number;
  confidence: number;
  ambiguous: boolean;
}

/**
 * The impact-mode input: the engine's real impact result (target uid + affected
 * set) joined with a `SubgraphDto` that supplies node/edge *shape* (kind, path,
 * plane, edge provenance/confidence). Impact is **never** recomputed here — the
 * `affected` set comes verbatim from the engine; we only restrict and tint the
 * subgraph to it.
 */
export interface ImpactJoin {
  targetUid: string;
  affected: AffectedRef[];
  subgraph: SubgraphDto;
}

export interface BuildOptions {
  /** The focus node uid (neighbourhood mode): accented like a target. */
  focusUid?: string;
}

// ── helpers ──────────────────────────────────────────────────────────────────

function clamp(value: number, lo: number, hi: number): number {
  if (Number.isNaN(value)) return lo;
  return Math.min(hi, Math.max(lo, value));
}

/** Linear map of `t∈[0,1]` onto `[lo,hi]` (t clamped first). */
function lerp(t: number, lo: number, hi: number): number {
  const c = clamp(t, 0, 1);
  return lo + (hi - lo) * c;
}

/** Plane string → node colour, with a grey fallback for unknown planes. */
export function planeColor(plane: string): string {
  return (PLANE_COLORS as Record<string, string>)[plane] ?? UNKNOWN_PLANE_COLOR;
}

/** Confidence∈[0,1] → edge width, clamped and monotonic non-decreasing. */
export function confidenceToWidth(confidence: number): number {
  return lerp(confidence, EDGE_MIN_WIDTH, EDGE_MAX_WIDTH);
}

/** Confidence∈[0,1] → edge opacity, clamped and monotonic non-decreasing. */
export function confidenceToOpacity(confidence: number): number {
  return lerp(confidence, EDGE_MIN_OPACITY, EDGE_MAX_OPACITY);
}

/** Degree → node size, clamped to [NODE_MIN_SIZE, NODE_MAX_SIZE], monotonic. */
export function degreeToSize(degree: number): number {
  // Compress with sqrt so a hub does not dwarf everything, then clamp.
  const raw = NODE_MIN_SIZE + Math.sqrt(Math.max(0, degree)) * 2.2;
  return clamp(raw, NODE_MIN_SIZE, NODE_MAX_SIZE);
}

/**
 * Impact depth → tint weight in [0,1], monotonic **non-increasing**: the target
 * (depth 0) is 1.0, and each hop fades toward — but never reaches — 0, so the
 * deepest ring stays visible. `maxDepth` is the largest depth in the set (used to
 * spread the gradient); a single-ring impact still fades from 1.0.
 */
export function depthToTint(depth: number, maxDepth: number): number {
  const d = Math.max(0, depth);
  const span = Math.max(1, maxDepth);
  // 1.0 at depth 0 → 0.35 at maxDepth (floor keeps deep nodes readable).
  const TINT_FLOOR = 0.35;
  return clamp(1 - (d / span) * (1 - TINT_FLOOR), TINT_FLOOR, 1);
}

/** The reserved AMBIGUOUS edge colour (for the legend / assertions). */
export function reservedAmbiguousColor(): string {
  return AMBIGUOUS_EDGE_COLOR;
}

/** An edge is ambiguous when its provenance is the AMBIGUOUS variant. */
function isAmbiguousEdge(e: SubgraphEdge): boolean {
  return e.provenance === "Ambiguous";
}

/** Convert a `#rrggbb` hex + alpha∈[0,1] to an `rgba(...)` string for Sigma. */
export function withAlpha(hex: string, alpha: number): string {
  const m = /^#?([0-9a-fA-F]{6})$/.exec(hex.trim());
  const a = clamp(alpha, 0, 1);
  if (!m) return hex;
  const n = parseInt(m[1], 16);
  const r = (n >> 16) & 0xff;
  const g = (n >> 8) & 0xff;
  const b = n & 0xff;
  return `rgba(${r}, ${g}, ${b}, ${a.toFixed(3)})`;
}

/** Compute undirected degree per node uid from an edge list. */
function degrees(edges: SubgraphEdge[]): Map<string, number> {
  const deg = new Map<string, number>();
  const bump = (uid: string) => deg.set(uid, (deg.get(uid) ?? 0) + 1);
  for (const e of edges) {
    bump(e.src);
    bump(e.dst);
  }
  return deg;
}

/**
 * Deterministic initial position for a node, derived from its uid. A real
 * layout (ForceAtlas2) runs afterwards in the renderer; this only needs to be
 * a non-degenerate, reproducible seed (ForceAtlas2 misbehaves if every node
 * starts at the origin). Pure + seeded so tests are stable.
 */
function seedPosition(uid: string, index: number, total: number): { x: number; y: number } {
  // Spread seeds on a circle by index, jittered by a cheap uid hash so equal
  // indices across renders still differ a touch.
  let h = 0;
  for (let i = 0; i < uid.length; i++) h = (h * 31 + uid.charCodeAt(i)) | 0;
  const jitter = ((h >>> 0) % 1000) / 1000; // [0,1)
  const angle = (index / Math.max(1, total)) * Math.PI * 2 + jitter * 0.5;
  const radius = 1 + jitter; // keep off the exact origin
  return { x: Math.cos(angle) * radius, y: Math.sin(angle) * radius };
}

// ── the legend ───────────────────────────────────────────────────────────────

/**
 * The legend rows for the current mode. Always includes the four planes and the
 * reserved AMBIGUOUS edge colour; impact mode adds the target accent.
 */
export function buildLegend(impact: boolean): LegendEntry[] {
  const rows: LegendEntry[] = [
    { label: "code", color: PLANE_COLORS.code },
    { label: "contract", color: PLANE_COLORS.contract },
    { label: "infra", color: PLANE_COLORS.infra },
    { label: "data", color: PLANE_COLORS.data },
    { label: "ambiguous edge", color: AMBIGUOUS_EDGE_COLOR },
  ];
  if (impact) rows.push({ label: "impact target", color: TARGET_COLOR });
  return rows;
}

// ── node / edge builders shared by both modes ────────────────────────────────

interface NodeStyle {
  isTarget: boolean;
  depth?: number;
  depthTint?: number;
}

function buildNode(
  node: SubgraphNode,
  index: number,
  total: number,
  degree: number,
  style: NodeStyle,
): BuiltNode {
  const baseSize = degreeToSize(degree);
  const size = style.isTarget ? Math.max(baseSize, TARGET_MIN_SIZE) : baseSize;
  const color = style.isTarget ? TARGET_COLOR : planeColor(node.plane);
  const pos = seedPosition(node.uid, index, total);
  return {
    key: node.uid,
    attributes: {
      label: node.name,
      x: pos.x,
      y: pos.y,
      size,
      color,
      plane: node.plane,
      kind: node.kind,
      path: node.path,
      degree,
      isTarget: style.isTarget,
      depth: style.depth,
      depthTint: style.depthTint,
    },
  };
}

function buildEdge(e: SubgraphEdge): BuiltEdge {
  const ambiguous = isAmbiguousEdge(e);
  const opacity = confidenceToOpacity(e.confidence);
  const baseColor = ambiguous ? AMBIGUOUS_EDGE_COLOR : EDGE_BASE_COLOR;
  return {
    key: `${e.src}->${e.dst}:${e.kind}`,
    source: e.src,
    target: e.dst,
    attributes: {
      size: confidenceToWidth(e.confidence),
      color: withAlpha(baseColor, opacity),
      opacity,
      kind: e.kind,
      provenance: e.provenance,
      confidence: e.confidence,
      ambiguous,
    },
  };
}

// ── public entry points ──────────────────────────────────────────────────────

/**
 * Build the neighbourhood graph (no impact tinting). The focus node — if present
 * in the payload and named by `options.focusUid` — is accented like a target.
 */
export function buildNeighborhood(dto: SubgraphDto, options: BuildOptions = {}): BuiltGraph {
  const deg = degrees(dto.edges);
  const total = dto.nodes.length;
  const nodes = dto.nodes.map((n, i) =>
    buildNode(n, i, total, deg.get(n.uid) ?? 0, {
      isTarget: n.uid === options.focusUid,
    }),
  );
  const edges = dto.edges.map(buildEdge);
  return { nodes, edges, legend: buildLegend(false), truncated: dto.truncated };
}

/**
 * Build the impact blast-radius graph: the engine's affected set tinted by depth
 * over the subgraph that supplies shape. Only nodes in the affected set (plus the
 * target) are emitted; edges are kept when **both** endpoints survive, so nothing
 * dangles. Impact is taken verbatim from `join.affected` — never recomputed.
 */
export function buildImpact(join: ImpactJoin): BuiltGraph {
  const { targetUid, affected, subgraph } = join;

  // depth lookup for the affected set; the target is depth 0 even if the engine
  // did not list it among `affected`.
  const depthByUid = new Map<string, number>();
  depthByUid.set(targetUid, 0);
  for (const a of affected) {
    // Keep the shallowest depth if a uid appears more than once.
    const prev = depthByUid.get(a.uid);
    depthByUid.set(a.uid, prev === undefined ? a.depth : Math.min(prev, a.depth));
  }

  const maxDepth = Math.max(0, ...[...depthByUid.values()]);

  // Restrict the subgraph nodes to the impacted set (target + affected).
  const kept = subgraph.nodes.filter((n) => depthByUid.has(n.uid));
  const keptUids = new Set(kept.map((n) => n.uid));
  const deg = degrees(subgraph.edges.filter((e) => keptUids.has(e.src) && keptUids.has(e.dst)));
  const total = kept.length;

  const nodes = kept.map((n, i) => {
    const depth = depthByUid.get(n.uid) ?? 0;
    const isTarget = n.uid === targetUid;
    return buildNode(n, i, total, deg.get(n.uid) ?? 0, {
      isTarget,
      depth,
      depthTint: depthToTint(depth, maxDepth),
    });
  });

  const edges = subgraph.edges
    .filter((e) => keptUids.has(e.src) && keptUids.has(e.dst))
    .map(buildEdge);

  return { nodes, edges, legend: buildLegend(true), truncated: subgraph.truncated };
}
