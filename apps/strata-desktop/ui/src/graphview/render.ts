// The Sigma/Graphology renderer that draws what build.ts computes.
//
// This is the *impure* half of the graph view: it owns a Sigma WebGL instance
// bound to a container, populates a Graphology graph from the pure transform's
// attribute bags, runs a bounded (synchronous, worker-free) ForceAtlas2 layout,
// and forwards Sigma's node/stage events to the controller. All visual-encoding
// decisions already happened in build.ts — here we only translate them onto a
// canvas (and apply the impact depth tint to node alpha, since that's a
// render-time colour concern).
//
// Headless note: a WebGL canvas cannot be asserted on in CI, so nothing in this
// file is unit-tested directly; its inputs (BuiltGraph) are fully covered by
// build.test.ts, and the wiring is exercised by `tauri build` + manual runs.

import Graph from "graphology";
import forceAtlas2 from "graphology-layout-forceatlas2";
import Sigma from "sigma";
import { drawDiscNodeLabel } from "sigma/rendering";
import type { Settings } from "sigma/settings";
import type { NodeDisplayData, PartialButFor } from "sigma/types";

import { withAlpha, type BuiltGraph, type BuiltNode } from "./build";

/**
 * Canvas colours resolved from the app stylesheet, so the WebGL layer follows
 * the same theme tokens as the DOM (labels were unreadable black-on-dark when
 * left to Sigma's defaults).
 */
interface CanvasTheme {
  label: string;
  hoverBg: string;
  hoverBorder: string;
  font: string;
}

/** Read a CSS custom property off `el`, falling back if unset/empty. */
function cssVar(el: HTMLElement, name: string, fallback: string): string {
  const value = getComputedStyle(el).getPropertyValue(name).trim();
  return value || fallback;
}

/** Resolve the canvas theme from the container's computed styles. */
function resolveTheme(el: HTMLElement): CanvasTheme {
  return {
    label: cssVar(el, "--text", "#e6e9ef"),
    hoverBg: cssVar(el, "--bg-raised", "#1b1f26"),
    hoverBorder: cssVar(el, "--border-strong", "#3a414d"),
    font: getComputedStyle(el).fontFamily || "sans-serif",
  };
}

/**
 * Hover badge drawer matching the app theme. Sigma's default hardcodes a white
 * box (fine on light themes, wrong here); this paints the same capsule geometry
 * in the theme's raised-surface colour, then defers to the stock label drawer —
 * which uses `settings.labelColor`, i.e. the themed text colour.
 */
function makeHoverDrawer(theme: CanvasTheme) {
  return function drawNodeHover(
    context: CanvasRenderingContext2D,
    data: PartialButFor<NodeDisplayData, "x" | "y" | "size" | "label" | "color">,
    settings: Settings,
  ): void {
    const size = settings.labelSize;
    context.font = `${settings.labelWeight} ${size}px ${settings.labelFont}`;

    context.fillStyle = theme.hoverBg;
    context.strokeStyle = theme.hoverBorder;
    context.lineWidth = 1;

    if (typeof data.label === "string") {
      const textWidth = context.measureText(data.label).width;
      const boxWidth = Math.round(textWidth + 9);
      const boxHeight = Math.round(size + 8);
      const radius = Math.max(data.size, size / 2) + 2;

      context.beginPath();
      context.moveTo(data.x, data.y - boxHeight / 2);
      context.lineTo(data.x + radius + boxWidth, data.y - boxHeight / 2);
      context.lineTo(data.x + radius + boxWidth, data.y + boxHeight / 2);
      context.lineTo(data.x, data.y + boxHeight / 2);
      context.arc(data.x, data.y, radius, Math.PI / 2, (Math.PI * 3) / 2);
      context.closePath();
      context.fill();
      context.stroke();
    } else {
      context.beginPath();
      context.arc(data.x, data.y, data.size + 2, 0, Math.PI * 2);
      context.closePath();
      context.fill();
      context.stroke();
    }

    drawDiscNodeLabel(context, data, settings);
  };
}

/** Callbacks the controller supplies to react to graph interactions. */
export interface GraphViewHandlers {
  /** Single click on a node → select it + load Context. */
  onSelect(uid: string): void;
  /** Double click on a node → re-root the neighbourhood there. */
  onReroot(uid: string): void;
}

/** A node's last-known layout position, reused across re-renders. */
interface Pos {
  x: number;
  y: number;
}

/**
 * Bounded ForceAtlas2 budget. At ≤ MAX_NODES (500) nodes this runs well under a
 * frame budget synchronously, so no web worker is needed (spec).
 */
const FA2_ITERATIONS = 200;

/**
 * Owns the Sigma instance for one container. Call `render` to (re)draw a built
 * graph; `destroy` to tear down. Positions persist between renders for any node
 * that survives, so filtering/depth changes don't reshuffle the whole layout.
 */
export class GraphView {
  private readonly container: HTMLElement;
  private readonly handlers: GraphViewHandlers;
  private sigma: Sigma | null = null;
  private graph: Graph;
  /** Remembered positions by node key, so a re-render reuses them. */
  private positions = new Map<string, Pos>();

  constructor(container: HTMLElement, handlers: GraphViewHandlers) {
    this.container = container;
    this.handlers = handlers;
    this.graph = new Graph();
  }

  /** Whether a Sigma instance is currently live. */
  get isMounted(): boolean {
    return this.sigma !== null;
  }

  /**
   * Draw `built` into the container. Rebuilds the Graphology graph from scratch
   * (cheap at this scale) but reuses any remembered positions, then runs a
   * bounded layout over the nodes that lack one and mounts/refreshes Sigma.
   */
  render(built: BuiltGraph): void {
    const graph = new Graph({ multi: true });

    for (const node of built.nodes) {
      graph.addNode(node.key, this.nodeAttributes(node));
    }
    for (const edge of built.edges) {
      // Guard against an edge whose endpoint was dropped (shouldn't happen — the
      // transform filters dangling edges — but never throw on the render path).
      if (!graph.hasNode(edge.source) || !graph.hasNode(edge.target)) continue;
      if (graph.hasEdge(edge.key)) continue;
      graph.addEdgeWithKey(edge.key, edge.source, edge.target, edge.attributes);
    }

    this.layout(graph);
    this.remember(graph);
    this.graph = graph;
    this.mountOrRefresh();
  }

  /** Tear down Sigma and forget the layout. */
  destroy(): void {
    if (this.sigma) {
      this.sigma.kill();
      this.sigma = null;
    }
    this.positions.clear();
  }

  /** Drop remembered positions so the next render lays out fresh. */
  resetLayout(): void {
    this.positions.clear();
  }

  // ── internals ──────────────────────────────────────────────────────────────

  /**
   * Translate a BuiltNode's attributes into Sigma node attributes, applying the
   * impact depth tint to the colour's alpha (deeper ⇒ more transparent). A
   * remembered position wins over the transform's seed so layouts are stable.
   */
  private nodeAttributes(node: BuiltNode): Record<string, unknown> {
    const a = node.attributes;
    const pos = this.positions.get(node.key) ?? { x: a.x, y: a.y };
    // depthTint (impact mode) fades deeper rings; absent ⇒ full opacity. The
    // accented target has tint 1, so it stays solid.
    const color =
      a.depthTint !== undefined && !a.isTarget ? withAlpha(a.color, a.depthTint) : a.color;
    return {
      label: a.label,
      x: pos.x,
      y: pos.y,
      size: a.size,
      color,
      // carried through for tooltips / interactions:
      plane: a.plane,
      kind: a.kind,
      path: a.path,
      nodeType: a.isTarget ? "target" : "normal",
    };
  }

  /**
   * Run a bounded ForceAtlas2 over `graph`. Nodes with remembered positions are
   * already seeded there; ForceAtlas2 refines all of them but the budget is
   * small so survivors barely move. Skips entirely for a trivial graph.
   */
  private layout(graph: Graph): void {
    if (graph.order < 3) return; // nothing meaningful to lay out
    forceAtlas2.assign(graph, {
      iterations: FA2_ITERATIONS,
      settings: {
        gravity: 1,
        scalingRatio: 10,
        slowDown: 1 + Math.log(graph.order + 1),
        barnesHutOptimize: graph.order > 200,
      },
    });
  }

  /** Remember every node's resolved position for the next render. */
  private remember(graph: Graph): void {
    const next = new Map<string, Pos>();
    graph.forEachNode((key, attrs) => {
      next.set(key, { x: attrs.x as number, y: attrs.y as number });
    });
    this.positions = next;
  }

  /** Create the Sigma instance on first render, or swap its graph thereafter. */
  private mountOrRefresh(): void {
    if (!this.sigma) {
      // Resolved at mount (not import) time so the stylesheet is in effect.
      const theme = resolveTheme(this.container);
      this.sigma = new Sigma(this.graph, this.container, {
        renderEdgeLabels: false,
        defaultEdgeType: "line",
        labelDensity: 0.6,
        labelRenderedSizeThreshold: 6,
        zIndex: true,
        labelColor: { color: theme.label },
        labelFont: theme.font,
        labelWeight: "500",
        defaultDrawNodeHover: makeHoverDrawer(theme),
      });
      this.bindEvents(this.sigma);
    } else {
      // Sigma can't swap its graph reference; clear + repopulate the live one.
      const live = this.sigma.getGraph();
      live.clear();
      this.graph.forEachNode((key, attrs) => live.addNode(key, attrs));
      this.graph.forEachEdge((key, attrs, source, target) => {
        if (!live.hasEdge(key)) live.addEdgeWithKey(key, source, target, attrs);
      });
      this.sigma.refresh();
    }
  }

  /** Forward Sigma node/stage events to the controller's handlers. */
  private bindEvents(sigma: Sigma): void {
    sigma.on("clickNode", ({ node }) => this.handlers.onSelect(node));
    sigma.on("doubleClickNode", ({ node, event }) => {
      // Stop the double-click from also zooming the camera (Sigma's default).
      event.preventSigmaDefault();
      this.handlers.onReroot(node);
    });

    // Hover: surface the node's name + path as the native tooltip on the canvas.
    sigma.on("enterNode", ({ node }) => {
      const attrs = sigma.getGraph().getNodeAttributes(node);
      const label = (attrs.label as string) ?? node;
      const path = (attrs.path as string) ?? "";
      this.container.title = path ? `${label} — ${path}` : label;
      this.container.style.cursor = "pointer";
    });
    sigma.on("leaveNode", () => {
      this.container.title = "";
      this.container.style.cursor = "default";
    });
  }
}
