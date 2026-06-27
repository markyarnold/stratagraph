// StrataGraph desktop UI controller (vanilla TS, no framework).
//
// Wires the shell in index.html to the typed backend bindings in api.ts:
//   open → load a graph/estate and show its summary
//   search → query() → results list
//   select a result → Context (callers/callees/imports/members) + Impact tabs
// Errors surface in the banner, never silently.

import * as api from "./api";
import type {
  AffectedNode,
  ExplainResult,
  MemberDependent,
  NodeRef,
  OpenInfo,
  SubgraphNode,
} from "./api";
import { formatHop, membersHint } from "./api";
import { buildImpact, buildNeighborhood, type BuiltGraph } from "./graphview/build";
import { GraphView } from "./graphview/render";

// ── tiny DOM helpers ─────────────────────────────────────────────────────────

function el<T extends HTMLElement>(id: string): T {
  const node = document.getElementById(id);
  if (!node) throw new Error(`missing #${id}`);
  return node as T;
}

function clear(node: HTMLElement): void {
  node.replaceChildren();
}

/** Create an element with optional class, text, and attributes. */
function make<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  opts: { class?: string; text?: string; attrs?: Record<string, string> } = {},
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  if (opts.class) node.className = opts.class;
  if (opts.text !== undefined) node.textContent = opts.text;
  if (opts.attrs) {
    for (const [k, v] of Object.entries(opts.attrs)) node.setAttribute(k, v);
  }
  return node;
}

// ── element handles ──────────────────────────────────────────────────────────

const els = {
  openProject: el<HTMLButtonElement>("open-project"),
  openGraph: el<HTMLButtonElement>("open-graph"),
  openWorkspace: el<HTMLButtonElement>("open-workspace"),
  reindex: el<HTMLButtonElement>("reindex"),
  summary: el<HTMLDivElement>("summary"),
  banner: el<HTMLDivElement>("banner"),
  search: el<HTMLInputElement>("search"),
  resultsMeta: el<HTMLDivElement>("results-meta"),
  results: el<HTMLUListElement>("results"),
  selection: el<HTMLDivElement>("selection"),
  tabContextBtn: document.querySelector<HTMLButtonElement>('.tab[data-tab="context"]')!,
  tabImpactBtn: document.querySelector<HTMLButtonElement>('.tab[data-tab="impact"]')!,
  tabGraphBtn: document.querySelector<HTMLButtonElement>('.tab[data-tab="graph"]')!,
  tabContext: el<HTMLDivElement>("tab-context"),
  tabImpact: el<HTMLDivElement>("tab-impact"),
  tabGraph: el<HTMLDivElement>("tab-graph"),
  contextBody: el<HTMLDivElement>("context-body"),
  impactDepth: el<HTMLInputElement>("impact-depth"),
  impactMinConf: el<HTMLInputElement>("impact-minconf"),
  impactContracts: el<HTMLInputElement>("impact-contracts"),
  impactInfra: el<HTMLInputElement>("impact-infra"),
  impactRun: el<HTMLButtonElement>("impact-run"),
  impactBody: el<HTMLDivElement>("impact-body"),
  // Graph tab
  graphModeNeighborhood: el<HTMLButtonElement>("graph-mode-neighborhood"),
  graphModeImpact: el<HTMLButtonElement>("graph-mode-impact"),
  graphDepth: el<HTMLSelectElement>("graph-depth"),
  planeCode: el<HTMLInputElement>("plane-code"),
  planeContract: el<HTMLInputElement>("plane-contract"),
  planeInfra: el<HTMLInputElement>("plane-infra"),
  planeData: el<HTMLInputElement>("plane-data"),
  graphRefresh: el<HTMLButtonElement>("graph-refresh"),
  graphTruncated: el<HTMLSpanElement>("graph-truncated"),
  graphLegend: el<HTMLDivElement>("graph-legend"),
  graphStage: el<HTMLDivElement>("graph-stage"),
  graphHint: el<HTMLDivElement>("graph-hint"),
};

// ── app state ────────────────────────────────────────────────────────────────

let graphLoaded = false;
/** The currently-selected node (drives the Context/Impact panels). */
let selected: NodeRef | null = null;
/** True while a reindex / index-now is in flight (drives the busy UI). */
let indexing = false;

// ── graph-view state ─────────────────────────────────────────────────────────

type GraphMode = "neighborhood" | "impact";
let graphMode: GraphMode = "neighborhood";
/** The node the graph view is currently rooted at (re-root changes this). */
let graphFocus: NodeRef | null = null;
/** Which tab is showing (so a new selection only re-renders the graph if live). */
let activeTab: "context" | "impact" | "graph" = "context";
/** The Sigma renderer, created lazily on first graph render. */
let graphView: GraphView | null = null;
/** Last subgraph nodes by uid, so a re-root event can recover a NodeRef. */
let lastGraphNodes = new Map<string, SubgraphNode>();

// ── banner ────────────────────────────────────────────────────────────────────

function showError(message: string): void {
  els.banner.hidden = false;
  els.banner.className = "banner error";
  els.banner.textContent = message;
}

function showInfo(message: string): void {
  els.banner.hidden = false;
  els.banner.className = "banner info";
  els.banner.textContent = message;
}

function clearBanner(): void {
  els.banner.hidden = true;
  els.banner.textContent = "";
}

/** Normalise any thrown value (Tauri rejects with the Err string) to text. */
function errText(e: unknown): string {
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  return String(e);
}

// ── open ──────────────────────────────────────────────────────────────────────

function renderSummary(info: OpenInfo): void {
  clear(els.summary);
  els.summary.classList.remove("muted");

  const head = make("div", {
    text: `${info.source} — ${info.nodes} nodes, ${info.edges} edges · engine ${info.engine}`,
  });
  els.summary.append(head);

  // Per-repo statuses (estate open). A single-db open has one ok entry.
  if (info.repos.length > 0) {
    const repos = make("div");
    info.repos.forEach((r, i) => {
      if (i > 0) repos.append(document.createTextNode("  ·  "));
      const span = make("span", {
        class: r.ok ? "repo-ok" : "repo-bad",
        text: r.ok ? `✓ ${r.name}` : `✗ ${r.name}`,
        attrs: r.error ? { title: r.error } : {},
      });
      repos.append(span);
    });
    els.summary.append(repos);
  }
}

/** Apply the freshly-loaded graph to the UI: summary, reset working area,
 *  enable controls, surface estate per-repo failures. Shared by open + index. */
function applyLoaded(info: OpenInfo): void {
  graphLoaded = true;
  renderSummary(info);
  els.reindex.disabled = false;

  // Reset the working area for the new graph.
  els.search.disabled = false;
  els.search.value = "";
  clear(els.results);
  els.resultsMeta.textContent = "";
  setSelection(null);

  // Tear down any prior graph render (it belongs to the old graph).
  graphFocus = null;
  lastGraphNodes = new Map();
  graphView?.destroy();
  graphView = null;

  // Surface any per-repo load failures (estate) without blocking the rest.
  const failed = info.repos.filter((r) => !r.ok);
  if (failed.length > 0) {
    showInfo(
      `Loaded with ${failed.length} repo(s) failed: ${failed
        .map((r) => r.name)
        .join(", ")} (hover the summary for details).`,
    );
  }
}

async function doOpen(path: string): Promise<void> {
  clearBanner();
  try {
    const info = await api.open(path);
    applyLoaded(info);
  } catch (e) {
    const msg = errText(e);
    // A folder with no index dead-ends here — offer Index Now instead of a flat
    // error (detected structurally via the marker prefix, never by regex on the
    // folder name).
    if (api.isNoIndexError(msg)) {
      offerIndexNow(path);
    } else {
      showError(`Open failed: ${msg}`);
    }
  }
}

/** Banner offering an **Index Now** button for a folder that has no index yet. */
function offerIndexNow(dir: string): void {
  els.banner.hidden = false;
  els.banner.className = "banner info";
  clear(els.banner);
  els.banner.append(
    make("span", {
      text: `No StrataGraph index found in ${dir}. `,
    }),
  );
  const btn = make("button", { class: "banner-action", text: "Index Now" });
  btn.addEventListener("click", () => void doIndexPath(dir));
  els.banner.append(btn);
}

// ── reindex / index-now (busy state) ────────────────────────────────────────

/** Toggle the busy UI: disable open/reindex/search while indexing runs. The
 *  loaded graph stays QUERYABLE on the backend, but we freeze new actions so a
 *  second request can't race (the backend also single-flights). */
function setIndexingBusy(busy: boolean): void {
  indexing = busy;
  els.openProject.disabled = busy;
  els.openGraph.disabled = busy;
  els.openWorkspace.disabled = busy;
  // Reindex is only meaningful when a graph is loaded; keep it disabled while
  // busy and re-enable on completion only if something is loaded.
  els.reindex.disabled = busy || !graphLoaded;
}

/** Replace the summary with an indeterminate "indexing" note (no percentage —
 *  the engine emits no progress). Honest about the old graph staying usable. */
function showIndexingSummary(): void {
  clear(els.summary);
  els.summary.classList.remove("muted");
  els.summary.append(
    make("div", {
      class: "indexing-note",
      text: "Indexing — the loaded graph stays usable; queries run against it until the rebuild finishes.",
    }),
  );
}

/**
 * After a reindex swaps the graph: re-run the active search so results reflect
 * the new graph, and decide the selection's fate — keep it (and refresh Context)
 * if its uid still exists, else clear it. Uid existence is probed with a depth-0
 * `subgraph` call (it errors on an unknown uid), so "survives" is exact.
 */
async function refreshAfterReindex(): Promise<void> {
  // Re-run the current search against the new graph (if any text is present).
  if (els.search.value.trim().length > 0) {
    await runSearch(els.search.value);
  }

  if (!selected) return;
  let survived = false;
  try {
    await api.subgraph(selected.uid, { depth: 0 });
    survived = true;
  } catch {
    survived = false;
  }

  if (survived) {
    // Keep the selection; reload its Context against the new graph. Drop any
    // stale graph layout so a Graph-tab re-render uses fresh data.
    graphView?.resetLayout();
    await loadContext(selected);
    if (activeTab === "graph") await renderGraph();
  } else {
    // The selected node vanished in the rebuild — clear it honestly.
    setSelection(null);
    graphFocus = null;
    els.results
      .querySelectorAll(".result.selected")
      .forEach((n) => n.classList.remove("selected"));
  }
}

/** Reindex the loaded source (Reindex button). Busy state + atomic swap result. */
async function doReindex(): Promise<void> {
  if (!graphLoaded || indexing) return;
  clearBanner();
  setIndexingBusy(true);
  showIndexingSummary();
  try {
    const info = await api.reindex();
    renderSummary(info);
    await refreshAfterReindex();
    const failed = info.repos.filter((r) => !r.ok);
    if (failed.length > 0) {
      showInfo(
        `Reindexed with ${failed.length} repo(s) failed: ${failed
          .map((r) => r.name)
          .join(", ")} (hover the summary for details).`,
      );
    }
  } catch (e) {
    // The old graph is still loaded and queryable on the backend (the swap only
    // happens on success), so the selection/results stay valid — we just surface
    // the error and note that the previous graph is still active.
    showError(`Reindex failed: ${errText(e)}`);
    els.summary.classList.add("muted");
    els.summary.textContent =
      "Reindex failed — the previously-loaded graph is still active.";
  } finally {
    setIndexingBusy(false);
  }
}

/** Index a folder with no index yet (Index Now), then load it. Same busy state. */
async function doIndexPath(dir: string): Promise<void> {
  if (indexing) return;
  clearBanner();
  setIndexingBusy(true);
  showIndexingSummary();
  try {
    const info = await api.indexPath(dir);
    applyLoaded(info);
  } catch (e) {
    showError(`Index failed: ${errText(e)}`);
    els.summary.classList.add("muted");
    els.summary.textContent = "Indexing failed.";
  } finally {
    setIndexingBusy(false);
  }
}

// ── search ─────────────────────────────────────────────────────────────────────

function kindChip(kind: string): HTMLElement {
  return make("span", { class: "chip", text: kind });
}

function renderResults(matches: NodeRef[]): void {
  clear(els.results);
  els.resultsMeta.textContent =
    matches.length === 0 ? "No matches." : `${matches.length} match(es)`;

  for (const m of matches) {
    const li = make("li", { class: "result" });
    li.append(make("span", { class: "name", text: m.name }));
    li.append(kindChip(m.kind));
    li.append(make("span", { class: "path", text: m.path }));
    li.addEventListener("click", () => {
      // Visual selection.
      els.results
        .querySelectorAll(".result.selected")
        .forEach((n) => n.classList.remove("selected"));
      li.classList.add("selected");
      void selectNode(m);
    });
    els.results.append(li);
  }
}

let searchTimer: number | undefined;

async function runSearch(text: string): Promise<void> {
  if (!graphLoaded) return;
  const trimmed = text.trim();
  if (trimmed.length === 0) {
    clear(els.results);
    els.resultsMeta.textContent = "";
    return;
  }
  try {
    const res = await api.query(trimmed);
    renderResults(res.matches);
  } catch (e) {
    showError(`Search failed: ${errText(e)}`);
  }
}

// ── selection + tabs ───────────────────────────────────────────────────────────

function setSelection(node: NodeRef | null): void {
  selected = node;
  clear(els.selection);
  if (!node) {
    els.selection.classList.add("muted");
    els.selection.textContent = "Select a result to inspect it.";
    els.contextBody.className = "context-body muted";
    els.contextBody.textContent = "No selection.";
    els.impactBody.className = "impact-body muted";
    els.impactBody.textContent = "No selection.";
    return;
  }
  els.selection.classList.remove("muted");
  els.selection.append(make("span", { class: "sel-name", text: node.name }));
  els.selection.append(kindChip(node.kind));
  els.selection.append(make("div", { class: "sel-path", text: node.path }));
}

/** The identifier we pass to context/impact: prefer the uid's fqn-ish name. */
function symbolOf(node: NodeRef): string {
  // `query` returns name; context/impact resolve by fqn-or-name. The node name
  // is the safest broadly-resolvable token the payload carries.
  return node.name;
}

async function selectNode(node: NodeRef): Promise<void> {
  setSelection(node);
  clearBanner();
  // A fresh selection re-roots the graph view (and drops the remembered layout).
  graphFocus = node;
  graphView?.resetLayout();
  await loadContext(node);
  // Impact is run on demand (button) so depth/conf controls apply; clear stale.
  els.impactBody.className = "impact-body muted";
  els.impactBody.textContent = "Run impact to compute the blast radius.";
  // Keep the graph in sync if it is the visible tab.
  if (activeTab === "graph") void renderGraph();
}

function activateTab(tab: "context" | "impact" | "graph"): void {
  activeTab = tab;
  els.tabContextBtn.classList.toggle("active", tab === "context");
  els.tabImpactBtn.classList.toggle("active", tab === "impact");
  els.tabGraphBtn.classList.toggle("active", tab === "graph");
  els.tabContext.hidden = tab !== "context";
  els.tabImpact.hidden = tab !== "impact";
  els.tabGraph.hidden = tab !== "graph";

  // Entering the Graph tab renders the current selection (Sigma needs the
  // container to be visible/sized before it mounts, so we defer to here).
  if (tab === "graph") void renderGraph();
}

// ── context rendering ──────────────────────────────────────────────────────────

/** A labelled bucket: a heading plus a name/kind/path table (or an empty note). */
function bucket(title: string, nodes: NodeRef[]): HTMLElement {
  const wrap = make("div", { class: "bucket" });
  wrap.append(make("h3", { text: `${title} (${nodes.length})` }));
  if (nodes.length === 0) {
    wrap.append(make("div", { class: "empty", text: "none" }));
    return wrap;
  }
  const table = make("table");
  const thead = make("thead");
  const hr = make("tr");
  for (const h of ["Name", "Kind", "Path"]) hr.append(make("th", { text: h }));
  thead.append(hr);
  table.append(thead);

  const tbody = make("tbody");
  for (const n of nodes) {
    const tr = make("tr", { class: "clickable" });
    tr.append(make("td", { class: "name", text: n.name }));
    const kindTd = make("td");
    kindTd.append(kindChip(n.kind));
    tr.append(kindTd);
    tr.append(make("td", { class: "col-path", text: n.path }));
    tr.addEventListener("click", () => void selectNode(n));
    tbody.append(tr);
  }
  table.append(tbody);
  wrap.append(table);
  return wrap;
}

async function loadContext(node: NodeRef): Promise<void> {
  els.contextBody.className = "context-body";
  clear(els.contextBody);
  els.contextBody.append(make("div", { class: "muted", text: "Loading…" }));
  try {
    const ctx = await api.context(symbolOf(node));
    clear(els.contextBody);

    if (ctx.ambiguous) {
      els.contextBody.append(
        make("div", {
          class: "muted",
          text: `“${ctx.symbol}” is ambiguous — ${
            ctx.candidates?.length ?? 0
          } candidates. Pick one:`,
        }),
      );
      els.contextBody.append(bucket("Candidates", ctx.candidates ?? []));
      return;
    }

    if (ctx.container) {
      els.contextBody.append(bucket("Container", [ctx.container]));
    }
    // Contract plane first — for a schema field/operation these are the buckets
    // that apply. Always rendered (empty → "none"), so a dead field honestly
    // shows Producers (0) / Consumers (0) instead of hiding it behind the code
    // buckets. Fixed order, no plane re-derivation.
    els.contextBody.append(bucket("Producers", ctx.producers ?? []));
    els.contextBody.append(bucket("Consumers", ctx.consumers ?? []));
    els.contextBody.append(bucket("Produces", ctx.produces ?? []));
    els.contextBody.append(bucket("Consumes", ctx.consumes ?? []));
    // Infra plane next — for an IamRole, Assumed by lists its Lambdas (the
    // headline fix); the resolver→DS→lambda chain shows from both ends; a
    // handler module's Run by lists its Lambda. Fixed order, always rendered.
    els.contextBody.append(bucket("Assumes", ctx.assumes ?? []));
    els.contextBody.append(bucket("Assumed by", ctx.assumed_by ?? []));
    els.contextBody.append(bucket("Routes to", ctx.routes_to ?? []));
    els.contextBody.append(bucket("Routed from", ctx.routed_from ?? []));
    els.contextBody.append(bucket("Runs", ctx.runs ?? []));
    els.contextBody.append(bucket("Run by", ctx.run_by ?? []));
    // Data plane — for a Table, Mapped by lists the ORM model classes that map to
    // it; for a model class, Maps to is its Table. Fixed order, always rendered.
    els.contextBody.append(bucket("Mapped by", ctx.mapped_by ?? []));
    els.contextBody.append(bucket("Maps to", ctx.maps_to ?? []));
    els.contextBody.append(bucket("Callers", ctx.callers ?? []));
    els.contextBody.append(bucket("Callees", ctx.callees ?? []));
    els.contextBody.append(bucket("Imports in", ctx.imports_in ?? []));
    els.contextBody.append(bucket("Imports out", ctx.imports_out ?? []));
    els.contextBody.append(bucket("Members", ctx.members ?? []));
  } catch (e) {
    els.contextBody.className = "context-body";
    clear(els.contextBody);
    showError(`Context failed: ${errText(e)}`);
    els.contextBody.append(
      make("div", { class: "muted", text: `Context failed: ${errText(e)}` }),
    );
  }
}

// ── impact rendering ───────────────────────────────────────────────────────────

/** The impact parameters used for a run, so each row's Explain reuses them
 *  verbatim (same target symbol, depth, and contract/infra toggles → the
 *  explained confidence matches the row's confidence). `uid` pins the target when
 *  the symbol was ambiguous (a candidate was picked) or when re-running on a
 *  specific member — it flows into the impact call exactly as MCP/CLI's uid pin. */
interface ExplainContext {
  symbol: string;
  uid?: string;
  depth?: number;
  min_confidence?: number;
  include_contracts: boolean;
  include_infra: boolean;
}

function renderImpact(
  affected: AffectedNode[],
  minConf: number,
  ctx: ExplainContext,
  members: MemberDependent[] = [],
): void {
  els.impactBody.className = "impact-body";
  clear(els.impactBody);

  els.impactBody.append(
    make("div", {
      class: "impact-summary",
      text: `${affected.length} affected node(s). Ambiguous and low-confidence (< ${minConf || 0.5}) rows are shown dashed/muted. Click Explain on a row for its evidence chain.`,
    }),
  );

  if (affected.length === 0) {
    // Honest zero-direct surfacing (parity with CLI render_zero_affected + the MCP
    // members_with_dependents field): if the target is member-bearing and some of
    // its members DO have dependents, say so and let the user re-run on a member —
    // never reprint the misleading bare "nothing depends on this".
    if (members.length > 0) {
      els.impactBody.append(
        make("div", { class: "impact-members-hint", text: membersHint(ctx.symbol, members) }),
      );
      const list = make("ul", { class: "impact-members" });
      for (const m of members) {
        const li = make("li");
        const btn = make("button", {
          class: "member-impact-btn",
          text: `impact ${m.name}`,
        });
        // Re-run impact pinned to this member's uid (exact node), same params.
        btn.addEventListener("click", () => void runImpactOnMember(m, ctx));
        li.append(btn);
        li.append(kindChip(m.kind));
        list.append(li);
      }
      els.impactBody.append(list);
      return;
    }
    els.impactBody.append(
      make("div", {
        class: "muted",
        text: "Nothing affected (nothing depends on this within the given depth/confidence).",
      }),
    );
    return;
  }

  const table = make("table");
  const thead = make("thead");
  const hr = make("tr");
  for (const h of ["Name", "Depth", "Confidence", "Verdict", "Flags", "Why"]) {
    hr.append(make("th", { text: h }));
  }
  thead.append(hr);
  table.append(thead);

  // Lower-confidence rows (below this) are muted even when above the server
  // min_confidence cut — a visual triage hint, not a filter.
  const lowConfMark = minConf > 0 ? minConf : 0.5;

  const tbody = make("tbody");
  const sorted = [...affected].sort(
    (a, b) => a.depth - b.depth || b.confidence - a.confidence,
  );
  for (const a of sorted) {
    const classes: string[] = [];
    if (a.ambiguous) classes.push("ambiguous");
    if (a.confidence < lowConfMark) classes.push("lowconf");
    const tr = make("tr", { class: classes.join(" ") });
    tr.append(make("td", { class: "name", text: a.name }));
    tr.append(make("td", { text: String(a.depth) }));
    tr.append(make("td", { class: "conf-cell", text: a.confidence.toFixed(2) }));
    // The §15.6 verdict column: the engine's will-break call, echoed verbatim.
    tr.append(
      make("td", {
        class: a.will_break ? "verdict will-break" : "verdict may-affect",
        text: api.breakVerdict(a.will_break),
      }),
    );
    tr.append(
      make("td", { text: a.ambiguous ? "ambiguous" : "" }),
    );
    // The Explain affordance — why is THIS node in the blast radius? A toggle:
    // first click loads the evidence chain into a detail row beneath this one;
    // a second click hides it.
    const whyTd = make("td");
    const btn = make("button", { class: "explain-btn", text: "Explain" });
    const detail = make("tr", { class: "explain-detail", attrs: { hidden: "" } });
    const detailCell = make("td", { attrs: { colspan: "6" } });
    detail.append(detailCell);
    let loaded = false;
    btn.addEventListener("click", () => {
      const showing = !detail.hidden;
      if (showing) {
        detail.hidden = true;
        return;
      }
      detail.hidden = false;
      if (!loaded) {
        loaded = true;
        void loadExplainInto(detailCell, ctx, a.name);
      }
    });
    whyTd.append(btn);
    tr.append(whyTd);
    tbody.append(tr);
    tbody.append(detail);
  }
  table.append(tbody);
  els.impactBody.append(table);
}

/**
 * Fetch and render the evidence chain for `affectedName` (within `ctx`'s impact
 * parameters) into `cell`. AMBIGUOUS hops reuse the amber/dashed encoding; an
 * unreachable node renders the honest "not in blast radius" note rather than an
 * empty panel.
 */
async function loadExplainInto(
  cell: HTMLElement,
  ctx: ExplainContext,
  affectedName: string,
): Promise<void> {
  clear(cell);
  cell.append(make("div", { class: "muted", text: "Explaining…" }));
  try {
    const res: ExplainResult = await api.explain(ctx.symbol, affectedName, {
      depth: ctx.depth,
      min_confidence: ctx.min_confidence,
      include_contracts: ctx.include_contracts,
      include_infra: ctx.include_infra,
    });
    clear(cell);
    renderExplanation(cell, res);
  } catch (e) {
    clear(cell);
    cell.append(
      make("div", { class: "muted", text: `Explain failed: ${errText(e)}` }),
    );
  }
}

/** Render one [`ExplainResult`] into `cell`: a header line, then one line per
 *  hop (AMBIGUOUS hops visually distinct), or the honest unreachable note. */
function renderExplanation(cell: HTMLElement, res: ExplainResult): void {
  const panel = make("div", { class: "explain-panel" });

  if (!res.reachable) {
    panel.classList.add("explain-unreachable");
    panel.append(
      make("div", {
        class: "explain-reason",
        text:
          res.reason ??
          `${res.affected.name} is not in ${res.target.name}'s blast radius (nothing to explain).`,
      }),
    );
    cell.append(panel);
    return;
  }

  const conf = res.confidence ?? 1;
  const verdict = api.breakVerdict(res.will_break ?? false);
  const ambNote = res.ambiguous ? ", via AMBIGUOUS" : "";
  panel.append(
    make("div", {
      class: "explain-header",
      text: `Why ${res.target.name} affects ${res.affected.name} (conf ${conf.toFixed(2)}, ${verdict}${ambNote})`,
    }),
  );

  const hops = res.hops ?? [];
  if (hops.length === 0) {
    panel.append(
      make("div", {
        class: "explain-hop",
        text: "(the target is the affected node — nothing to traverse)",
      }),
    );
    cell.append(panel);
    return;
  }

  // Resolve a hop endpoint's uid to its friendly name when the chain happens to
  // reference a node we already know (the target/affected); otherwise show the
  // uid. The hop endpoints are uids; the names we have are target/affected.
  const known = new Map<string, string>([
    [res.target.uid, res.target.name],
    [res.affected.uid, res.affected.name],
  ]);
  const nameOf = (uid: string) => known.get(uid) ?? uid;

  for (const hop of hops) {
    const line = make("div", {
      class:
        hop.provenance === "Ambiguous"
          ? "explain-hop ambiguous-hop"
          : "explain-hop",
      text: formatHop(hop, nameOf),
    });
    panel.append(line);
  }
  cell.append(panel);
}

async function runImpact(): Promise<void> {
  if (!selected) {
    showError("Select a node before running impact.");
    return;
  }
  clearBanner();
  const depth = Number(els.impactDepth.value);
  const minConf = Number(els.impactMinConf.value);
  // The exact parameters of this run, so each row's Explain reuses them verbatim.
  const explainCtx: ExplainContext = {
    symbol: symbolOf(selected),
    depth: Number.isFinite(depth) ? depth : undefined,
    min_confidence: Number.isFinite(minConf) ? minConf : undefined,
    include_contracts: els.impactContracts.checked,
    include_infra: els.impactInfra.checked,
  };
  await executeImpact(explainCtx, minConf);
}

/**
 * Run impact for `ctx` and render the outcome. Three shapes, all handled (parity
 * with CLI/MCP):
 *  - ambiguous symbol → list candidates, each re-runnable pinned to its uid
 *    (mirrors the Context ambiguity handling), instead of throwing "Impact failed";
 *  - resolved with dependents → the affected table;
 *  - resolved with 0 dependents → the members hint (when members have dependents)
 *    or the honest "nothing affected" line.
 */
async function executeImpact(ctx: ExplainContext, minConf: number): Promise<void> {
  els.impactBody.className = "impact-body muted";
  els.impactBody.textContent = "Computing…";
  try {
    const res = await api.impact(ctx.symbol, {
      uid: ctx.uid,
      depth: ctx.depth,
      min_confidence: ctx.min_confidence,
      include_contracts: ctx.include_contracts,
      include_infra: ctx.include_infra,
    });
    // Pure classifier decides the shape, so the ambiguous case is handled (never a
    // throw) and the members hint and empty cases are explicit.
    const outcome = api.impactOutcome(res);
    switch (outcome.kind) {
      case "ambiguous":
        renderImpactAmbiguity(outcome.symbol || ctx.symbol, outcome.candidates, ctx, minConf);
        break;
      case "affected":
        renderImpact(outcome.affected, minConf, ctx, []);
        break;
      case "members":
        renderImpact([], minConf, ctx, outcome.members);
        break;
      case "empty":
        renderImpact([], minConf, ctx, []);
        break;
    }
  } catch (e) {
    els.impactBody.className = "impact-body muted";
    els.impactBody.textContent = "";
    showError(`Impact failed: ${errText(e)}`);
  }
}

/** Re-run impact pinned to a specific member's uid (the zero-direct hint's "impact
 *  <member>" button) — same run parameters, target swapped to the member. */
async function runImpactOnMember(member: MemberDependent, ctx: ExplainContext): Promise<void> {
  clearBanner();
  await executeImpact({ ...ctx, symbol: member.name, uid: member.uid }, ctx.min_confidence ?? 0);
}

/**
 * Render an ambiguous impact result: the candidates, each as a row that re-runs
 * impact pinned to that candidate's uid. Mirrors the Context-tab ambiguity handling
 * so the GUI never dead-ends on a `Many` resolution.
 */
function renderImpactAmbiguity(
  symbol: string,
  candidates: NodeRef[],
  ctx: ExplainContext,
  minConf: number,
): void {
  els.impactBody.className = "impact-body";
  clear(els.impactBody);
  els.impactBody.append(
    make("div", {
      class: "muted",
      text: `“${symbol}” is ambiguous — ${candidates.length} candidates. Pick one to run impact on:`,
    }),
  );
  if (candidates.length === 0) return;
  const table = make("table", { class: "impact-candidates" });
  const tbody = make("tbody");
  for (const c of candidates) {
    const tr = make("tr", { class: "clickable" });
    const nameTd = make("td", { class: "name" });
    const btn = make("button", { class: "candidate-impact-btn", text: c.name });
    // Pin THIS candidate's uid and re-run (CLI --uid / MCP uid parity).
    btn.addEventListener("click", () =>
      void executeImpact({ ...ctx, symbol: c.name, uid: c.uid }, minConf),
    );
    nameTd.append(btn);
    tr.append(nameTd);
    const kindTd = make("td");
    kindTd.append(kindChip(c.kind));
    tr.append(kindTd);
    tr.append(make("td", { class: "col-path", text: c.path }));
    tbody.append(tr);
  }
  table.append(tbody);
  els.impactBody.append(table);
}

// ── graph view ─────────────────────────────────────────────────────────────────

/** The plane filter from the checkboxes, or `undefined` when all are checked. */
function selectedPlanes(): string[] | undefined {
  const planes: string[] = [];
  if (els.planeCode.checked) planes.push("code");
  if (els.planeContract.checked) planes.push("contract");
  if (els.planeInfra.checked) planes.push("infra");
  if (els.planeData.checked) planes.push("data");
  // All four on ⇒ no filter (let the server return everything).
  return planes.length === 4 ? undefined : planes;
}

function setGraphMode(mode: GraphMode): void {
  graphMode = mode;
  els.graphModeNeighborhood.classList.toggle("active", mode === "neighborhood");
  els.graphModeImpact.classList.toggle("active", mode === "impact");
  // Depth only applies to the neighbourhood walk; impact uses the engine's depth.
  els.graphDepth.disabled = mode === "impact";
}

function setGraphHint(text: string | null): void {
  if (text === null) {
    els.graphHint.hidden = true;
    return;
  }
  els.graphHint.hidden = false;
  els.graphHint.textContent = text;
}

function setTruncated(truncated: boolean, shown: number): void {
  els.graphTruncated.hidden = !truncated;
  if (truncated) {
    els.graphTruncated.textContent = `truncated — showing ${shown} (cap reached)`;
  }
}

function renderLegend(built: BuiltGraph): void {
  clear(els.graphLegend);
  for (const entry of built.legend) {
    const item = make("span", { class: "legend-item" });
    const swatch = make("span", { class: "legend-swatch" });
    swatch.style.background = entry.color;
    item.append(swatch);
    item.append(make("span", { text: entry.label }));
    els.graphLegend.append(item);
  }
}

/** Remember the rendered nodes so a re-root (uid-only event) can find a NodeRef. */
function rememberNodes(nodes: SubgraphNode[]): void {
  lastGraphNodes = new Map(nodes.map((n) => [n.uid, n]));
}

/** Lazily create the Sigma renderer bound to the stage container. */
function ensureGraphView(): GraphView {
  if (!graphView) {
    graphView = new GraphView(els.graphStage, {
      onSelect: (uid) => void onGraphSelect(uid),
      onReroot: (uid) => void onGraphReroot(uid),
    });
  }
  return graphView;
}

/** Click on a graph node → make it the selection (loads Context, syncs panels). */
async function onGraphSelect(uid: string): Promise<void> {
  const n = lastGraphNodes.get(uid);
  if (!n) return;
  // Don't reset the focus on a plain click — only update the inspected node.
  const node: NodeRef = { uid: n.uid, name: n.name, kind: n.kind, path: n.path };
  setSelection(node);
  await loadContext(node);
  // Sync the result-list highlight if the node is visible there.
  els.results.querySelectorAll(".result.selected").forEach((x) => x.classList.remove("selected"));
}

/** Double-click on a graph node → re-root the neighbourhood/impact there. */
async function onGraphReroot(uid: string): Promise<void> {
  const n = lastGraphNodes.get(uid);
  if (!n) return;
  const node: NodeRef = { uid: n.uid, name: n.name, kind: n.kind, path: n.path };
  graphFocus = node;
  setSelection(node);
  graphView?.resetLayout();
  await loadContext(node);
  await renderGraph();
}

/**
 * Render the graph for the current focus + mode. Neighbourhood mode draws the
 * `subgraph` (depth + plane filters); impact mode joins the engine's real
 * `impact` result with a subgraph for shape. Never recomputes impact.
 */
async function renderGraph(): Promise<void> {
  const focus = graphFocus ?? selected;
  if (!graphLoaded || !focus) {
    setGraphHint("Select a node, then choose Neighbourhood or Impact to render its graph.");
    setTruncated(false, 0);
    clear(els.graphLegend);
    return;
  }

  setGraphHint("Rendering…");
  const planes = selectedPlanes();
  try {
    let built: BuiltGraph;
    if (graphMode === "impact") {
      // Real engine impact for the affected set + target uid. Pin the focus uid so
      // the resolution is unambiguous (focus came from the graph, so it always has
      // a real uid) — the result then always carries `target`/`affected`.
      const imp = await api.impact(symbolOf(focus), { uid: focus.uid });
      const targetUid = imp.target?.uid ?? focus.uid;
      const affected = imp.affected ?? [];
      // … and a subgraph at max depth for the node/edge shape to draw it on.
      const sub = await api.subgraph(targetUid, { depth: 3, planes });
      rememberNodes(sub.nodes);
      built = buildImpact({
        targetUid,
        affected: affected.map((a) => ({
          uid: a.uid,
          depth: a.depth,
          confidence: a.confidence,
          ambiguous: a.ambiguous,
        })),
        subgraph: sub,
      });
    } else {
      const depth = Number(els.graphDepth.value) || 2;
      const sub = await api.subgraph(focus.uid, { depth, planes });
      rememberNodes(sub.nodes);
      built = buildNeighborhood(sub, { focusUid: focus.uid });
    }

    if (built.nodes.length === 0) {
      setGraphHint("Nothing to show for this node with the current filters.");
      setTruncated(built.truncated, 0);
      clear(els.graphLegend);
      return;
    }

    setGraphHint(null);
    renderLegend(built);
    setTruncated(built.truncated, built.nodes.length);
    ensureGraphView().render(built);
  } catch (e) {
    setGraphHint(null);
    showError(`Graph failed: ${errText(e)}`);
    setTruncated(false, 0);
  }
}

// ── wiring ─────────────────────────────────────────────────────────────────────

function wire(): void {
  els.openProject.addEventListener("click", async () => {
    const path = await api.pickProjectFolder();
    if (path) await doOpen(path);
  });
  els.openGraph.addEventListener("click", async () => {
    const path = await api.pickGraphFile();
    if (path) await doOpen(path);
  });
  els.openWorkspace.addEventListener("click", async () => {
    const path = await api.pickWorkspaceFile();
    if (path) await doOpen(path);
  });
  els.reindex.addEventListener("click", () => void doReindex());

  els.search.addEventListener("input", () => {
    window.clearTimeout(searchTimer);
    searchTimer = window.setTimeout(() => void runSearch(els.search.value), 180);
  });

  els.tabContextBtn.addEventListener("click", () => activateTab("context"));
  els.tabImpactBtn.addEventListener("click", () => activateTab("impact"));
  els.tabGraphBtn.addEventListener("click", () => activateTab("graph"));
  els.impactRun.addEventListener("click", () => void runImpact());

  // Graph-tab controls. Mode/filters/depth all re-render the current focus.
  els.graphModeNeighborhood.addEventListener("click", () => {
    setGraphMode("neighborhood");
    void renderGraph();
  });
  els.graphModeImpact.addEventListener("click", () => {
    setGraphMode("impact");
    void renderGraph();
  });
  els.graphDepth.addEventListener("change", () => void renderGraph());
  for (const cb of [els.planeCode, els.planeContract, els.planeInfra, els.planeData]) {
    cb.addEventListener("change", () => {
      // A plane filter change re-queries the server, so the layout is fresh.
      graphView?.resetLayout();
      void renderGraph();
    });
  }
  els.graphRefresh.addEventListener("click", () => {
    graphView?.resetLayout();
    void renderGraph();
  });

  // Default-disable the depth selector if we boot in impact mode (we don't, but
  // keep the control state consistent with the mode).
  setGraphMode(graphMode);
}

wire();
