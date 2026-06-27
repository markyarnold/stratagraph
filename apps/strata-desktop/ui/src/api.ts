// Typed bindings over the StrataGraph Tauri backend.
//
// Each function here maps 1:1 to a `#[tauri::command]` and mirrors its Rust DTO,
// so the rest of the UI never touches `invoke` directly or guesses at payload
// shapes. The `tool` calls go through the SAME `strata_mcp::call_tool` dispatch
// the MCP server and CLI use, so the shapes below match those tools exactly.

import { invoke } from "@tauri-apps/api/core";
import { open as openDialog } from "@tauri-apps/plugin-dialog";

// ── open ────────────────────────────────────────────────────────────────────

export interface RepoStatus {
  name: string;
  ok: boolean;
  error?: string;
}

export interface OpenInfo {
  source: string;
  /** Engine build id (git hash) that produced this graph view. */
  engine: string;
  nodes: number;
  edges: number;
  repos: RepoStatus[];
}

/** A compact node, as `call_tool` emits it (query matches, context buckets). */
export interface NodeRef {
  uid: string;
  name: string;
  kind: string;
  path: string;
}

// ── query / context / impact payloads (shapes from strata_mcp::call_tool) ────

export interface QueryResult {
  matches: NodeRef[];
}

/** Either a resolved node's 360° context, or an ambiguous-candidates payload. */
export interface ContextResult {
  // present when resolved to one node:
  node?: NodeRef;
  callers?: NodeRef[];
  callees?: NodeRef[];
  imports_in?: NodeRef[];
  imports_out?: NodeRef[];
  members?: NodeRef[];
  container?: NodeRef | null;
  // contract plane — the buckets that apply to a schema field/operation:
  //   producers: who implements it (incoming PRODUCES)
  //   consumers: who queries it (incoming CONSUMES)
  //   produces:  what a handler implements (outgoing PRODUCES)
  //   consumes:  what a module calls (outgoing CONSUMES)
  producers?: NodeRef[];
  consumers?: NodeRef[];
  produces?: NodeRef[];
  consumes?: NodeRef[];
  // infra plane — the wiring that applies to a role/datasource/Lambda/module:
  //   assumes:     the IamRole a LambdaFn assumes (outgoing Assumes)
  //   assumed_by:  the Lambdas that assume an IamRole (incoming Assumes)
  //   routes_to:   a resolver's datasource / a datasource's LambdaFn (outgoing Routes)
  //   routed_from: the resolver/datasource routing in (incoming Routes)
  //   runs:        the code Module a LambdaFn runs (outgoing Runs)
  //   run_by:      the LambdaFn that runs a handler module (incoming Runs)
  assumes?: NodeRef[];
  assumed_by?: NodeRef[];
  routes_to?: NodeRef[];
  routed_from?: NodeRef[];
  runs?: NodeRef[];
  run_by?: NodeRef[];
  // data plane — the ORM mapping that applies to a Table / model class:
  //   mapped_by: the ORM model classes that map to a Table (incoming MapsTo)
  //   maps_to:   the Table a model class maps to (outgoing MapsTo)
  mapped_by?: NodeRef[];
  maps_to?: NodeRef[];
  // present when the symbol was ambiguous:
  ambiguous?: boolean;
  symbol?: string;
  candidates?: NodeRef[];
}

export interface AffectedNode {
  uid: string;
  name: string;
  depth: number;
  confidence: number;
  ambiguous: boolean;
  // The §15.6 will-break verdict, computed server-side (strata_core's
  // will_break_label: confidence ≥ 0.40 and non-ambiguous) and echoed verbatim —
  // true = "will break", false = "may be affected, review".
  will_break: boolean;
}

/**
 * A member (method/column) of a member-bearing target that itself has dependents,
 * surfaced by `impact` only on the zero-direct case (mirrors the MCP
 * `members_with_dependents` field and the CLI hint). `kind` is the server-side
 * kind name. Lets the GUI point the user at a member to re-run `impact` on, instead
 * of reprinting a misleading "nothing depends on this".
 */
export interface MemberDependent {
  uid: string;
  name: string;
  kind: string;
}

/**
 * Either a resolved impact result (`target` + `affected`, optionally a
 * `members_with_dependents` hint when `affected` is empty), or — when the symbol
 * resolved to several nodes — the ambiguous-candidates payload (`ambiguous` +
 * `candidates`, no `target`/`affected`). The shape matches `call_tool`'s impact
 * dispatch exactly, the same way `ContextResult` carries its ambiguous variant.
 */
export interface ImpactResult {
  // present when resolved to one node:
  target?: NodeRef;
  affected?: AffectedNode[];
  // present (and non-empty) ONLY when `affected` is empty but the target's members
  // have dependents — the honest hint, never the type's own direct dependents.
  members_with_dependents?: MemberDependent[];
  // present when the symbol was ambiguous (mirrors ContextResult):
  ambiguous?: boolean;
  symbol?: string;
  candidates?: NodeRef[];
}

/**
 * The §15.6 will-break verdict as a table label — the GUI's word-for-word echo
 * of the engine's classification, matching the CLI's "WILL BREAK" / "may affect"
 * column. The boolean itself is computed and tested server-side.
 */
export function breakVerdict(willBreak: boolean): string {
  return willBreak ? "WILL BREAK" : "may affect";
}

/**
 * The classified outcome of an `impact` call — the pure decision the GUI renders,
 * factored out of the DOM so it is unit-testable and so the ambiguous shape can
 * never fall through to a throw:
 *  - `ambiguous`  → the symbol resolved to several nodes; render the candidates.
 *  - `affected`   → there are dependents; render the table.
 *  - `members`    → 0 direct dependents, but members have them; render the hint.
 *  - `empty`      → genuinely nothing depends on it.
 */
export type ImpactOutcome =
  | { kind: "ambiguous"; symbol: string; candidates: NodeRef[] }
  | { kind: "affected"; affected: AffectedNode[] }
  | { kind: "members"; members: MemberDependent[] }
  | { kind: "empty" };

/** Classify an [`ImpactResult`] into the shape the GUI should render. Pure (no
 *  DOM) — the testable core of `executeImpact`'s branching. */
export function impactOutcome(res: ImpactResult): ImpactOutcome {
  if (res.ambiguous) {
    return {
      kind: "ambiguous",
      symbol: res.symbol ?? "",
      candidates: res.candidates ?? [],
    };
  }
  const affected = res.affected ?? [];
  if (affected.length > 0) return { kind: "affected", affected };
  const members = res.members_with_dependents ?? [];
  if (members.length > 0) return { kind: "members", members };
  return { kind: "empty" };
}

/** How many member names the zero-direct hint lists before summarising the rest
 *  as a count — mirrors the CLI `MEMBER_HINT_MAX`. */
export const MEMBER_HINT_MAX = 5;

/**
 * The honest zero-direct hint, mirroring the CLI `render_zero_affected`: when a
 * member-bearing target has 0 direct dependents but some of its members do, say so
 * and name a few — never the misleading bare "nothing depends on this". Pure (no
 * DOM) so it is unit-testable; `members` must be non-empty (callers check first).
 */
export function membersHint(targetName: string, members: MemberDependent[]): string {
  const shown = members.slice(0, MEMBER_HINT_MAX).map((m) => m.name);
  const more = members.length - shown.length;
  const suffix = more > 0 ? `, … (+${more} more)` : "";
  return (
    `0 dependents on ${targetName} itself; ${members.length} of its members ` +
    `have dependents: ${shown.join(", ")}${suffix}`
  );
}

// ── explain payloads (the evidence chain — shapes from strata_mcp::call_tool) ──

/**
 * One edge on the best reaching path from the changed target to the affected
 * node, mirroring strata_core's `PathHop`. `from`/`to` are uids (display the node
 * names alongside), `confidence` is the single edge's own confidence, and
 * `running_confidence` is the accumulated product after this hop — the final
 * hop's value is the number `impact` reports (the consistency invariant).
 */
export interface PathHop {
  from: string;
  to: string;
  edge_kind: string;
  provenance: string;
  confidence: number;
  running_confidence: number;
}

/**
 * The `explain` tool result: **why is B in A's blast radius?** When `reachable`
 * is false the affected node is not in the blast radius (an honest negative, with
 * a `reason`), and there are no `hops`. When true, the `hops` chain plus the
 * overall `confidence` (== the affected node's impact confidence), `ambiguous`,
 * and `will_break` describe the evidence.
 */
export interface ExplainResult {
  target: NodeRef;
  affected: NodeRef;
  reachable: boolean;
  reason?: string;
  confidence?: number;
  ambiguous?: boolean;
  will_break?: boolean;
  hops?: PathHop[];
}

/**
 * Render one hop of the evidence chain as a single line — the GUI's pure,
 * tested formatter (the same shape the CLI prints): `from —KIND (Provenance
 * conf)→ to    running R`. `nameOf` maps a hop endpoint's uid to its display
 * name (falling back to the uid). Kept pure (no DOM) so it is unit-testable.
 */
export function formatHop(
  hop: PathHop,
  nameOf: (uid: string) => string,
): string {
  const kind = hop.edge_kind.toUpperCase();
  return (
    `${nameOf(hop.from)}  —${kind} (${hop.provenance} ${hop.confidence.toFixed(2)})→  ` +
    `${nameOf(hop.to)}    running ${hop.running_confidence.toFixed(2)}`
  );
}

// ── subgraph payloads (mirror src-tauri/src/subgraph.rs DTOs) ─────────────────

/**
 * One node in a subgraph result. `plane` is derived **server-side** from `kind`
 * (code / contract / infra) — the renderer colours by `plane` and must never
 * re-derive it from the kind.
 */
export interface SubgraphNode {
  uid: string;
  name: string;
  kind: string;
  path: string;
  plane: string;
}

/** One edge in a subgraph result, carrying the visual-encoding inputs. */
export interface SubgraphEdge {
  src: string;
  dst: string;
  kind: string;
  provenance: string;
  confidence: number;
}

/** The `subgraph` command result: nodes + edges + a truncation flag. */
export interface SubgraphDto {
  nodes: SubgraphNode[];
  edges: SubgraphEdge[];
  truncated: boolean;
}

// ── commands ─────────────────────────────────────────────────────────────────

/** Open a `.duckdb` graph file or a `strata.workspace.toml` estate manifest. */
export function open(path: string): Promise<OpenInfo> {
  return invoke<OpenInfo>("open", { path });
}

/**
 * Structured marker the backend prefixes onto the "no StrataGraph index found" open
 * error (`commands::NO_INDEX_PREFIX`). The UI keys its **Index Now** affordance
 * off this prefix — it never parses the folder name out of the message body.
 */
export const NO_INDEX_PREFIX = "NO_INDEX::";

/** Whether an open-error string is the structured "no index found" case. */
export function isNoIndexError(message: string): boolean {
  return message.startsWith(NO_INDEX_PREFIX);
}

/**
 * Rebuild the currently-loaded graph the same way the CLI's `index` command
 * does, then return the fresh summary. Rejects with "Indexing is already
 * running." if a reindex/index is already in flight, or "Open a project first."
 * when nothing is loaded.
 */
export function reindex(): Promise<OpenInfo> {
  return invoke<OpenInfo>("reindex");
}

/**
 * Index a folder that has no index yet (the Index Now affordance), then open it.
 * Shares the backend single-flight guard with {@link reindex}.
 */
export function indexPath(path: string): Promise<OpenInfo> {
  return invoke<OpenInfo>("index_path", { path });
}

/** Run a query/context/impact tool over the loaded graph (shared dispatch). */
export function tool<T>(name: string, args: Record<string, unknown>): Promise<T> {
  return invoke<T>("tool", { name, args });
}

export const query = (text: string) => tool<QueryResult>("query", { text });

export const context = (symbol: string) =>
  tool<ContextResult>("context", { symbol });

export interface ImpactArgs {
  depth?: number;
  min_confidence?: number;
  // Live through the shared `call_tool` impact dispatch: when omitted, impact
  // uses the engine default `include_contracts = true` (contract-aware); set
  // false for a code-only blast radius (drops cross-plane/cross-repo consumers).
  include_contracts?: boolean;
  // Likewise `include_infra` (default true): when false, the infra plane
  // (Assumes/Routes/Runs) is not traversed, so an IamRole no longer reaches the
  // Lambdas that assume it (and their reach). The arg flows through `tool`.
  include_infra?: boolean;
  // Pin ONE candidate when the symbol resolves to several nodes: pass the chosen
  // candidate's uid (from the ambiguous result's `candidates`), exactly as the CLI
  // `--uid` / MCP `uid` do, so impact runs against that specific node.
  uid?: string;
}

export const impact = (symbol: string, args: ImpactArgs = {}) =>
  tool<ImpactResult>("impact", { symbol, ...args });

/**
 * Explain why `affected` is in `symbol`'s blast radius — the evidence chain. Goes
 * through the SAME `call_tool` dispatch as `impact`, so the returned overall
 * confidence matches the affected node's impact confidence. Pass the same
 * include_contracts/include_infra toggles the impact run used.
 */
export const explain = (
  symbol: string,
  affected: string,
  args: ImpactArgs = {},
) => tool<ExplainResult>("explain", { symbol, affected, ...args });

export interface SubgraphArgs {
  /** BFS depth (server clamps to 3). */
  depth: number;
  /** Optional edge-kind filter (serde names, e.g. `"Calls"`). */
  kinds?: string[];
  /** Optional plane filter (`"code"`/`"contract"`/`"infra"`). */
  planes?: string[];
}

/**
 * Bounded both-directions neighbourhood of `uid` for the renderer. `kinds` and
 * `planes` are server-side filters; an unknown plane/kind is an error there.
 */
export function subgraph(uid: string, args: SubgraphArgs): Promise<SubgraphDto> {
  return invoke<SubgraphDto>("subgraph", {
    uid,
    depth: args.depth,
    kinds: args.kinds ?? null,
    planes: args.planes ?? null,
  });
}

// ── file pickers (dialog plugin) ─────────────────────────────────────────────

/** Native directory picker for a project/workspace folder; returns the path or null. */
export async function pickProjectFolder(): Promise<string | null> {
  const selected = await openDialog({
    multiple: false,
    directory: true,
    title: "Open a project folder — StrataGraph finds the index inside",
  });
  return typeof selected === "string" ? selected : null;
}

/** Native open dialog for a DuckDB graph file; returns the path or null. */
export async function pickGraphFile(): Promise<string | null> {
  const selected = await openDialog({
    multiple: false,
    directory: false,
    title: "Open a StrataGraph graph (graph.duckdb)",
    filters: [{ name: "StrataGraph graph", extensions: ["duckdb"] }],
  });
  return typeof selected === "string" ? selected : null;
}

/** Native open dialog for a multi-repo estate manifest; returns the path or null. */
export async function pickWorkspaceFile(): Promise<string | null> {
  const selected = await openDialog({
    multiple: false,
    directory: false,
    title: "Open a multi-repo estate (strata.workspace.toml)",
    filters: [{ name: "Estate manifest", extensions: ["toml"] }],
  });
  return typeof selected === "string" ? selected : null;
}
