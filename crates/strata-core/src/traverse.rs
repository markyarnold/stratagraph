use std::collections::HashMap;

use crate::graph::{Direction, Graph};
use crate::ids::Uid;
use crate::model::{Edge, EdgeKind, Node, NodeKind, Provenance};

// ── context ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct ContextResult {
    pub node: Node,
    pub callers: Vec<Node>,
    pub callees: Vec<Node>,
    pub imports_out: Vec<Node>,
    pub imports_in: Vec<Node>,
    pub members: Vec<Node>,
    pub container: Option<Node>,
    // ── contract plane (dogfood fix) ──
    /// Sources of incoming `Produces` edges — who implements this node. For a
    /// schema field/operation: the Lambda or resolver that produces it.
    pub producers: Vec<Node>,
    /// Sources of incoming `Consumes` edges — who calls this node. For a schema
    /// field/operation: the frontend modules that query it.
    pub consumers: Vec<Node>,
    /// Targets of outgoing `Produces` edges. For a Lambda/route handler: the
    /// operations/fields it implements.
    pub produces: Vec<Node>,
    /// Targets of outgoing `Consumes` edges. For a module: the operations/fields
    /// it calls.
    pub consumes: Vec<Node>,
    // ── infra plane (Slice 10, B1a) ──
    /// Targets of outgoing `Assumes` edges. For a `LambdaFn`: the `IamRole` it
    /// assumes.
    pub assumes: Vec<Node>,
    /// Sources of incoming `Assumes` edges. For an `IamRole`: the Lambdas that
    /// assume it — the headline fix (a role's dependents were invisible).
    pub assumed_by: Vec<Node>,
    /// Targets of outgoing `Routes` edges. For an `AppSyncResolver`: its
    /// `AppSyncDataSource`; for a datasource: the `LambdaFn` it backs.
    pub routes_to: Vec<Node>,
    /// Sources of incoming `Routes` edges. For a datasource: the resolver that
    /// routes to it; for a `LambdaFn`: the datasources routing to it.
    pub routed_from: Vec<Node>,
    /// Targets of outgoing `Runs` edges. For a `LambdaFn`: the code `Module` its
    /// handler resolves to.
    pub runs: Vec<Node>,
    /// Sources of incoming `Runs` edges. For a code `Module`: the Lambda(s) that
    /// run it (a handler module's `run_by` lists its Lambda).
    pub run_by: Vec<Node>,
    // ── data plane (Slice 25, D3, M2b) ──
    /// Sources of incoming `MapsTo` edges. For a `Table`: the ORM **model classes**
    /// that map to it (an explicit SQLAlchemy `__tablename__` / Django `db_table` /
    /// TypeORM `@Entity` mapping). The table's dedicated mapping view, distinct from
    /// the `Reads`/`Writes` query buckets. Empty for a non-table node, or a table no
    /// model maps to.
    pub mapped_by: Vec<Node>,
    /// Targets of outgoing `MapsTo` edges. For an ORM **model class**: the `Table` it
    /// maps to (the model's own view of its mapping). Empty for a node that is not a
    /// mapping model.
    pub maps_to: Vec<Node>,
}

fn sorted_unique_nodes(neighbors: Vec<(&Edge, &Node)>) -> Vec<Node> {
    let mut v: Vec<Node> = neighbors.into_iter().map(|(_, n)| n.clone()).collect();
    v.sort_by(|a, b| a.uid.cmp(&b.uid));
    v.dedup_by(|a, b| a.uid == b.uid);
    v
}

/// The members a node OWNS — its outgoing containment neighbours: code
/// containment (`Defines`: a class's methods / a module's symbols), infra
/// containment (`Contains`: an AppSyncApi's resolvers), AND data containment
/// (`HasColumn`: a table's columns). The single source of truth shared by
/// [`context`] (the `members` bucket) and [`impact`]'s `members_with_dependents`
/// surfacing, so both find members the same way. Sorted/deduped by uid.
fn member_nodes(graph: &Graph, uid: &Uid) -> Vec<Node> {
    sorted_unique_nodes(graph.neighbors(
        uid,
        Direction::Outgoing,
        &[EdgeKind::Defines, EdgeKind::Contains, EdgeKind::HasColumn],
    ))
}

/// The 360-degree view of one symbol: who calls it, what it calls,
/// its imports, its members, and its container.
pub fn context(graph: &Graph, uid: &Uid) -> Option<ContextResult> {
    let node = graph.get_node(uid)?.clone();
    let callers =
        sorted_unique_nodes(graph.neighbors(uid, Direction::Incoming, &[EdgeKind::Calls]));
    let callees =
        sorted_unique_nodes(graph.neighbors(uid, Direction::Outgoing, &[EdgeKind::Calls]));
    let imports_out =
        sorted_unique_nodes(graph.neighbors(uid, Direction::Outgoing, &[EdgeKind::Imports]));
    let imports_in =
        sorted_unique_nodes(graph.neighbors(uid, Direction::Incoming, &[EdgeKind::Imports]));
    // `members` covers code containment (`Defines`: a module's symbols), infra
    // containment (`Contains`: an AppSyncApi's resolvers/datasources, Slice 10 B3),
    // AND data containment (`HasColumn`: a Table's columns, Slice 16 D3), so
    // `context(api).members` lists what the API owns and `context(table).members`
    // lists its columns. Reusing `members` (rather than a dedicated bucket) keeps
    // the dispatch one path: every containment edge surfaces the same way. The
    // member-finding itself is the shared [`member_nodes`] seam `impact` also uses.
    let members = member_nodes(graph, uid);
    let container = graph
        .neighbors(uid, Direction::Outgoing, &[EdgeKind::MemberOf])
        .into_iter()
        .map(|(_, n)| n.clone())
        .next();
    // Contract plane (read-only, same dedup/order as the code buckets): the
    // relationships that DO apply to a schema field/operation. Incoming PRODUCES
    // = the handler implementing it; incoming CONSUMES = the modules querying it;
    // the outgoing directions cover the producer's/consumer's own view.
    let producers =
        sorted_unique_nodes(graph.neighbors(uid, Direction::Incoming, &[EdgeKind::Produces]));
    let consumers =
        sorted_unique_nodes(graph.neighbors(uid, Direction::Incoming, &[EdgeKind::Consumes]));
    let produces =
        sorted_unique_nodes(graph.neighbors(uid, Direction::Outgoing, &[EdgeKind::Produces]));
    let consumes =
        sorted_unique_nodes(graph.neighbors(uid, Direction::Outgoing, &[EdgeKind::Consumes]));
    // Infra plane (read-only, same dedup/order as the code/contract buckets): the
    // wiring that applies to a role/datasource/Lambda/handler-module. A role's
    // incoming `Assumes` (`assumed_by`) lists its Lambdas; a datasource's `Routes`
    // surface its resolver→DS→lambda chain from both ends; a handler module's
    // incoming `Runs` (`run_by`) lists its Lambda.
    let assumes =
        sorted_unique_nodes(graph.neighbors(uid, Direction::Outgoing, &[EdgeKind::Assumes]));
    let assumed_by =
        sorted_unique_nodes(graph.neighbors(uid, Direction::Incoming, &[EdgeKind::Assumes]));
    let routes_to =
        sorted_unique_nodes(graph.neighbors(uid, Direction::Outgoing, &[EdgeKind::Routes]));
    let routed_from =
        sorted_unique_nodes(graph.neighbors(uid, Direction::Incoming, &[EdgeKind::Routes]));
    let runs = sorted_unique_nodes(graph.neighbors(uid, Direction::Outgoing, &[EdgeKind::Runs]));
    let run_by = sorted_unique_nodes(graph.neighbors(uid, Direction::Incoming, &[EdgeKind::Runs]));
    // Data plane (read-only, same dedup/order as the other buckets): a `Table`'s
    // incoming `MapsTo` (`mapped_by`) lists the ORM model classes that map to it; a
    // model class's outgoing `MapsTo` (`maps_to`) is the table it maps to (Slice 25).
    let mapped_by =
        sorted_unique_nodes(graph.neighbors(uid, Direction::Incoming, &[EdgeKind::MapsTo]));
    let maps_to =
        sorted_unique_nodes(graph.neighbors(uid, Direction::Outgoing, &[EdgeKind::MapsTo]));
    Some(ContextResult {
        node,
        callers,
        callees,
        imports_out,
        imports_in,
        members,
        container,
        producers,
        consumers,
        produces,
        consumes,
        assumes,
        assumed_by,
        routes_to,
        routed_from,
        runs,
        run_by,
        mapped_by,
        maps_to,
    })
}

// ── impact ────────────────────────────────────────────────────────────────────

/// The default **"will break" confidence threshold** (design §15.6, resolved by
/// measurement 2026-06-12). An affected node whose best reaching-path confidence
/// is **≥** this value is labelled *"will break"*; below it, *"may be affected,
/// review"*.
///
/// The value is **measured, not chosen**: it is the lowest design-§4.1 band whose
/// empirically-measured precision crosses the will-break bar, per the calibration
/// in `docs/accuracy/ts-resolution.md`. The `INFERRED` band (0.40–0.80) measured
/// **1.00** precision against SCIP; the `AMBIGUOUS` band measured **0.53** — too
/// noisy to call a break, and §4.1 already **caps every Ambiguous edge's stored
/// confidence below 0.40**, so it falls under this threshold by construction. So
/// the boundary sits at the **INFERRED floor, 0.40**: Inferred-grade edges and
/// above are trustworthy enough to call a break; Ambiguous-grade edges (capped
/// sub-0.40, and excluded by provenance via the `!ambiguous` guard below) are
/// surfaced-but-marked.
///
/// This is intended to govern the **label only** — never the default
/// [`ImpactOptions::min_confidence`] (which stays `0.0`, recall-biased: surfaces
/// everything including AMBIGUOUS, flagged). The threshold decides what is
/// *called* a break, never what is *included*.
///
/// **Consumed in production** via [`will_break_label`]: every [`AffectedNode`]
/// carries a `will_break` field stamped from this constant — surfaced through the
/// MCP impact tool JSON, the CLI `impact`/`detect-changes` printers, and the
/// desktop impact table. `detect_changes` re-derives it after cross-symbol
/// aggregation. A caller wanting a hard *filter* (not just the label) can still
/// pass this value as `min_confidence`.
pub const DEFAULT_WILL_BREAK_CONFIDENCE: f32 = 0.40;

/// The §15.6 "will break" verdict for one reaching path, from its accumulated
/// `confidence` and whether the path is `ambiguous`. **The single source of
/// truth for the label**, shared by [`impact`] (stamped per affected node) and
/// `detect_changes` (re-derived after cross-symbol aggregation): `true` **iff**
/// the path is **at or above** [`DEFAULT_WILL_BREAK_CONFIDENCE`] **and**
/// non-ambiguous.
///
/// An ambiguous path is never "will break", regardless of confidence: it is
/// excluded by **provenance** (the `!ambiguous` guard), not by a numeric race.
/// AMBIGUOUS-grade edges measured 0.53 precision — too noisy to trust — and §4.1
/// independently caps their stored confidence below 0.40 (see
/// `docs/accuracy/ts-resolution.md`), so a path that traversed one is
/// surfaced-but-flagged — "may be affected, review" — never called a break.
pub fn will_break_label(confidence: f32, ambiguous: bool) -> bool {
    confidence >= DEFAULT_WILL_BREAK_CONFIDENCE && !ambiguous
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImpactOptions {
    /// Maximum reverse-traversal depth (hops from the target).
    pub max_depth: usize,
    /// Drop paths whose accumulated confidence falls below this value.
    /// Default 0.0 = recall-biased: keep everything, let the caller triage.
    pub min_confidence: f32,
    /// Also traverse reverse IMPORTS (module-level reach), not only CALLS.
    /// NOTE (slice 1): IMPORTS and CALLS edges are traversed together and are
    /// not distinguished in the result; this is intentionally coarse for now.
    pub include_imports: bool,
    /// Also follow the contract plane: from the target and each code node already
    /// in the blast radius, hop **outgoing `PRODUCES`** to the operation(s) it
    /// implements, then **incoming `CONSUMES`** to the consumer code that calls
    /// those operations (and that consumer's own reverse-CALLS callers, within
    /// the remaining depth). This surfaces cross-repo consumers of a producer
    /// handler (brief §5). Confidence is multiplied along the produce/consume
    /// hops and the path is flagged `ambiguous` if any contract edge is ambiguous.
    ///
    /// Default `true`. A graph with **no** contract edges is unaffected (the hop
    /// finds nothing), so contract-free impact results are byte-identical.
    pub include_contracts: bool,
    /// Also follow the infra plane (Slice 10, B1b): traverse INCOMING
    /// `Assumes`/`Routes`/`Runs` edges as dependency edges, at each edge's OWN
    /// confidence/provenance (never re-graded), exactly as reverse `CALLS`. This
    /// surfaces, for an `IamRole` target, the Lambdas that assume it (and then —
    /// via the existing contract hop off each Lambda — the operations they produce
    /// and the frontend that consumes them: the §6.3 reach); for a code `Module`,
    /// the Lambda that runs it; for a `LambdaFn`, the datasources/resolvers routing
    /// to it.
    ///
    /// Default `true`. A graph with **no** infra edges is unaffected (the
    /// traversal finds none), so infra-free impact results are byte-identical.
    pub include_infra: bool,
}

impl Default for ImpactOptions {
    fn default() -> ImpactOptions {
        ImpactOptions {
            max_depth: 5,
            min_confidence: 0.0,
            include_imports: false,
            include_contracts: true,
            include_infra: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AffectedNode {
    pub uid: Uid,
    pub name: String,
    pub depth: usize,
    /// Best (maximum) accumulated path confidence by which this node reaches the target.
    pub confidence: f32,
    /// True if any reaching path traversed an AMBIGUOUS edge.
    pub ambiguous: bool,
    /// The §15.6 verdict, derived from [`confidence`](Self::confidence) and
    /// [`ambiguous`](Self::ambiguous) via [`will_break_label`]: `true` ("will
    /// break") when the best reaching path is at/above
    /// [`DEFAULT_WILL_BREAK_CONFIDENCE`] and non-ambiguous; `false` ("may be
    /// affected, review") otherwise. A classification of what is surfaced — never
    /// a filter: impact stays recall-biased and still reports `false`-labelled
    /// nodes.
    pub will_break: bool,
}

/// A member of a member-bearing target (a class method, a table column, …) that
/// ITSELF has at least one dependent — the honest surfacing for the
/// misleading-zero case (see [`ImpactResult::members_with_dependents`]). Carries
/// just enough (`uid`/`name`/`kind`) for a caller to re-run `impact` on it.
#[derive(Debug, Clone, PartialEq)]
pub struct MemberDependent {
    /// The member node's uid (pin it with `impact --uid` / the `uid` tool arg).
    pub uid: Uid,
    /// The member's display name (e.g. the method/column name).
    pub name: String,
    /// The member's node kind (Method/Function/Column/Field/…).
    pub kind: NodeKind,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImpactResult {
    pub target: Uid,
    pub affected: Vec<AffectedNode>,
    /// Honest surfacing for a **member-bearing** target whose own reverse blast
    /// radius is empty: the members (methods/columns/…) that THEMSELVES have ≥1
    /// dependent. impact reverse-walks INCOMING edges and members hang off
    /// OUTGOING `Defines`/`Contains`/`HasColumn`, so `impact(Type)` can be empty
    /// even when a method has callers — bare-saying "nothing depends on this" then
    /// misleads (it looks dead) and borderline violates "NEVER claim nothing
    /// depends on this". This field lets the CLI/MCP point the user at the members
    /// that DO have dependents instead.
    ///
    /// **Populated ONLY** when [`affected`](Self::affected) is empty AND the target
    /// has members with dependents. Otherwise empty — so a genuinely dead container
    /// (members exist but none have dependents) still yields the honest
    /// "nothing depends on this", and the non-empty-`affected` path is unchanged.
    /// It is NEVER an aggregation into `affected`: a listed member is a real graph
    /// dependent of that member, framed as "members have deps", never "the type has
    /// these direct deps".
    pub members_with_dependents: Vec<MemberDependent>,
}

// ── explain (the evidence chain) ────────────────────────────────────────────────

/// One edge in an [`Explanation`]'s evidence chain — a single hop on the best
/// reaching path from the changed target to the affected node, carrying the
/// edge's kind, provenance, and confidence plus the running (accumulated)
/// confidence *after* this hop.
///
/// `from`/`to` are graph node uids, in the **dependency direction the impact walk
/// traverses**: impact reverse-walks, so `from` is the node nearer the target and
/// `to` is one hop further out (the dependent). Reading the hops top-to-bottom
/// retraces the same predecessors impact used to reach the dependent.
#[derive(Debug, Clone, PartialEq)]
pub struct PathHop {
    /// The node this hop departs from (nearer the target).
    pub from: Uid,
    /// The node this hop arrives at (one hop further out — the dependent).
    pub to: Uid,
    /// The edge kind traversed (`Calls`/`Produces`/`Consumes`/`Assumes`/…).
    pub edge_kind: EdgeKind,
    /// The edge's provenance — `Ambiguous` is what flags the hop (and the path).
    pub provenance: Provenance,
    /// The single edge's own confidence (never re-graded), as impact reads it.
    pub confidence: f32,
    /// The accumulated path confidence *after* applying this hop — the
    /// multiplicative product of every edge confidence from the target to `to`.
    /// The final hop's `running_confidence` is the number `impact` reports for the
    /// affected node (the consistency invariant).
    pub running_confidence: f32,
}

/// The evidence chain for "why is `affected` in `target`'s blast radius?": the
/// exact sequence of edges (each with kind/provenance/confidence) on the best
/// reaching path impact found, plus the overall accumulated `confidence` and
/// whether that path is `ambiguous`.
///
/// **Consistency invariant:** `explain(t, a)` reconstructs the path from the SAME
/// per-node best-confidence bookkeeping `impact` builds (same walk, same gating,
/// same multiplicative math), so its `confidence` (== the last hop's
/// `running_confidence`, or `1.0` for the empty `target == affected` chain) equals
/// the `confidence` `impact(t)` reports for `a`. If they ever diverge the
/// explainer is lying — a test asserts they match across a contract path and an
/// infra-traversal path.
#[derive(Debug, Clone, PartialEq)]
pub struct Explanation {
    /// The hops from `target` to `affected`, in traversal order. Empty when
    /// `target == affected` (a node trivially "explains itself", confidence 1.0).
    pub hops: Vec<PathHop>,
    /// The overall accumulated confidence of this path — equals the final hop's
    /// `running_confidence` (or `1.0` for the empty self-path), and equals the
    /// `confidence` impact reports for the affected node.
    pub confidence: f32,
    /// True if any hop traversed an AMBIGUOUS-provenance edge — mirrors the
    /// `ambiguous` flag impact stamps on the affected node.
    pub ambiguous: bool,
}

/// Reverse blast radius: everything that depends on `target` within `max_depth`
/// hops. Recall-biased — AMBIGUOUS paths are included and flagged, never dropped.
/// Confidence decays multiplicatively along each path; the reported confidence is
/// the maximum over all reaching paths. Depth-bounded, so cycles terminate.
///
/// With `include_contracts` (default), after the reverse-CALLS pass the contract
/// plane is followed: producer → operation → consumer (brief §5). When the TARGET
/// is itself a contract node (`ApiOperation`/`GraphqlField`), its incoming
/// `CONSUMES` consumers and incoming `PRODUCES` handlers are seeded directly
/// ("who breaks if I change this schema field / API operation?"). A graph with no
/// contract edges is unaffected.
///
/// With `include_infra` (default, Slice 10 B1b), the reverse walk additionally
/// follows incoming `Assumes`/`Routes`/`Runs` edges at each edge's own confidence
/// — so an `IamRole` target reaches the Lambdas that assume it, and (because the
/// infra hop seeds the contract hop) the operations those Lambdas produce and the
/// frontend that consumes them: the §6.3 reach. A graph with no infra edges is
/// unaffected.
pub fn impact(graph: &Graph, target: &Uid, opts: &ImpactOptions) -> ImpactResult {
    let best = reverse_walk(graph, target, opts);

    let mut affected: Vec<AffectedNode> = best
        .map
        .into_iter()
        .map(|(uid, entry)| {
            // neighbor always exists in the graph (see Graph::neighbors)
            let name = graph
                .get_node(&uid)
                .map(|n| n.name.clone())
                .unwrap_or_default();
            AffectedNode {
                uid,
                name,
                depth: entry.depth,
                confidence: entry.conf,
                ambiguous: entry.amb,
                will_break: will_break_label(entry.conf, entry.amb),
            }
        })
        .collect();
    // deterministic order: confidence desc, then uid asc
    affected.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.uid.cmp(&b.uid))
    });

    // Honest surfacing for the misleading-zero case: when the target has NO direct
    // dependents but IS a member-bearing container, list the members that
    // themselves have a dependent (so the caller is pointed at `impact <member>`
    // instead of a bare "nothing depends on this"). Computed only on the zero-
    // direct path, so the non-empty-`affected` result is byte-identical, and only
    // for members that REALLY have a dependent (a dead container stays dead).
    let members_with_dependents = if affected.is_empty() {
        members_with_dependents(graph, target, opts)
    } else {
        Vec::new()
    };

    ImpactResult {
        target: target.clone(),
        affected,
        members_with_dependents,
    }
}

/// The members of `target` that THEMSELVES have ≥1 dependent — the honest
/// surfacing behind [`ImpactResult::members_with_dependents`]. Reuses the shared
/// [`member_nodes`] seam (the same outgoing `Defines`/`Contains`/`HasColumn`
/// neighbours `context` lists as `members`), keeping a member's existence
/// dependent on exactly one definition. A member "has a dependent" iff its OWN
/// reverse blast radius under the SAME `opts` (so the depth/confidence/contract/
/// infra toggles the caller chose apply consistently) reaches at least one node
/// OTHER than `target` itself — a real graph dependent of that member, never an
/// invented one, and never the container we are already impacting.
///
/// **Why exclude `target`:** `HasColumn` is BOTH a member edge
/// (`Table —HasColumn→ Column`) AND an incoming reverse-walk dependency edge, so
/// `impact(column).affected` ALWAYS contains the column's own parent `Table` (the
/// column reaches its container via the incoming `HasColumn`). A bare emptiness
/// check would therefore pass for EVERY column and surface a genuinely dead
/// `Table`'s columns as "members with dependents" — the looks-alive-when-dead lie
/// this surfacing exists to prevent. Dropping the outer-target self-reach keeps
/// only members with a REAL external dependent (an FK-referencing column, code
/// that Reads/Writes the column, a method's callers). For code/infra containers
/// this is a no-op: `Defines`/`Contains` are not reverse-walked, so the container
/// is never in a member's `affected` to begin with — their behaviour is unchanged.
///
/// Members are returned in [`member_nodes`] order (sorted by uid). Empty when the
/// target has no members or no member has a dependent (dead = dead).
fn members_with_dependents(
    graph: &Graph,
    target: &Uid,
    opts: &ImpactOptions,
) -> Vec<MemberDependent> {
    member_nodes(graph, target)
        .into_iter()
        .filter(|m| {
            impact(graph, &m.uid, opts)
                .affected
                .iter()
                .any(|a| &a.uid != target)
        })
        .map(|m| MemberDependent {
            uid: m.uid,
            name: m.name,
            kind: m.kind,
        })
        .collect()
}

// ── shared bookkeeping (impact AND explain) ─────────────────────────────────────

/// The predecessor edge that yielded a node's BEST reaching confidence — the one
/// piece of bookkeeping `explain` adds on top of impact's per-node best. Recorded
/// by [`relax`] exactly when a node's best confidence improves, so following
/// `pred` backwards from any node retraces the same maximal-confidence path impact
/// scored. `None` only on the walk seed (the target).
#[derive(Debug, Clone, PartialEq)]
struct Pred {
    /// The node this hop came FROM (nearer the target).
    from: Uid,
    /// The edge kind traversed to reach this node.
    edge_kind: EdgeKind,
    /// The traversed edge's provenance.
    provenance: Provenance,
    /// The traversed edge's own confidence (never re-graded).
    edge_conf: f32,
}

/// One node's best reaching state in the reverse walk: the same `(min depth, max
/// confidence, sticky ambiguity)` impact has always tracked, plus the [`Pred`]
/// edge that produced the best confidence (so `explain` can reconstruct the path).
#[derive(Debug, Clone, PartialEq)]
struct BestEntry {
    depth: usize,
    conf: f32,
    amb: bool,
    pred: Option<Pred>,
}

/// The per-node best-reaching bookkeeping the reverse walk fills. **The single
/// source of truth shared by [`impact`] and [`explain`]:** impact reads each
/// entry's `(depth, conf, amb)`; explain additionally follows `pred` backwards to
/// reconstruct the evidence chain. One walk, one map — never two reachability
/// models — which is what makes `explain(t,a).confidence == impact(t).affected[a]`
/// true by construction.
#[derive(Debug, Default)]
struct Best {
    map: HashMap<Uid, BestEntry>,
}

/// Run the reverse blast-radius walk from `target` and return the shared [`Best`]
/// bookkeeping. **The one walk both `impact` and `explain` call** — identical
/// edge kinds (`Calls` + optional `Imports`/`Assumes`/`Routes`/`Runs`), identical
/// phase-1 reverse reach, contract hop, and target-is-contract-node seeding,
/// identical multiplicative confidence and `min_confidence`/depth gating. The
/// target's own entry is removed (a node is not its own dependent), exactly as
/// before. Factoring it here means explain can never drift from impact: they read
/// the same map.
fn reverse_walk(graph: &Graph, target: &Uid, opts: &ImpactOptions) -> Best {
    // `Calls` and the data-plane `ForeignKey`/`HasColumn`/`Reads`/`Writes` are
    // always-on dependency edges: each is traversed INCOMING from the target in the
    // one reverse walk, so the chain composes (a referenced `Column` ←ForeignKey— the
    // referencing `Column`; an owning `Table` ←HasColumn— a changed `Column`, so
    // `impact(column)` reaches its table; a `Table` ←Reads/Writes— the code that
    // reads/writes it, so `impact(table)` reaches the dependent code — the §6.2
    // payoff). Because the walk is incoming-only, `impact(table)` does NOT re-list its
    // columns (a `Table` has no incoming `HasColumn`); that view is
    // `context(table).members`. All are facts (Extracted 0.95), propagated at the
    // edge's own confidence — impact never re-grades. A graph with no data edges is
    // unaffected (the traversal finds none), so data-free impact is byte-identical.
    // This also seeds a `Column`/`Table` target's own dependents (Phase 1 reverse-
    // reaches FROM the target over these kinds — no separate seeding needed).
    // (`HasColumn` is traversed unlike the infra `Contains`: a table IS the sum of its
    // columns, see `EdgeKind::HasColumn`.)
    let mut kinds = vec![
        EdgeKind::Calls,
        EdgeKind::ForeignKey,
        EdgeKind::HasColumn,
        EdgeKind::Reads,
        EdgeKind::Writes,
        // `MapsTo` (an ORM model class to its `Table`, Slice 25 D3 M2b) is a data-plane
        // dependency edge like `Reads`/`Writes`: traversed INCOMING from the target so
        // `impact(table)` reaches the mapping model — and, because the model node also
        // has incoming `Calls`, transitively the code that instantiates/uses the model.
        // A graph with no ORM edges is unaffected (the traversal finds none).
        EdgeKind::MapsTo,
    ];
    if opts.include_imports {
        kinds.push(EdgeKind::Imports);
    }
    // Infra dependency edges are traversed INCOMING from the target, in the SAME
    // reverse walk as `Calls`, so the chain composes: e.g. role ←Assumes— Lambda
    // ←Routes— datasource, and module ←Runs— Lambda (whose produced operations the
    // contract hop then reaches). Each propagates at its own edge confidence —
    // impact never re-grades. This is also what seeds an `IamRole`/`AppSyncData
    // Source`/`AppSyncResolver`/`LambdaFn` target's own dependents (Phase 1 reverse
    // -reaches FROM the target over these kinds — no separate seeding needed).
    if opts.include_infra {
        kinds.push(EdgeKind::Assumes);
        kinds.push(EdgeKind::Routes);
        kinds.push(EdgeKind::Runs);
    }

    let mut best = Best::default();

    // ── Phase 1: the reverse-CALLS (+IMPORTS) blast radius from the target. ──
    reverse_reach(graph, target, &kinds, opts, 0, 1.0, false, &mut best);

    // ── Phase 2: the contract hop (producer → operation → consumer). ──
    //
    // Layered ON TOP of phase 1 so a contract-free graph is byte-identical: the
    // hop only fires where outgoing PRODUCES edges exist. We collect the seed set
    // first (the target plus every phase-1 node and its current best conf/depth/
    // ambiguity) so mutating `best` during the hop cannot feed back into itself.
    if opts.include_contracts {
        let mut seeds: Vec<(Uid, usize, f32, bool)> = Vec::with_capacity(best.map.len() + 1);
        seeds.push((target.clone(), 0, 1.0, false));
        for (uid, entry) in &best.map {
            seeds.push((uid.clone(), entry.depth, entry.conf, entry.amb));
        }
        contract_hop(graph, &kinds, opts, &seeds, &mut best);

        // ── Phase 2b: target-is-contract-node seeding. ──
        //
        // When the TARGET is itself a contract node (`ApiOperation`/`GraphqlField`)
        // it has no OUTGOING `PRODUCES`, so the phase-2 hop above never reaches its
        // consumers. Seed the contract traversal with the target AS the operation:
        // walk its incoming `CONSUMES` (consumers break when the contract changes)
        // and incoming `PRODUCES` (the implementing handler must change too).
        if matches!(
            graph.get_node(target).map(|n| n.kind),
            Some(NodeKind::ApiOperation | NodeKind::GraphqlField)
        ) {
            contract_target_seed(graph, &kinds, opts, target, &mut best);
        }
    }

    best.map.remove(target); // the target is not its own dependent
    best
}

/// Explain **why `affected` is in `target`'s blast radius**: the exact evidence
/// chain on the best reaching path impact found. Runs the SAME [`reverse_walk`]
/// `impact` does (so the bookkeeping is byte-for-byte the one impact scores), then
/// reconstructs `target → affected` by following each node's recorded [`Pred`]
/// backwards from `affected`.
///
/// Returns:
/// * `Some(empty hops, confidence 1.0)` when `target == affected` (a node
///   trivially explains itself);
/// * `None` when `affected` is **not reachable** from `target` under `opts` (not
///   in the blast radius — there is nothing to explain, an honest absence rather
///   than an empty success);
/// * `Some(Explanation)` otherwise, whose final `running_confidence` (== the
///   overall `confidence`) **equals `impact(target).affected[affected].confidence`**
///   — the consistency invariant a test asserts. The `ambiguous` flag likewise
///   mirrors impact's.
pub fn explain(
    graph: &Graph,
    target: &Uid,
    affected: &Uid,
    opts: &ImpactOptions,
) -> Option<Explanation> {
    // A node trivially explains itself: empty chain, full confidence, clean.
    if target == affected {
        return Some(Explanation {
            hops: Vec::new(),
            confidence: 1.0,
            ambiguous: false,
        });
    }

    let best = reverse_walk(graph, target, opts);
    // Not in the blast radius → nothing to explain (honest None, not empty Ok).
    // The overall `ambiguous` flag is the STICKY value impact stamps on the node
    // (an OR over ALL reaching paths), NOT just the displayed max-confidence chain:
    // a node whose best path is clean can still be ambiguous via a lower-confidence
    // route, and the verdict must match impact's exactly (never under-report — that
    // would render a confident "WILL BREAK" where impact says "may be affected").
    let affected_amb = best.map.get(affected)?.amb;

    // Walk predecessors backwards from `affected` to the target, collecting the
    // reversed hops. The recorded `Pred` chain is acyclic (each `relax` only sets
    // a predecessor on a *confidence improvement*, and confidence strictly decays
    // along the path), but we still cap the walk at the node count as a belt-and
    // -braces guard against any malformed bookkeeping.
    let mut rev: Vec<(Uid, Pred)> = Vec::new();
    let mut cursor = affected.clone();
    let guard = best.map.len() + 1;
    for _ in 0..guard {
        match best.map.get(&cursor).and_then(|e| e.pred.clone()) {
            // The seed (target) has no predecessor: the chain is complete.
            None => break,
            Some(pred) => {
                let from = pred.from.clone();
                rev.push((cursor.clone(), pred));
                if from == *target {
                    break;
                }
                cursor = from;
            }
        }
    }

    // Reconstruct forward (target → affected), recomputing the running confidence
    // multiplicatively from the seed so the final hop equals impact's number.
    let mut hops: Vec<PathHop> = Vec::with_capacity(rev.len());
    let mut running = 1.0_f32;
    for (to, pred) in rev.into_iter().rev() {
        running *= pred.edge_conf;
        hops.push(PathHop {
            from: pred.from,
            to,
            edge_kind: pred.edge_kind,
            provenance: pred.provenance,
            confidence: pred.edge_conf,
            running_confidence: running,
        });
    }

    Some(Explanation {
        hops,
        confidence: running,
        // Sticky (matches impact), not chain-derived — see `affected_amb` above.
        ambiguous: affected_amb,
    })
}

/// Reverse-reachability BFS over `kinds` edges, recording the best (min depth,
/// max confidence, sticky ambiguity) for each node into `best`.
///
/// Starts from `start` at `start_depth` with the accumulated `start_conf` /
/// `start_amb`, and explores incoming edges up to `opts.max_depth` total hops.
/// Used for both the initial blast radius (from the target) and the bounded
/// caller sub-tree under each cross-repo consumer (the contract hop). `start`
/// itself is NOT written (only reached nodes are), so the caller controls
/// whether the seed appears in the result.
#[allow(clippy::too_many_arguments)]
fn reverse_reach(
    graph: &Graph,
    start: &Uid,
    kinds: &[EdgeKind],
    opts: &ImpactOptions,
    start_depth: usize,
    start_conf: f32,
    start_amb: bool,
    best: &mut Best,
) {
    let mut frontier: Vec<(Uid, f32, bool)> = vec![(start.clone(), start_conf, start_amb)];

    for depth in start_depth..opts.max_depth {
        let mut next: Vec<(Uid, f32, bool)> = Vec::new();
        for (uid, conf, amb) in &frontier {
            for (edge, neighbor) in graph.neighbors(uid, Direction::Incoming, kinds) {
                let path_conf = *conf * edge.confidence.value();
                if path_conf < opts.min_confidence {
                    continue;
                }
                let path_amb = *amb || edge.provenance == Provenance::Ambiguous;
                let nd = depth + 1;
                let nuid = neighbor.uid.clone();
                let pred = Pred {
                    from: uid.clone(),
                    edge_kind: edge.kind,
                    provenance: edge.provenance,
                    edge_conf: edge.confidence.value(),
                };
                if relax(best, &nuid, nd, path_conf, path_amb, pred) {
                    next.push((nuid, path_conf, path_amb));
                }
            }
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }
}

/// The contract hop (brief §5): from each seed (a producer in the blast radius),
/// follow **outgoing `PRODUCES`** to the operation(s) it implements, then
/// **incoming `CONSUMES`** to the consumer code that calls those operations. Each
/// consumer is added to `best`, and a bounded reverse-CALLS sub-tree from it adds
/// its own callers (within the remaining depth).
///
/// Confidence multiplies along produce × consume; the path is `ambiguous` if the
/// seed path, the PRODUCES edge, or the CONSUMES edge is ambiguous. Deterministic:
/// seeds are processed in sorted (uid) order and graph neighbours are stable.
fn contract_hop(
    graph: &Graph,
    kinds: &[EdgeKind],
    opts: &ImpactOptions,
    seeds: &[(Uid, usize, f32, bool)],
    best: &mut Best,
) {
    // Sort seeds for deterministic processing order.
    let mut seeds: Vec<&(Uid, usize, f32, bool)> = seeds.iter().collect();
    seeds.sort_by(|a, b| a.0.cmp(&b.0));

    for (producer, p_depth, p_conf, p_amb) in seeds {
        // The producer must have room left for at least the operation+consumer
        // hops; if it is already at the depth limit, stop (bounded traversal).
        if *p_depth >= opts.max_depth {
            continue;
        }
        // producer —PRODUCES→ operation(s).
        for (prod_edge, op_node) in
            graph.neighbors(producer, Direction::Outgoing, &[EdgeKind::Produces])
        {
            let op_conf = *p_conf * prod_edge.confidence.value();
            if op_conf < opts.min_confidence {
                continue;
            }
            let op_amb = *p_amb || prod_edge.provenance == Provenance::Ambiguous;
            let op_depth = p_depth + 1;
            // The operation itself is affected by the producer change. Its
            // predecessor is the producer via this PRODUCES edge (the producer's
            // own `pred` already links back toward the target, so the chain
            // composes).
            let op_pred = Pred {
                from: producer.clone(),
                edge_kind: prod_edge.kind,
                provenance: prod_edge.provenance,
                edge_conf: prod_edge.confidence.value(),
            };
            relax(best, &op_node.uid, op_depth, op_conf, op_amb, op_pred);
            // operation ←CONSUMES— consumer(s), from the operation's depth/conf.
            consume_reach(
                graph,
                kinds,
                opts,
                &op_node.uid,
                op_depth,
                op_conf,
                op_amb,
                best,
            );
        }
    }
}

/// From an operation node (`op`, already at `op_depth`/`op_conf`/`op_amb`), follow
/// every **incoming `CONSUMES`** edge: each consumer is added to `best` at
/// `op_depth + 1` (confidence decayed by the CONSUMES edge, ambiguity sticky), and
/// a bounded reverse-CALLS sub-tree from each consumer adds its own callers within
/// the remaining depth.
///
/// Shared by the producer→op→consumer hop (`contract_hop`) and the
/// target-is-contract-node seeding (`contract_target_seed`) so both treat
/// consumers identically. Deterministic: graph neighbours are stable.
#[allow(clippy::too_many_arguments)]
fn consume_reach(
    graph: &Graph,
    kinds: &[EdgeKind],
    opts: &ImpactOptions,
    op: &Uid,
    op_depth: usize,
    op_conf: f32,
    op_amb: bool,
    best: &mut Best,
) {
    for (cons_edge, consumer) in graph.neighbors(op, Direction::Incoming, &[EdgeKind::Consumes]) {
        let c_conf = op_conf * cons_edge.confidence.value();
        if c_conf < opts.min_confidence {
            continue;
        }
        let c_amb = op_amb || cons_edge.provenance == Provenance::Ambiguous;
        let c_depth = op_depth + 1;
        if c_depth > opts.max_depth {
            continue;
        }
        let cuid = consumer.uid.clone();
        // The consumer's predecessor is the operation via this CONSUMES edge (the
        // edge is consumer→op in the graph, but the *dependency* hop op→consumer is
        // what the reverse walk traverses, so `from` is the operation).
        let c_pred = Pred {
            from: op.clone(),
            edge_kind: cons_edge.kind,
            provenance: cons_edge.provenance,
            edge_conf: cons_edge.confidence.value(),
        };
        relax(best, &cuid, c_depth, c_conf, c_amb, c_pred);
        // The consumer's own reverse-CALLS callers, within remaining depth.
        reverse_reach(graph, &cuid, kinds, opts, c_depth, c_conf, c_amb, best);
    }
}

/// Target-is-contract-node seeding (dogfood fix 2): the impact TARGET is itself an
/// operation (`ApiOperation`/`GraphqlField`). It has no outgoing `PRODUCES`, so the
/// producer→op→consumer hop never reaches its dependents. Seed the contract
/// traversal with the target AS the operation (at depth 0, conf 1.0, clean):
///
/// - incoming `CONSUMES` → each consumer is affected at depth 1 (conf = edge
///   confidence, ambiguity = edge provenance is Ambiguous), plus its bounded
///   reverse-CALLS callers — via the shared `consume_reach`.
/// - incoming `PRODUCES` → each producer (the implementing handler) is affected at
///   depth 1 (same conf/amb rules), plus its bounded reverse-CALLS callers, because
///   changing the contract forces the producer to change.
///
/// Depth-bounded: consumers/producers at depth 1 require `max_depth >= 1`.
/// Deterministic: producers are processed in sorted-uid order (consumers via
/// `consume_reach`, whose graph neighbours are stable). Confidence decay,
/// `min_confidence` filtering, and ambiguity stickiness follow the existing rules.
fn contract_target_seed(
    graph: &Graph,
    kinds: &[EdgeKind],
    opts: &ImpactOptions,
    target: &Uid,
    best: &mut Best,
) {
    // target ←CONSUMES— consumer(s): the target seeds at depth 0, conf 1.0, clean.
    consume_reach(graph, kinds, opts, target, 0, 1.0, false, best);

    // target ←PRODUCES— producer(s): the implementing handler(s) must change.
    let mut producers: Vec<(&Edge, &Node)> =
        graph.neighbors(target, Direction::Incoming, &[EdgeKind::Produces]);
    producers.sort_by(|a, b| a.1.uid.cmp(&b.1.uid));
    for (prod_edge, producer) in producers {
        let p_conf = prod_edge.confidence.value();
        if p_conf < opts.min_confidence {
            continue;
        }
        let p_amb = prod_edge.provenance == Provenance::Ambiguous;
        let p_depth = 1;
        if p_depth > opts.max_depth {
            continue;
        }
        let puid = producer.uid.clone();
        // The producer's predecessor is the target operation via this PRODUCES
        // edge (graph edge is producer→target, but the dependency hop the walk
        // takes is target→producer, so `from` is the target).
        let p_pred = Pred {
            from: target.clone(),
            edge_kind: prod_edge.kind,
            provenance: prod_edge.provenance,
            edge_conf: prod_edge.confidence.value(),
        };
        relax(best, &puid, p_depth, p_conf, p_amb, p_pred);
        // The producer's own reverse-CALLS callers, within remaining depth.
        reverse_reach(graph, &puid, kinds, opts, p_depth, p_conf, p_amb, best);
    }
}

/// Relax one node's best (min depth, max confidence, sticky ambiguity) entry in
/// `best`, recording `pred` as the predecessor edge **whenever the best
/// confidence improves** (so the recorded chain always retraces the
/// maximal-confidence path — the one `explain` must reconstruct, and the one
/// impact's reported number reflects). Returns whether the node *improved* (or is
/// new) — i.e. whether its neighbours should be re-explored.
fn relax(best: &mut Best, uid: &Uid, depth: usize, conf: f32, amb: bool, pred: Pred) -> bool {
    let (is_new, prev_depth, prev_conf, prev_amb) = match best.map.get(uid) {
        None => (true, usize::MAX, f32::MIN, false),
        Some(e) => (false, e.depth, e.conf, e.amb),
    };
    let conf_improved = is_new || conf > prev_conf + f32::EPSILON;
    let improved = conf_improved || depth < prev_depth || (amb && !prev_amb);
    let entry = best.map.entry(uid.clone()).or_insert(BestEntry {
        depth,
        conf,
        amb,
        pred: None,
    });
    entry.depth = entry.depth.min(depth);
    entry.conf = entry.conf.max(conf);
    entry.amb = entry.amb || amb;
    // The predecessor tracks the path that gave the BEST confidence: set it on a
    // fresh entry or any confidence improvement (the same condition that makes
    // `entry.conf` adopt this `conf`). Depth-only or ambiguity-only improvements
    // do not change which path is maximal-confidence, so they leave `pred` alone.
    if conf_improved {
        entry.pred = Some(pred);
    }
    improved
}

// ── query ─────────────────────────────────────────────────────────────────────

/// Lexical search over node name, fully-qualified name, and path
/// (case-insensitive substring). Results are sorted by uid for determinism.
pub fn query(graph: &Graph, text: &str) -> Vec<Node> {
    let needle = text.to_lowercase();
    let mut hits: Vec<Node> = graph
        .nodes()
        .filter(|n| {
            n.name.to_lowercase().contains(&needle)
                || n.fqn.to_lowercase().contains(&needle)
                || n.path.to_lowercase().contains(&needle)
        })
        .cloned()
        .collect();
    hits.sort_by(|a, b| a.uid.cmp(&b.uid));
    hits
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Confidence, NodeKind, Provenance, Span};

    fn node(uid: &str) -> Node {
        Node {
            uid: Uid(uid.into()),
            kind: NodeKind::Function,
            name: uid.into(),
            fqn: uid.into(),
            path: "x.ts".into(),
            span: Span::default(),
            provenance: Provenance::Extracted,
            confidence: Confidence::new(1.0),
        }
    }

    fn edge(src: &str, dst: &str, kind: EdgeKind) -> Edge {
        Edge {
            src: Uid(src.into()),
            dst: Uid(dst.into()),
            kind,
            provenance: Provenance::Inferred,
            confidence: Confidence::new(0.9),
        }
    }

    fn edge_c(src: &str, dst: &str, conf: f32, prov: Provenance) -> Edge {
        Edge {
            src: Uid(src.into()),
            dst: Uid(dst.into()),
            kind: EdgeKind::Calls,
            provenance: prov,
            confidence: Confidence::new(conf),
        }
    }

    // ── context tests ──

    #[test]
    fn context_buckets_neighbors_correctly() {
        let mut g = Graph::new();
        for id in ["target", "caller", "callee", "mod"] {
            g.add_node(node(id));
        }
        g.add_edge(edge("caller", "target", EdgeKind::Calls));
        g.add_edge(edge("target", "callee", EdgeKind::Calls));
        g.add_edge(edge("target", "mod", EdgeKind::Imports));

        let ctx = context(&g, &Uid("target".into())).unwrap();
        assert_eq!(ctx.node.uid, Uid("target".into()));
        assert_eq!(
            ctx.callers
                .iter()
                .map(|n| n.uid.as_str())
                .collect::<Vec<_>>(),
            vec!["caller"]
        );
        assert_eq!(
            ctx.callees
                .iter()
                .map(|n| n.uid.as_str())
                .collect::<Vec<_>>(),
            vec!["callee"]
        );
        assert_eq!(
            ctx.imports_out
                .iter()
                .map(|n| n.uid.as_str())
                .collect::<Vec<_>>(),
            vec!["mod"]
        );
    }

    #[test]
    fn context_returns_none_for_unknown_symbol() {
        let g = Graph::new();
        assert!(context(&g, &Uid("missing".into())).is_none());
    }

    // ── context infra-plane buckets (Slice 10, B1a) ──
    //
    // The infra plane wires `LambdaFn —Assumes→ IamRole`, `resolver —Routes→ ds
    // —Routes→ lambda`, and `LambdaFn —Runs→ Module`. Each is surfaced from BOTH
    // ends (same dedup/order as the contract buckets):
    //   assumes / assumed_by, routes_to / routed_from, runs / run_by.
    // A role's `assumed_by` is the headline fix — it lists its Lambdas.

    #[test]
    fn context_role_lists_assuming_lambdas_in_assumed_by() {
        // fn1, fn2 —Assumes→ role; role.assumed_by == [fn1, fn2], assumes == [].
        let mut g = Graph::new();
        for id in ["fn1", "fn2", "role"] {
            g.add_node(node(id));
        }
        g.add_edge(edge("fn1", "role", EdgeKind::Assumes));
        g.add_edge(edge("fn2", "role", EdgeKind::Assumes));

        let role = context(&g, &Uid("role".into())).unwrap();
        assert_eq!(
            role.assumed_by
                .iter()
                .map(|n| n.uid.as_str())
                .collect::<Vec<_>>(),
            vec!["fn1", "fn2"],
            "a role's assumed_by lists its assuming Lambdas"
        );
        assert!(role.assumes.is_empty(), "a role assumes nothing outward");

        // And from the Lambda's end: fn1.assumes == [role], assumed_by == [].
        let lambda = context(&g, &Uid("fn1".into())).unwrap();
        assert_eq!(
            lambda
                .assumes
                .iter()
                .map(|n| n.uid.as_str())
                .collect::<Vec<_>>(),
            vec!["role"]
        );
        assert!(lambda.assumed_by.is_empty());
    }

    #[test]
    fn context_datasource_lists_routes_both_directions() {
        // resolver —Routes→ ds —Routes→ lambda. The ds sees both ends:
        // ds.routed_from == [resolver], ds.routes_to == [lambda].
        let mut g = Graph::new();
        for id in ["resolver", "ds", "lambda"] {
            g.add_node(node(id));
        }
        g.add_edge(edge("resolver", "ds", EdgeKind::Routes));
        g.add_edge(edge("ds", "lambda", EdgeKind::Routes));

        let ds = context(&g, &Uid("ds".into())).unwrap();
        assert_eq!(
            ds.routed_from
                .iter()
                .map(|n| n.uid.as_str())
                .collect::<Vec<_>>(),
            vec!["resolver"],
            "the datasource's routed_from lists its resolver"
        );
        assert_eq!(
            ds.routes_to
                .iter()
                .map(|n| n.uid.as_str())
                .collect::<Vec<_>>(),
            vec!["lambda"],
            "the datasource's routes_to lists its backing lambda"
        );
    }

    #[test]
    fn context_module_lists_running_lambda_in_run_by() {
        // lambda —Runs→ module. module.run_by == [lambda]; lambda.runs == [module].
        let mut g = Graph::new();
        for id in ["lambda", "module"] {
            g.add_node(node(id));
        }
        g.add_edge(edge("lambda", "module", EdgeKind::Runs));

        let module = context(&g, &Uid("module".into())).unwrap();
        assert_eq!(
            module
                .run_by
                .iter()
                .map(|n| n.uid.as_str())
                .collect::<Vec<_>>(),
            vec!["lambda"],
            "the handler module's run_by lists its Lambda"
        );
        assert!(module.runs.is_empty());

        let lambda = context(&g, &Uid("lambda".into())).unwrap();
        assert_eq!(
            lambda
                .runs
                .iter()
                .map(|n| n.uid.as_str())
                .collect::<Vec<_>>(),
            vec!["module"]
        );
        assert!(lambda.run_by.is_empty());
    }

    // ── impact tests ──

    #[test]
    fn impact_finds_transitive_callers_with_depth() {
        // a calls b calls c  =>  impact(c) = {b@1, a@2}
        let mut g = Graph::new();
        for id in ["a", "b", "c"] {
            g.add_node(node(id));
        }
        g.add_edge(edge_c("a", "b", 0.9, Provenance::Inferred));
        g.add_edge(edge_c("b", "c", 0.9, Provenance::Inferred));

        let r = impact(&g, &Uid("c".into()), &ImpactOptions::default());
        let ids: Vec<&str> = r.affected.iter().map(|a| a.uid.as_str()).collect();
        assert_eq!(ids, vec!["b", "a"]); // b higher confidence (0.9) than a (0.81)
        let a = r.affected.iter().find(|x| x.uid.as_str() == "a").unwrap();
        assert_eq!(a.depth, 2);
        assert!((a.confidence - 0.81).abs() < 1e-5);
    }

    #[test]
    fn impact_respects_max_depth() {
        let mut g = Graph::new();
        for id in ["a", "b", "c"] {
            g.add_node(node(id));
        }
        g.add_edge(edge_c("a", "b", 1.0, Provenance::Inferred));
        g.add_edge(edge_c("b", "c", 1.0, Provenance::Inferred));

        let opts = ImpactOptions {
            max_depth: 1,
            ..ImpactOptions::default()
        };
        let r = impact(&g, &Uid("c".into()), &opts);
        let ids: Vec<&str> = r.affected.iter().map(|a| a.uid.as_str()).collect();
        assert_eq!(ids, vec!["b"]); // a is at depth 2, excluded
    }

    #[test]
    fn impact_includes_and_flags_ambiguous_paths() {
        let mut g = Graph::new();
        for id in ["a", "c"] {
            g.add_node(node(id));
        }
        g.add_edge(edge_c("a", "c", 0.3, Provenance::Ambiguous));

        let r = impact(&g, &Uid("c".into()), &ImpactOptions::default());
        assert_eq!(r.affected.len(), 1);
        assert_eq!(r.affected[0].uid.as_str(), "a");
        assert!(
            r.affected[0].ambiguous,
            "ambiguous path must be flagged, not dropped"
        );
    }

    #[test]
    fn impact_min_confidence_filters_low_paths() {
        let mut g = Graph::new();
        for id in ["a", "c"] {
            g.add_node(node(id));
        }
        g.add_edge(edge_c("a", "c", 0.3, Provenance::Ambiguous));

        let opts = ImpactOptions {
            min_confidence: 0.8,
            ..ImpactOptions::default()
        };
        let r = impact(&g, &Uid("c".into()), &opts);
        assert!(
            r.affected.is_empty(),
            "0.3 path is below the 0.8 will-break threshold"
        );
    }

    // §15.6 (resolved by measurement): the will-break label threshold is the
    // measured INFERRED-band floor (0.40), it sits strictly above the AMBIGUOUS
    // ceiling, and it does NOT change impact's recall-biased default (everything
    // still surfaces; the constant only classifies what is *called* a break).
    #[test]
    fn will_break_threshold_is_the_measured_inferred_floor() {
        // Compile-time guards on the constant (so changing it without re-deriving
        // from the measured curve fails to build). The measured §4.1 boundary:
        // INFERRED (0.40–0.80) precision 1.00 ≥ the bar, AMBIGUOUS (< 0.40)
        // precision 0.53 below it ⇒ cutoff at the INFERRED floor 0.40.
        const _: () = assert!(
            DEFAULT_WILL_BREAK_CONFIDENCE == 0.40,
            "the will-break cutoff is set from the measured curve (docs/accuracy/ts-resolution.md)"
        );
        // Strictly above the AMBIGUOUS ceiling (< 0.40) and within the INFERRED
        // band [0.40, 0.80]: an AMBIGUOUS edge is never labelled will-break; an
        // INFERRED edge can be.
        const _: () = assert!(
            DEFAULT_WILL_BREAK_CONFIDENCE > 0.39
                && DEFAULT_WILL_BREAK_CONFIDENCE >= 0.40
                && DEFAULT_WILL_BREAK_CONFIDENCE <= 0.80
        );

        // The default stays recall-biased: impact surfaces everything, including
        // a 0.3 AMBIGUOUS path, which the *label* (not the default filter) marks.
        let mut g = Graph::new();
        for id in ["a", "c"] {
            g.add_node(node(id));
        }
        g.add_edge(edge_c("a", "c", 0.3, Provenance::Ambiguous));
        let r = impact(&g, &Uid("c".into()), &ImpactOptions::default());
        assert_eq!(
            r.affected.len(),
            1,
            "default impact must keep the 0.3 path (recall-biased); the threshold is a label, not a filter"
        );
        let a = &r.affected[0];
        assert!(
            a.confidence < DEFAULT_WILL_BREAK_CONFIDENCE,
            "the surfaced 0.3 path is below the will-break bar ⇒ labelled 'may be affected'"
        );
        assert!(
            !a.will_break,
            "below the bar ⇒ the will_break field itself is false (labelled, not dropped)"
        );
    }

    // §15.6, continued: the label itself (`will_break`) follows the measured rule
    // — a clean reaching path AT or above the INFERRED floor (0.40) is "will
    // break"; a sub-floor path OR any ambiguous path is "may be affected, review".
    // The label classifies; it never filters (the recall-biased default stands).

    #[test]
    fn will_break_label_requires_threshold_and_non_ambiguity() {
        // The single source of truth, pinned directly: ≥ 0.40 AND not ambiguous.
        assert!(
            will_break_label(0.40, false),
            "the boundary 0.40, clean ⇒ will break"
        );
        assert!(will_break_label(1.0, false));
        assert!(
            !will_break_label(0.39, false),
            "just below the floor ⇒ may affect"
        );
        assert!(
            !will_break_label(0.95, true),
            "an ambiguous path is may-affect regardless of confidence"
        );
    }

    #[test]
    fn impact_labels_clean_at_or_above_floor_as_will_break() {
        // hi reaches T cleanly at 0.9; bnd reaches T cleanly at exactly the 0.40
        // floor. Both are INFERRED-grade ⇒ labelled will-break.
        let mut g = Graph::new();
        for id in ["t", "hi", "bnd"] {
            g.add_node(node(id));
        }
        g.add_edge(edge_c("hi", "t", 0.9, Provenance::Inferred));
        g.add_edge(edge_c("bnd", "t", 0.40, Provenance::Inferred));

        let r = impact(&g, &Uid("t".into()), &ImpactOptions::default());
        let get = |id: &str| r.affected.iter().find(|a| a.uid.as_str() == id).unwrap();
        assert!(get("hi").will_break, "conf 0.9 ≥ 0.40, clean ⇒ will break");
        assert!(
            get("bnd").will_break,
            "conf exactly at the 0.40 floor ⇒ will break"
        );
    }

    #[test]
    fn impact_labels_sub_floor_or_ambiguous_as_may_affect_but_still_surfaces() {
        // lo reaches T cleanly but below the floor (0.3); amb reaches T at 0.9 but
        // through an AMBIGUOUS edge. Neither is will-break — yet BOTH still surface
        // (the label classifies, it never drops: the recall-biased default).
        let mut g = Graph::new();
        for id in ["t", "lo", "amb"] {
            g.add_node(node(id));
        }
        g.add_edge(edge_c("lo", "t", 0.3, Provenance::Inferred));
        g.add_edge(edge_c("amb", "t", 0.9, Provenance::Ambiguous));

        let r = impact(&g, &Uid("t".into()), &ImpactOptions::default());
        assert_eq!(
            r.affected.len(),
            2,
            "recall-biased: both surface, labelled not dropped"
        );
        let get = |id: &str| r.affected.iter().find(|a| a.uid.as_str() == id).unwrap();
        assert!(!get("lo").will_break, "conf 0.3 < 0.40 ⇒ may affect");
        assert!(
            !get("amb").will_break,
            "an ambiguous reaching path ⇒ may affect even at conf 0.9"
        );
    }

    #[test]
    fn impact_terminates_on_cycles() {
        // a <-> b cycle; impact(a) should return b and not loop forever
        let mut g = Graph::new();
        for id in ["a", "b"] {
            g.add_node(node(id));
        }
        g.add_edge(edge_c("a", "b", 1.0, Provenance::Inferred));
        g.add_edge(edge_c("b", "a", 1.0, Provenance::Inferred));

        let r = impact(&g, &Uid("a".into()), &ImpactOptions::default());
        let ids: Vec<&str> = r.affected.iter().map(|a| a.uid.as_str()).collect();
        assert_eq!(ids, vec!["b"]);
    }

    #[test]
    fn impact_propagates_ambiguity_to_transitive_dependents() {
        // Edges are "src CALLS dst".
        // N->T clean, X->T clean, X->N AMBIGUOUS, Y->X clean.
        // Y depends on T via a clean route (Y->X->T) AND an ambiguous route
        // (Y->X->N->T). Recall-biased: X and Y MUST be flagged ambiguous.
        let mut g = Graph::new();
        for id in ["t", "n", "x", "y"] {
            g.add_node(node(id));
        }
        g.add_edge(edge_c("n", "t", 1.0, Provenance::Inferred));
        g.add_edge(edge_c("x", "t", 1.0, Provenance::Inferred));
        g.add_edge(edge_c("x", "n", 0.5, Provenance::Ambiguous));
        g.add_edge(edge_c("y", "x", 1.0, Provenance::Inferred));

        let r = impact(&g, &Uid("t".into()), &ImpactOptions::default());
        let get = |id: &str| r.affected.iter().find(|a| a.uid.as_str() == id).cloned();

        assert!(!get("n").unwrap().ambiguous, "N reaches T cleanly");
        assert!(
            get("x").unwrap().ambiguous,
            "X reaches T via an ambiguous hop (X->N->T)"
        );
        assert!(
            get("y").unwrap().ambiguous,
            "Y inherits ambiguity through X (Y->X->N->T)"
        );
    }

    #[test]
    fn impact_reports_min_depth_and_max_confidence_across_paths() {
        // D->T direct (short, low conf) and D->M->T (longer, higher conf).
        // depth = min (1), confidence = max (1.0).
        let mut g = Graph::new();
        for id in ["t", "m", "d"] {
            g.add_node(node(id));
        }
        g.add_edge(edge_c("d", "t", 0.5, Provenance::Inferred));
        g.add_edge(edge_c("m", "t", 1.0, Provenance::Inferred));
        g.add_edge(edge_c("d", "m", 1.0, Provenance::Inferred));

        let r = impact(&g, &Uid("t".into()), &ImpactOptions::default());
        let d = r.affected.iter().find(|a| a.uid.as_str() == "d").unwrap();
        assert_eq!(d.depth, 1, "minimum depth across paths");
        assert!(
            (d.confidence - 1.0).abs() < 1e-5,
            "maximum confidence across paths"
        );
    }

    // ── contract-aware impact tests (brief §5) ──

    /// A typed edge with explicit kind/provenance/confidence.
    fn cedge(src: &str, dst: &str, kind: EdgeKind, prov: Provenance, conf: f32) -> Edge {
        Edge {
            src: Uid(src.into()),
            dst: Uid(dst.into()),
            kind,
            provenance: prov,
            confidence: Confidence::new(conf),
        }
    }

    /// Build the canonical contract shape:
    ///   consumerCaller —CALLS→ consumer —CONSUMES→ op ←PRODUCES— producer
    /// so `impact(producer)` should surface `op`, `consumer`, and `consumerCaller`.
    fn contract_graph() -> Graph {
        let mut g = Graph::new();
        for id in ["producer", "op", "consumer", "consumerCaller"] {
            g.add_node(node(id));
        }
        g.add_edge(cedge(
            "producer",
            "op",
            EdgeKind::Produces,
            Provenance::Inferred,
            0.80,
        ));
        g.add_edge(cedge(
            "consumer",
            "op",
            EdgeKind::Consumes,
            Provenance::Inferred,
            0.70,
        ));
        g.add_edge(cedge(
            "consumerCaller",
            "consumer",
            EdgeKind::Calls,
            Provenance::Inferred,
            1.0,
        ));
        g
    }

    #[test]
    fn impact_follows_contract_plane_producer_to_consumer() {
        let g = contract_graph();
        let r = impact(&g, &Uid("producer".into()), &ImpactOptions::default());
        let ids: Vec<&str> = r.affected.iter().map(|a| a.uid.as_str()).collect();

        // The operation, the consumer, AND the consumer's caller are all affected.
        assert!(ids.contains(&"op"), "operation in blast radius: {ids:?}");
        assert!(
            ids.contains(&"consumer"),
            "consumer in blast radius: {ids:?}"
        );
        assert!(
            ids.contains(&"consumerCaller"),
            "consumer's caller in blast radius: {ids:?}"
        );

        // Confidence multiplies along produce × consume: 0.80 × 0.70 = 0.56.
        let consumer = r
            .affected
            .iter()
            .find(|a| a.uid.as_str() == "consumer")
            .unwrap();
        assert!(
            (consumer.confidence - 0.56).abs() < 1e-5,
            "consumer conf = produce(0.80) × consume(0.70) = 0.56, got {}",
            consumer.confidence
        );
        // The caller decays once more by the CALLS edge (×1.0 here) and is one
        // hop deeper than the consumer.
        let caller = r
            .affected
            .iter()
            .find(|a| a.uid.as_str() == "consumerCaller")
            .unwrap();
        assert_eq!(caller.depth, consumer.depth + 1, "caller is one hop deeper");
    }

    #[test]
    fn impact_contract_hop_disabled_yields_only_code_plane() {
        let g = contract_graph();
        let opts = ImpactOptions {
            include_contracts: false,
            ..ImpactOptions::default()
        };
        let r = impact(&g, &Uid("producer".into()), &opts);
        // With the hop off, the producer has no incoming CALLS, so nothing is
        // affected — the contract edges are invisible to impact.
        assert!(
            r.affected.is_empty(),
            "contract hop disabled → no contract-reached nodes, got {:?}",
            r.affected
                .iter()
                .map(|a| a.uid.as_str())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn impact_contract_hop_flags_ambiguous_consumes_edge() {
        // An AMBIGUOUS CONSUMES edge must flag the consumer ambiguous, not drop it.
        let mut g = Graph::new();
        for id in ["producer", "op", "consumer"] {
            g.add_node(node(id));
        }
        g.add_edge(cedge(
            "producer",
            "op",
            EdgeKind::Produces,
            Provenance::Inferred,
            0.80,
        ));
        g.add_edge(cedge(
            "consumer",
            "op",
            EdgeKind::Consumes,
            Provenance::Ambiguous,
            0.35,
        ));

        let r = impact(&g, &Uid("producer".into()), &ImpactOptions::default());
        let consumer = r
            .affected
            .iter()
            .find(|a| a.uid.as_str() == "consumer")
            .unwrap();
        assert!(
            consumer.ambiguous,
            "an ambiguous CONSUMES hop must flag the consumer ambiguous"
        );
    }

    #[test]
    fn impact_contract_hop_does_not_change_contract_free_graph() {
        // A pure CALLS graph yields the SAME result with the hop on or off
        // (the regression guard for "contract-aware impact must not change
        // existing contract-free impact results").
        let mut g = Graph::new();
        for id in ["a", "b", "c"] {
            g.add_node(node(id));
        }
        g.add_edge(edge_c("a", "b", 0.9, Provenance::Inferred));
        g.add_edge(edge_c("b", "c", 0.9, Provenance::Inferred));

        let on = impact(&g, &Uid("c".into()), &ImpactOptions::default());
        let off = impact(
            &g,
            &Uid("c".into()),
            &ImpactOptions {
                include_contracts: false,
                ..ImpactOptions::default()
            },
        );
        assert_eq!(
            on, off,
            "contract hop must be a no-op on a contract-free graph"
        );
    }

    // ── target-is-contract-node impact (dogfood fix 2) ──
    //
    // When the impact TARGET is itself a contract node (an `ApiOperation` or
    // `GraphqlField`), it has no OUTGOING `PRODUCES` edge, so the producer→op→
    // consumer hop never fired and `impact(op)` reported 0 affected. The
    // "I'm changing this schema field / API operation — who breaks?" question
    // is answered by seeding the contract traversal with the target AS the
    // operation: its incoming `CONSUMES` (consumers) and incoming `PRODUCES`
    // (the implementing handler) are the blast radius.

    /// A node with an explicit contract kind (the `node()` helper is Function).
    fn node_with_kind(uid: &str, kind: NodeKind) -> Node {
        Node { kind, ..node(uid) }
    }

    #[test]
    fn impact_on_contract_node_reaches_consumers_and_their_callers() {
        // op O (GraphqlField); C —CONSUMES(Extracted 0.95)→ O; A —CALLS(0.8)→ C;
        // P —PRODUCES(Inferred 0.8)→ O. impact(O) must reach the consumer C (depth
        // 1, conf 0.95), its caller A (depth 2, conf 0.95×0.8=0.76), and the
        // producer P (depth 1, conf 0.8). Nothing is ambiguous.
        let mut g = Graph::new();
        g.add_node(node_with_kind("O", NodeKind::GraphqlField));
        for id in ["C", "A", "P"] {
            g.add_node(node(id));
        }
        g.add_edge(cedge(
            "C",
            "O",
            EdgeKind::Consumes,
            Provenance::Extracted,
            0.95,
        ));
        g.add_edge(cedge("A", "C", EdgeKind::Calls, Provenance::Inferred, 0.8));
        g.add_edge(cedge(
            "P",
            "O",
            EdgeKind::Produces,
            Provenance::Inferred,
            0.8,
        ));

        let r = impact(&g, &Uid("O".into()), &ImpactOptions::default());
        let get = |id: &str| {
            r.affected
                .iter()
                .find(|a| a.uid.as_str() == id)
                .unwrap_or_else(|| panic!("{id} must be affected: {:?}", r.affected))
        };

        let c = get("C");
        assert_eq!(c.depth, 1, "consumer is at depth 1");
        assert!(
            (c.confidence - 0.95).abs() < 1e-5,
            "consumer conf = CONSUMES edge confidence 0.95, got {}",
            c.confidence
        );
        assert!(!c.ambiguous, "clean Extracted CONSUMES is not ambiguous");

        let a = get("A");
        assert_eq!(a.depth, 2, "consumer's caller is at depth 2");
        assert!(
            (a.confidence - 0.76).abs() < 1e-5,
            "caller conf = 0.95 × 0.8 = 0.76, got {}",
            a.confidence
        );
        assert!(!a.ambiguous, "clean caller is not ambiguous");

        let p = get("P");
        assert_eq!(p.depth, 1, "producer is at depth 1");
        assert!(
            (p.confidence - 0.8).abs() < 1e-5,
            "producer conf = PRODUCES edge confidence 0.8, got {}",
            p.confidence
        );
        assert!(!p.ambiguous, "clean Inferred PRODUCES is not ambiguous");

        // The target itself is never its own dependent.
        assert!(
            !r.affected.iter().any(|x| x.uid.as_str() == "O"),
            "the target operation is not in its own blast radius"
        );
    }

    #[test]
    fn impact_on_contract_node_respects_include_contracts_false() {
        // The same graph, but include_contracts=false → the op has no incoming
        // CALLS, so nothing is reached (the contract edges are invisible).
        let mut g = Graph::new();
        g.add_node(node_with_kind("O", NodeKind::GraphqlField));
        for id in ["C", "A", "P"] {
            g.add_node(node(id));
        }
        g.add_edge(cedge(
            "C",
            "O",
            EdgeKind::Consumes,
            Provenance::Extracted,
            0.95,
        ));
        g.add_edge(cedge("A", "C", EdgeKind::Calls, Provenance::Inferred, 0.8));
        g.add_edge(cedge(
            "P",
            "O",
            EdgeKind::Produces,
            Provenance::Inferred,
            0.8,
        ));

        let opts = ImpactOptions {
            include_contracts: false,
            ..ImpactOptions::default()
        };
        let r = impact(&g, &Uid("O".into()), &opts);
        assert!(
            r.affected.is_empty(),
            "with the contract hop off, a contract-node target reaches nothing, got {:?}",
            r.affected
                .iter()
                .map(|a| a.uid.as_str())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn impact_on_contract_node_ambiguous_consumes_flagged() {
        // An Ambiguous(0.35) CONSUMES edge → the consumer is flagged ambiguous;
        // and a min_confidence=0.8 filter drops it (0.35 < 0.8).
        let mut g = Graph::new();
        g.add_node(node_with_kind("O", NodeKind::ApiOperation));
        g.add_node(node("C"));
        g.add_edge(cedge(
            "C",
            "O",
            EdgeKind::Consumes,
            Provenance::Ambiguous,
            0.35,
        ));

        let r = impact(&g, &Uid("O".into()), &ImpactOptions::default());
        let c = r
            .affected
            .iter()
            .find(|a| a.uid.as_str() == "C")
            .unwrap_or_else(|| panic!("consumer must be affected: {:?}", r.affected));
        assert!(
            c.ambiguous,
            "an ambiguous CONSUMES hop must flag the consumer ambiguous"
        );

        // min_confidence above the edge confidence filters the consumer out.
        let opts = ImpactOptions {
            min_confidence: 0.8,
            ..ImpactOptions::default()
        };
        let filtered = impact(&g, &Uid("O".into()), &opts);
        assert!(
            filtered.affected.is_empty(),
            "0.35 ambiguous consumer is below the 0.8 threshold, got {:?}",
            filtered
                .affected
                .iter()
                .map(|a| a.uid.as_str())
                .collect::<Vec<_>>()
        );
    }

    // ── infra-aware impact (Slice 10, B1b) ──
    //
    // Dependency semantics (the src depends on the dst): `Assumes` (Lambda→Role),
    // `Routes` (Resolver→DS→Lambda), `Runs` (Lambda→Module) are all traversed
    // INCOMING from the target at the edge's OWN confidence (never re-graded),
    // exactly like Calls. Gated by `include_infra` (default true).

    /// The §6.3 shape, fully typed and graded like the real infra plane:
    ///   role ←Assumes(0.95)— fn1, fn2;  fn1 —Runs(0.95)→ module;
    ///   resolver —Routes(0.95)→ ds —Routes(0.95)→ fn1;
    ///   fn1 —Produces(0.95)→ field ←Consumes(0.95)— client.
    /// So `impact(role)` must reach fn1+fn2 (depth 1) and, through the existing
    /// contract hop off fn1, the field and its client.
    fn infra_role_chain() -> Graph {
        let mut g = Graph::new();
        g.add_node(node_with_kind("role", NodeKind::IamRole));
        g.add_node(node_with_kind("fn1", NodeKind::LambdaFn));
        g.add_node(node_with_kind("fn2", NodeKind::LambdaFn));
        g.add_node(node_with_kind("ds", NodeKind::AppSyncDataSource));
        g.add_node(node_with_kind("resolver", NodeKind::AppSyncResolver));
        g.add_node(node_with_kind("module", NodeKind::Module));
        g.add_node(node_with_kind("field", NodeKind::GraphqlField));
        g.add_node(node_with_kind("client", NodeKind::Module));
        g.add_edge(cedge(
            "fn1",
            "role",
            EdgeKind::Assumes,
            Provenance::Extracted,
            0.95,
        ));
        g.add_edge(cedge(
            "fn2",
            "role",
            EdgeKind::Assumes,
            Provenance::Extracted,
            0.95,
        ));
        g.add_edge(cedge(
            "resolver",
            "ds",
            EdgeKind::Routes,
            Provenance::Extracted,
            0.95,
        ));
        g.add_edge(cedge(
            "ds",
            "fn1",
            EdgeKind::Routes,
            Provenance::Extracted,
            0.95,
        ));
        g.add_edge(cedge(
            "fn1",
            "module",
            EdgeKind::Runs,
            Provenance::Extracted,
            0.95,
        ));
        g.add_edge(cedge(
            "fn1",
            "field",
            EdgeKind::Produces,
            Provenance::Extracted,
            0.95,
        ));
        g.add_edge(cedge(
            "client",
            "field",
            EdgeKind::Consumes,
            Provenance::Extracted,
            0.95,
        ));
        g
    }

    #[test]
    fn impact_on_role_reaches_assuming_lambdas_at_depth_1() {
        // THE §6.3 PROOF (engine level): impact(role) → fn1, fn2 at depth 1, 0.95.
        let g = infra_role_chain();
        let r = impact(&g, &Uid("role".into()), &ImpactOptions::default());
        let get = |id: &str| {
            r.affected
                .iter()
                .find(|a| a.uid.as_str() == id)
                .unwrap_or_else(|| panic!("{id} must be affected: {:?}", r.affected))
        };

        for fid in ["fn1", "fn2"] {
            let f = get(fid);
            assert_eq!(f.depth, 1, "{fid} (assuming Lambda) is at depth 1");
            assert!(
                (f.confidence - 0.95).abs() < 1e-5,
                "{fid} reach conf = Assumes edge confidence 0.95 (never re-graded), got {}",
                f.confidence
            );
            assert!(!f.ambiguous, "{fid} reaches via a clean Extracted Assumes");
        }
    }

    #[test]
    fn impact_on_role_reaches_field_and_consumer_through_lambda() {
        // THE §6.3 PROOF, full reach: the money link continues off the Lambda, so
        // impact(role) also surfaces the produced field AND its frontend consumer.
        let g = infra_role_chain();
        let r = impact(&g, &Uid("role".into()), &ImpactOptions::default());
        let ids: Vec<&str> = r.affected.iter().map(|a| a.uid.as_str()).collect();

        assert!(
            ids.contains(&"field"),
            "the produced field is reached: {ids:?}"
        );
        assert!(
            ids.contains(&"client"),
            "the field's frontend consumer is reached (role→fn1→field→client): {ids:?}"
        );
        // The handler module is NOT reached: `Runs` is Lambda→Module, so the
        // Lambda depends on the module, not the reverse. Changing the role does
        // not change the module — the dependency direction is honest.
        assert!(
            !ids.contains(&"module"),
            "the handler module must NOT be a dependent of the role (Runs is Lambda→Module): {ids:?}"
        );
    }

    #[test]
    fn impact_on_role_include_infra_false_reaches_nothing() {
        // include_infra=false → the role has no incoming Calls, so the whole chain
        // is invisible (proving the reach is the infra plane).
        let g = infra_role_chain();
        let r = impact(
            &g,
            &Uid("role".into()),
            &ImpactOptions {
                include_infra: false,
                ..ImpactOptions::default()
            },
        );
        assert!(
            r.affected.is_empty(),
            "with infra off, the role reaches nothing, got {:?}",
            r.affected
                .iter()
                .map(|a| a.uid.as_str())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn impact_on_module_reaches_lambda_field_and_consumer() {
        // THE MODULE PROOF: impact(handler module) → its Lambda (incoming Runs) →
        // the field it produces → the field's consumer. The chain-completer.
        let g = infra_role_chain();
        let r = impact(&g, &Uid("module".into()), &ImpactOptions::default());
        let ids: Vec<&str> = r.affected.iter().map(|a| a.uid.as_str()).collect();

        let lambda = r
            .affected
            .iter()
            .find(|a| a.uid.as_str() == "fn1")
            .unwrap_or_else(|| panic!("impact(module) must reach its Lambda: {ids:?}"));
        assert_eq!(lambda.depth, 1, "the running Lambda is at depth 1");
        assert!(
            (lambda.confidence - 0.95).abs() < 1e-5,
            "Lambda reach conf = Runs edge 0.95, got {}",
            lambda.confidence
        );
        assert!(
            ids.contains(&"field"),
            "the Lambda's produced field is reached: {ids:?}"
        );
        assert!(
            ids.contains(&"client"),
            "the field's consumer is reached: {ids:?}"
        );
    }

    #[test]
    fn impact_on_lambda_reaches_routing_datasource_and_resolver() {
        // impact(Lambda) → the datasource and resolver that route to it (incoming
        // Routes), at the edges' own 0.95.
        let g = infra_role_chain();
        let r = impact(&g, &Uid("fn1".into()), &ImpactOptions::default());
        let get = |id: &str| {
            r.affected
                .iter()
                .find(|a| a.uid.as_str() == id)
                .unwrap_or_else(|| panic!("{id} must be affected: {:?}", r.affected))
        };
        let ds = get("ds");
        assert_eq!(ds.depth, 1, "the backing datasource is at depth 1");
        assert!((ds.confidence - 0.95).abs() < 1e-5);
        let resolver = get("resolver");
        assert_eq!(
            resolver.depth, 2,
            "the resolver routing to the DS is at depth 2"
        );
    }

    #[test]
    fn impact_infra_edge_confidence_is_never_regraded() {
        // An Inferred 0.70 Assumes (the Sub/Join tier) must propagate at EXACTLY
        // 0.70 — impact uses the edge's own confidence, never re-grades it.
        let mut g = Graph::new();
        g.add_node(node_with_kind("role", NodeKind::IamRole));
        g.add_node(node_with_kind("fn", NodeKind::LambdaFn));
        g.add_edge(cedge(
            "fn",
            "role",
            EdgeKind::Assumes,
            Provenance::Inferred,
            0.70,
        ));

        let r = impact(&g, &Uid("role".into()), &ImpactOptions::default());
        let f = r.affected.iter().find(|a| a.uid.as_str() == "fn").unwrap();
        assert!(
            (f.confidence - 0.70).abs() < 1e-5,
            "Inferred 0.70 Assumes propagates at 0.70 (not re-graded), got {}",
            f.confidence
        );
    }

    #[test]
    fn impact_infra_hop_does_not_change_infra_free_graph() {
        // A pure CALLS graph yields the SAME result with include_infra on or off.
        let mut g = Graph::new();
        for id in ["a", "b", "c"] {
            g.add_node(node(id));
        }
        g.add_edge(edge_c("a", "b", 0.9, Provenance::Inferred));
        g.add_edge(edge_c("b", "c", 0.9, Provenance::Inferred));

        let on = impact(&g, &Uid("c".into()), &ImpactOptions::default());
        let off = impact(
            &g,
            &Uid("c".into()),
            &ImpactOptions {
                include_infra: false,
                ..ImpactOptions::default()
            },
        );
        assert_eq!(on, off, "infra hop must be a no-op on an infra-free graph");
    }

    // ── data-aware impact: code→table Reads/Writes (Slice 16, D3, M2) ──
    //
    // `Reads`/`Writes` are dependency edges traversed INCOMING from the target in
    // the SAME reverse walk as `Calls`, at the edge's own confidence. So
    // `impact(table)` reaches the code that reads/writes it (the §6.2 payoff). A
    // graph with no data edges is byte-identical (the traversal finds none).

    #[test]
    fn impact_on_table_reaches_reading_and_writing_code() {
        // reader —Reads(Extracted 0.95)→ table; writer —Writes(Extracted 0.95)→
        // table. impact(table) reaches BOTH at depth 1, conf 0.95, will-break.
        let mut g = Graph::new();
        g.add_node(node_with_kind("table", NodeKind::Table));
        g.add_node(node_with_kind("reader", NodeKind::Function));
        g.add_node(node_with_kind("writer", NodeKind::Function));
        g.add_edge(cedge(
            "reader",
            "table",
            EdgeKind::Reads,
            Provenance::Extracted,
            0.95,
        ));
        g.add_edge(cedge(
            "writer",
            "table",
            EdgeKind::Writes,
            Provenance::Extracted,
            0.95,
        ));

        let r = impact(&g, &Uid("table".into()), &ImpactOptions::default());
        let get = |id: &str| {
            r.affected
                .iter()
                .find(|a| a.uid.as_str() == id)
                .unwrap_or_else(|| panic!("{id} must be affected: {:?}", r.affected))
        };
        let reader = get("reader");
        assert_eq!(reader.depth, 1, "the reading code is at depth 1");
        assert!((reader.confidence - 0.95).abs() < 1e-5);
        assert!(
            reader.will_break && !reader.ambiguous,
            "Reads fact ⇒ will-break"
        );
        let writer = get("writer");
        assert_eq!(writer.depth, 1, "the writing code is at depth 1");
        assert!(writer.will_break, "Writes fact ⇒ will-break");
    }

    #[test]
    fn impact_reads_writes_compose_with_calls_transitively() {
        // caller —Calls→ reader —Reads→ table. impact(table) reaches the reader AND
        // its caller (the code chain composes through the data edge).
        let mut g = Graph::new();
        g.add_node(node_with_kind("table", NodeKind::Table));
        for id in ["reader", "caller"] {
            g.add_node(node(id));
        }
        g.add_edge(cedge(
            "reader",
            "table",
            EdgeKind::Reads,
            Provenance::Extracted,
            0.95,
        ));
        g.add_edge(edge_c("caller", "reader", 0.9, Provenance::Inferred));

        let r = impact(&g, &Uid("table".into()), &ImpactOptions::default());
        let ids: Vec<&str> = r.affected.iter().map(|a| a.uid.as_str()).collect();
        assert!(ids.contains(&"reader"), "the reader is reached: {ids:?}");
        assert!(
            ids.contains(&"caller"),
            "the reader's caller is reached transitively: {ids:?}"
        );
    }

    #[test]
    fn explain_reconstructs_a_reads_chain_matching_impact() {
        // explain(table, reader) is a one-hop Reads chain whose confidence equals
        // impact's number — the consistency invariant over a data edge.
        let mut g = Graph::new();
        g.add_node(node_with_kind("table", NodeKind::Table));
        g.add_node(node("reader"));
        g.add_edge(cedge(
            "reader",
            "table",
            EdgeKind::Reads,
            Provenance::Extracted,
            0.95,
        ));

        let opts = ImpactOptions::default();
        let e = explain(&g, &Uid("table".into()), &Uid("reader".into()), &opts)
            .expect("reader is reachable");
        assert_eq!(e.hops.len(), 1);
        assert_eq!(e.hops[0].edge_kind, EdgeKind::Reads);
        assert_eq!(e.hops[0].from.as_str(), "table");
        assert_eq!(e.hops[0].to.as_str(), "reader");
        let imp = impact_conf(&g, "table", "reader", &opts).expect("impact reaches reader");
        assert!(
            (e.confidence - imp).abs() < 1e-6,
            "explain.confidence {} must equal impact's {imp}",
            e.confidence
        );
    }

    // ── members_with_dependents: honest surfacing for a member-bearing target ──
    //
    // impact reverse-walks INCOMING edges; a type's members hang off OUTGOING
    // `Defines`/`HasColumn` edges, so `impact(Type)` is empty even when a METHOD
    // has callers (or a Table COLUMN is referenced). Rather than bare-say "nothing
    // depends on this" (misleading; borderline violates the core rule), impact
    // surfaces the members that themselves have dependents — populated ONLY in the
    // zero-direct + has-members case, never changing `affected` semantics.

    #[test]
    fn impact_on_type_surfaces_members_with_dependents_when_a_method_has_a_caller() {
        // Container T (Class) —Defines→ method m; caller —Calls→ m. impact(T) has
        // NO direct dependents (the type has no incoming edges), but its member m
        // DOES (caller). So `affected` is empty AND `members_with_dependents` lists m.
        let mut g = Graph::new();
        g.add_node(node_with_kind("T", NodeKind::Class));
        g.add_node(node_with_kind("m", NodeKind::Method));
        g.add_node(node("caller"));
        g.add_edge(edge("T", "m", EdgeKind::Defines));
        g.add_edge(edge_c("caller", "m", 0.9, Provenance::Inferred));

        let r = impact(&g, &Uid("T".into()), &ImpactOptions::default());
        assert!(
            r.affected.is_empty(),
            "the type itself has no direct dependents (members are outgoing): {:?}",
            r.affected
                .iter()
                .map(|a| a.uid.as_str())
                .collect::<Vec<_>>()
        );
        let members: Vec<&str> = r
            .members_with_dependents
            .iter()
            .map(|m| m.uid.as_str())
            .collect();
        assert_eq!(
            members,
            vec!["m"],
            "the member m (which HAS a caller) must be surfaced as a member-with-dependents"
        );
        // The surfaced member carries its name + kind so the caller can act on it.
        let m = &r.members_with_dependents[0];
        assert_eq!(m.name, "m");
        assert_eq!(m.kind, NodeKind::Method);
    }

    #[test]
    fn impact_on_dead_container_surfaces_no_members_honesty_preserved() {
        // A container T (Class) —Defines→ method m, but m has NO caller. Neither the
        // type NOR any member has a dependent ⇒ BOTH empty. dead = dead: the CLI's
        // existing "nothing depends on this" message stays correct (honesty kept).
        let mut g = Graph::new();
        g.add_node(node_with_kind("T", NodeKind::Class));
        g.add_node(node_with_kind("m", NodeKind::Method));
        g.add_edge(edge("T", "m", EdgeKind::Defines));

        let r = impact(&g, &Uid("T".into()), &ImpactOptions::default());
        assert!(r.affected.is_empty(), "a dead container has no direct deps");
        assert!(
            r.members_with_dependents.is_empty(),
            "no member has a dependent ⇒ members_with_dependents empty (dead = dead)"
        );
    }

    #[test]
    fn impact_on_table_surfaces_column_referenced_by_fk() {
        // A 2-column Table that genuinely DISTINGUISHES referenced from unreferenced:
        //   orders —HasColumn→ orders.id   (FK-referenced by items.order_id)
        //   orders —HasColumn→ orders.note (NO referrer, no Reads/Writes — bare)
        // impact(table) reaches nothing directly (its columns are OUTGOING), but the
        // FK-referenced column HAS a real external dependent ⇒ ONLY it is surfaced.
        // The bare column must NOT be surfaced: its only reachable node is the parent
        // table itself (via incoming HasColumn), which is the OUTER TARGET and so is
        // excluded — a member counts only when it reaches a node OTHER than the table.
        let mut g = Graph::new();
        g.add_node(node_with_kind("orders", NodeKind::Table));
        g.add_node(node_with_kind("orders.id", NodeKind::Column));
        g.add_node(node_with_kind("orders.note", NodeKind::Column));
        g.add_node(node_with_kind("items.order_id", NodeKind::Column));
        g.add_edge(edge("orders", "orders.id", EdgeKind::HasColumn));
        g.add_edge(edge("orders", "orders.note", EdgeKind::HasColumn));
        g.add_edge(cedge(
            "items.order_id",
            "orders.id",
            EdgeKind::ForeignKey,
            Provenance::Extracted,
            0.95,
        ));

        let r = impact(&g, &Uid("orders".into()), &ImpactOptions::default());
        // impact(table) reaches the FK column transitively via HasColumn? No — the
        // walk is INCOMING from the table; a table has no incoming HasColumn, and
        // the FK references the COLUMN not the table. So `affected` is empty and ONLY
        // the FK-referenced column is surfaced as a member-with-dependents.
        assert!(
            r.affected.is_empty(),
            "impact(table) has no direct dependents here: {:?}",
            r.affected
                .iter()
                .map(|a| a.uid.as_str())
                .collect::<Vec<_>>()
        );
        let members: Vec<&str> = r
            .members_with_dependents
            .iter()
            .map(|m| m.uid.as_str())
            .collect();
        assert_eq!(
            members,
            vec!["orders.id"],
            "ONLY the FK-referenced column is surfaced; the bare column (whose only \
             reach is the parent table itself) must NOT be"
        );
    }

    #[test]
    fn impact_on_dead_table_with_unreferenced_column_surfaces_no_members() {
        // A genuinely DEAD Table: one column with NO FK referrer and NO code
        // Reads/Writes anywhere. Honest outcome: impact(table).affected is empty AND
        // members_with_dependents is EMPTY (so the CLI keeps the bare "nothing
        // depends on this" and MCP omits the field).
        //
        // This is the data-plane honesty guard. `HasColumn` is BOTH a member edge
        // (Table —HasColumn→ Column) and an incoming reverse-walk dependency edge, so
        // impact(column).affected ALWAYS contains the column's own parent Table (the
        // column reaches its container). Without excluding that outer-target self-
        // reach, EVERY column would pass the emptiness filter and a dead Table would
        // surface ALL its columns as "members with dependents" — the exact
        // looks-alive-when-dead lie this feature exists to prevent.
        let mut g = Graph::new();
        g.add_node(node_with_kind("ghost", NodeKind::Table));
        g.add_node(node_with_kind("ghost.col", NodeKind::Column));
        g.add_edge(edge("ghost", "ghost.col", EdgeKind::HasColumn));

        let r = impact(&g, &Uid("ghost".into()), &ImpactOptions::default());
        assert!(
            r.affected.is_empty(),
            "a dead table has no direct dependents: {:?}",
            r.affected
                .iter()
                .map(|a| a.uid.as_str())
                .collect::<Vec<_>>()
        );
        assert!(
            r.members_with_dependents.is_empty(),
            "an unreferenced column reaches ONLY its parent table (the outer target), \
             which must be excluded ⇒ dead table surfaces no members (dead = dead), \
             got {:?}",
            r.members_with_dependents
                .iter()
                .map(|m| m.uid.as_str())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn impact_members_with_dependents_empty_when_affected_non_empty() {
        // The non-empty-affected path must be byte-identical: when the target has
        // its OWN direct dependents, members_with_dependents is NOT populated (it is
        // a fallback for the misleading-zero case only).
        let mut g = Graph::new();
        g.add_node(node_with_kind("T", NodeKind::Class));
        g.add_node(node_with_kind("m", NodeKind::Method));
        g.add_node(node("memberCaller"));
        g.add_node(node("typeCaller"));
        g.add_edge(edge("T", "m", EdgeKind::Defines));
        g.add_edge(edge_c("memberCaller", "m", 0.9, Provenance::Inferred));
        // typeCaller depends on the TYPE directly (e.g. constructs/extends it).
        g.add_edge(edge_c("typeCaller", "T", 0.9, Provenance::Inferred));

        let r = impact(&g, &Uid("T".into()), &ImpactOptions::default());
        assert!(
            r.affected.iter().any(|a| a.uid.as_str() == "typeCaller"),
            "the type has a direct dependent"
        );
        assert!(
            r.members_with_dependents.is_empty(),
            "members_with_dependents is the zero-direct fallback only; not populated when affected is non-empty"
        );
    }

    #[test]
    fn impact_non_container_with_zero_affected_has_empty_members() {
        // A plain Function with no dependents and no members: both empty (the hint
        // is only for member-bearing targets; a leaf function stays a bare zero).
        let mut g = Graph::new();
        g.add_node(node("leaf"));
        let r = impact(&g, &Uid("leaf".into()), &ImpactOptions::default());
        assert!(r.affected.is_empty());
        assert!(
            r.members_with_dependents.is_empty(),
            "a member-less target never surfaces members_with_dependents"
        );
    }

    // ── query tests ──

    #[test]
    fn query_matches_name_case_insensitively_and_is_sorted() {
        let mut g = Graph::new();
        for id in ["getUser", "getOrder", "deleteUser"] {
            g.add_node(node(id));
        }
        let result = query(&g, "user");
        let hits: Vec<&str> = result.iter().map(|n| n.uid.as_str()).collect();
        assert_eq!(hits, vec!["deleteUser", "getUser"]); // sorted by uid
    }

    #[test]
    fn query_returns_empty_when_no_match() {
        let mut g = Graph::new();
        g.add_node(node("foo"));
        assert!(query(&g, "zzz").is_empty());
    }

    // ── explain (the evidence chain) ──────────────────────────────────────────
    //
    // explain REUSES impact's exact reverse walk (shared `reverse_walk`/`Best`
    // bookkeeping); these tests pin the chain's shape AND the non-negotiable
    // consistency invariant — explain's final running confidence equals the
    // confidence impact reports for that node — across a contract path and an
    // infra-traversal path. If the explainer ever drifts from impact, the
    // invariant test fails.

    /// The confidence impact reports for `affected` (the number explain must match).
    fn impact_conf(g: &Graph, target: &str, affected: &str, opts: &ImpactOptions) -> Option<f32> {
        impact(g, &Uid(target.into()), opts)
            .affected
            .iter()
            .find(|a| a.uid.as_str() == affected)
            .map(|a| a.confidence)
    }

    #[test]
    fn explain_self_path_is_empty_with_full_confidence() {
        // A node trivially explains itself: no hops, conf 1.0, not ambiguous.
        let mut g = Graph::new();
        g.add_node(node("a"));
        let e = explain(
            &g,
            &Uid("a".into()),
            &Uid("a".into()),
            &ImpactOptions::default(),
        )
        .expect("self path always explains");
        assert!(e.hops.is_empty(), "self path has no hops");
        assert!((e.confidence - 1.0).abs() < 1e-6, "self path conf is 1.0");
        assert!(!e.ambiguous);
    }

    #[test]
    fn explain_unreachable_affected_is_none() {
        // `x` does not depend on `t` at all → not in the blast radius → None
        // (an honest absence, never an empty success).
        let mut g = Graph::new();
        for id in ["t", "x"] {
            g.add_node(node(id));
        }
        assert!(
            explain(
                &g,
                &Uid("t".into()),
                &Uid("x".into()),
                &ImpactOptions::default()
            )
            .is_none(),
            "an unreachable affected node has nothing to explain"
        );
    }

    #[test]
    fn explain_simple_calls_chain_reconstructs_hops_and_running() {
        // a calls b calls c ⇒ explain(c, a) is c→b→a, each Calls @0.9, running
        // 0.9 then 0.81 — and 0.81 equals impact(c).affected[a].confidence.
        let mut g = Graph::new();
        for id in ["a", "b", "c"] {
            g.add_node(node(id));
        }
        g.add_edge(edge_c("a", "b", 0.9, Provenance::Inferred));
        g.add_edge(edge_c("b", "c", 0.9, Provenance::Inferred));

        let opts = ImpactOptions::default();
        let e = explain(&g, &Uid("c".into()), &Uid("a".into()), &opts).expect("a is reachable");
        // Two hops, in dependency-traversal order c→b then b→a.
        assert_eq!(e.hops.len(), 2, "two hops: {:?}", e.hops);
        assert_eq!(e.hops[0].from.as_str(), "c");
        assert_eq!(e.hops[0].to.as_str(), "b");
        assert_eq!(e.hops[0].edge_kind, EdgeKind::Calls);
        assert!((e.hops[0].running_confidence - 0.9).abs() < 1e-5);
        assert_eq!(e.hops[1].from.as_str(), "b");
        assert_eq!(e.hops[1].to.as_str(), "a");
        assert!((e.hops[1].running_confidence - 0.81).abs() < 1e-5);
        assert!(!e.ambiguous);

        // THE INVARIANT: the final running confidence == impact's reported number.
        let imp = impact_conf(&g, "c", "a", &opts).expect("impact reaches a");
        assert!(
            (e.confidence - imp).abs() < 1e-6,
            "explain.confidence {} must equal impact's {imp}",
            e.confidence
        );
    }

    #[test]
    fn explain_marks_ambiguous_hop() {
        // a —AMBIGUOUS(0.3)→ c. explain(c, a) has one ambiguous hop and the path
        // is flagged ambiguous, mirroring impact.
        let mut g = Graph::new();
        for id in ["a", "c"] {
            g.add_node(node(id));
        }
        g.add_edge(edge_c("a", "c", 0.3, Provenance::Ambiguous));

        let opts = ImpactOptions::default();
        let e = explain(&g, &Uid("c".into()), &Uid("a".into()), &opts).expect("reachable");
        assert_eq!(e.hops.len(), 1);
        assert_eq!(e.hops[0].provenance, Provenance::Ambiguous);
        assert!(e.ambiguous, "an ambiguous hop flags the whole path");
        // The flag matches impact's `ambiguous` for the same node.
        let amb = impact(&g, &Uid("c".into()), &opts)
            .affected
            .iter()
            .find(|a| a.uid.as_str() == "a")
            .unwrap()
            .ambiguous;
        assert_eq!(e.ambiguous, amb, "explain ambiguity mirrors impact's");
    }

    // ── THE CONSISTENCY INVARIANT across a contract path AND an infra path ──────

    #[test]
    fn explain_matches_impact_confidence_on_a_contract_path() {
        // producer —PRODUCES(0.80)→ op ←CONSUMES(0.70)— consumer ←CALLS(1.0)—
        // consumerCaller. For EVERY affected node, explain(producer, node).confidence
        // must equal impact(producer).affected[node].confidence — the contract path
        // proof of the invariant.
        let g = contract_graph();
        let opts = ImpactOptions::default();
        let r = impact(&g, &Uid("producer".into()), &opts);
        assert!(!r.affected.is_empty(), "the contract graph has dependents");

        for a in &r.affected {
            let e = explain(&g, &Uid("producer".into()), &a.uid, &opts)
                .unwrap_or_else(|| panic!("{} is affected, so it must explain", a.uid.as_str()));
            assert!(
                (e.confidence - a.confidence).abs() < 1e-6,
                "consistency invariant (contract): explain({}).confidence {} != impact {}",
                a.uid.as_str(),
                e.confidence,
                a.confidence
            );
            assert_eq!(
                e.ambiguous,
                a.ambiguous,
                "explain ambiguity must mirror impact's for {}",
                a.uid.as_str()
            );
            // A non-self affected node has a non-empty chain whose first hop
            // departs the target.
            assert!(!e.hops.is_empty(), "{} has a real chain", a.uid.as_str());
            assert_eq!(e.hops.first().unwrap().from.as_str(), "producer");
            assert_eq!(e.hops.last().unwrap().to.as_str(), a.uid.as_str());
        }

        // And the specific consumer chain reads producer→op→consumer with the
        // right kinds and the 0.80 × 0.70 = 0.56 running confidence.
        let e = explain(&g, &Uid("producer".into()), &Uid("consumer".into()), &opts).unwrap();
        assert_eq!(e.hops.len(), 2, "producer→op→consumer: {:?}", e.hops);
        assert_eq!(e.hops[0].edge_kind, EdgeKind::Produces);
        assert_eq!(e.hops[0].to.as_str(), "op");
        assert!((e.hops[0].running_confidence - 0.80).abs() < 1e-5);
        assert_eq!(e.hops[1].edge_kind, EdgeKind::Consumes);
        assert_eq!(e.hops[1].to.as_str(), "consumer");
        assert!((e.hops[1].running_confidence - 0.56).abs() < 1e-5);
    }

    #[test]
    fn explain_matches_impact_confidence_on_an_infra_path() {
        // THE INFRA-TRAVERSAL PROOF: on the §6.3 role→fn1→field→client chain, for
        // every affected node explain(role, node).confidence == impact's number.
        let g = infra_role_chain();
        let opts = ImpactOptions::default();
        let r = impact(&g, &Uid("role".into()), &opts);
        assert!(!r.affected.is_empty(), "the role has dependents");

        for a in &r.affected {
            let e = explain(&g, &Uid("role".into()), &a.uid, &opts)
                .unwrap_or_else(|| panic!("{} is affected, so it must explain", a.uid.as_str()));
            assert!(
                (e.confidence - a.confidence).abs() < 1e-6,
                "consistency invariant (infra): explain({}).confidence {} != impact {}",
                a.uid.as_str(),
                e.confidence,
                a.confidence
            );
            assert_eq!(
                e.ambiguous,
                a.ambiguous,
                "explain ambiguity must mirror impact's for {}",
                a.uid.as_str()
            );
        }

        // The full money chain: role ←Assumes— fn1 —Produces→ field ←Consumes—
        // client. explain(role, client) retraces exactly those edges/kinds.
        let e = explain(&g, &Uid("role".into()), &Uid("client".into()), &opts)
            .expect("client is reachable from the role");
        let kinds: Vec<EdgeKind> = e.hops.iter().map(|h| h.edge_kind).collect();
        assert_eq!(
            kinds,
            vec![EdgeKind::Assumes, EdgeKind::Produces, EdgeKind::Consumes],
            "the role→client chain is Assumes→Produces→Consumes: {:?}",
            e.hops
        );
        assert_eq!(e.hops[0].from.as_str(), "role");
        assert_eq!(e.hops[0].to.as_str(), "fn1");
        assert_eq!(e.hops[1].to.as_str(), "field");
        assert_eq!(e.hops[2].to.as_str(), "client");
        // running = 0.95 × 0.95 × 0.95 = 0.857375.
        assert!(
            (e.confidence - 0.857_375).abs() < 1e-5,
            "running conf along the infra chain, got {}",
            e.confidence
        );
    }

    #[test]
    fn explain_respects_include_infra_false_consistently_with_impact() {
        // With infra off, the role reaches nothing — so explain(role, fn1) is None,
        // exactly as impact reports no `fn1`. The toggle moves both together.
        let g = infra_role_chain();
        let opts = ImpactOptions {
            include_infra: false,
            ..ImpactOptions::default()
        };
        assert!(
            impact_conf(&g, "role", "fn1", &opts).is_none(),
            "with infra off impact does not reach fn1"
        );
        assert!(
            explain(&g, &Uid("role".into()), &Uid("fn1".into()), &opts).is_none(),
            "with infra off explain has nothing to explain either"
        );
    }

    #[test]
    fn explain_respects_include_contracts_false_consistently_with_impact() {
        // With contracts off, the producer reaches no consumer — explain returns
        // None for the consumer, mirroring impact.
        let g = contract_graph();
        let opts = ImpactOptions {
            include_contracts: false,
            ..ImpactOptions::default()
        };
        assert!(
            impact_conf(&g, "producer", "consumer", &opts).is_none(),
            "with contracts off impact does not reach the consumer"
        );
        assert!(
            explain(&g, &Uid("producer".into()), &Uid("consumer".into()), &opts).is_none(),
            "with contracts off explain has nothing to explain for the consumer"
        );
    }

    #[test]
    fn explain_picks_the_max_confidence_path_matching_impact() {
        // D->T direct (0.5) and D->M->T (1.0 × 1.0). impact reports D at conf 1.0
        // (the max path); explain must reconstruct the SAME 2-hop max path, not the
        // shorter low-confidence one — its running confidence equals impact's 1.0.
        let mut g = Graph::new();
        for id in ["t", "m", "d"] {
            g.add_node(node(id));
        }
        g.add_edge(edge_c("d", "t", 0.5, Provenance::Inferred));
        g.add_edge(edge_c("m", "t", 1.0, Provenance::Inferred));
        g.add_edge(edge_c("d", "m", 1.0, Provenance::Inferred));

        let opts = ImpactOptions::default();
        let e = explain(&g, &Uid("t".into()), &Uid("d".into()), &opts).expect("d reachable");
        let imp = impact_conf(&g, "t", "d", &opts).unwrap();
        assert!(
            (e.confidence - imp).abs() < 1e-6 && (e.confidence - 1.0).abs() < 1e-6,
            "explain must follow impact's MAX-confidence path (1.0 via t→m→d), got {}",
            e.confidence
        );
        // The reconstructed max path is the 2-hop one (t→m→d), not the direct hop.
        assert_eq!(
            e.hops.len(),
            2,
            "the max-confidence path is t→m→d: {:?}",
            e.hops
        );
        assert_eq!(e.hops[0].to.as_str(), "m");
        assert_eq!(e.hops[1].to.as_str(), "d");
    }

    #[test]
    fn explain_ambiguous_flag_is_sticky_and_matches_impact_off_the_max_path() {
        // Regression (review, slice 15): the SAME dual-route shape as
        // `impact_propagates_ambiguity_to_transitive_dependents`. X reaches T via a
        // CLEAN max-confidence hop (X→T, 1.0) AND a lower-confidence AMBIGUOUS route
        // (X→N→T). impact stamps X/Y ambiguous (sticky OR over all routes). explain's
        // overall `ambiguous` must MATCH impact's — even though the displayed
        // max-confidence chain is clean — or it would render a confident "WILL BREAK"
        // where impact says "may be affected, review" (the under-report the review
        // caught). The per-hop provenance still reflects the displayed chain.
        let mut g = Graph::new();
        for id in ["t", "n", "x", "y"] {
            g.add_node(node(id));
        }
        g.add_edge(edge_c("n", "t", 1.0, Provenance::Inferred));
        g.add_edge(edge_c("x", "t", 1.0, Provenance::Inferred));
        g.add_edge(edge_c("x", "n", 0.5, Provenance::Ambiguous));
        g.add_edge(edge_c("y", "x", 1.0, Provenance::Inferred));

        let opts = ImpactOptions::default();
        let r = impact(&g, &Uid("t".into()), &opts);
        let imp_amb = |id: &str| {
            r.affected
                .iter()
                .find(|a| a.uid.as_str() == id)
                .unwrap_or_else(|| panic!("{id} affected"))
                .ambiguous
        };

        for id in ["x", "y"] {
            let e = explain(&g, &Uid("t".into()), &Uid(id.into()), &opts)
                .unwrap_or_else(|| panic!("{id} reachable"));
            assert!(imp_amb(id), "impact stamps {id} ambiguous (sticky)");
            assert_eq!(
                e.ambiguous,
                imp_amb(id),
                "explain.ambiguous must match impact's sticky flag for {id} \
                 (its max-confidence chain is clean, but a lower-conf route is ambiguous)"
            );
            // The displayed max-confidence chain itself is clean (X→T / Y→X→T).
            assert!(
                e.hops.iter().all(|h| h.provenance != Provenance::Ambiguous),
                "the shown max-conf chain for {id} is clean: {:?}",
                e.hops
            );
        }

        // N reaches T only cleanly → not ambiguous in either view.
        let en = explain(&g, &Uid("t".into()), &Uid("n".into()), &opts).expect("n reachable");
        assert!(!imp_amb("n") && !en.ambiguous, "N is clean in both views");
    }
}
