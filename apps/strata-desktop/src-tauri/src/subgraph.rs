//! `subgraph`: a bounded both-directions neighbourhood of a node, shaped for the
//! M2 WebGL renderer (and unit-tested now so the contract is pinned).
//!
//! This is the one piece of graph traversal the desktop app adds on top of the
//! shared `strata_mcp::call_tool` dispatch. It is a pure function of
//! `(graph, uid, depth, kinds, planes)` with no IO, so the tests below exercise
//! it directly without a window.
//!
//! Guarantees (all tested):
//! * BFS expands edges in **both** directions (a renderer needs callers *and*
//!   callees / producers *and* consumers around the focus node).
//! * `depth` is clamped to [`MAX_DEPTH`] server-side — a client cannot ask for an
//!   unbounded walk.
//! * The node set is capped at [`MAX_NODES`]; when the cap is hit the walk stops
//!   and `truncated` is set so the UI can say "showing N of more".
//! * An optional edge-kind filter (serde names, e.g. `"Calls"`, `"Produces"`)
//!   restricts which edges are followed *and* which appear in the result.
//! * An optional **plane** filter (`"code"`/`"contract"`/`"infra"`) restricts
//!   which nodes are admitted and traversed — the **one** server-side source of
//!   the plane mapping, so the UI never re-derives a plane from a node kind.
//! * An unknown `uid` is an error, not an empty graph (the caller asked about a
//!   node that does not exist).

use std::collections::{HashSet, VecDeque};

use serde::Serialize;
use serde_json::Value;
use strata_core::{Direction, EdgeKind, Graph, Node, NodeKind, Uid};

/// The four visual planes a node can belong to. This is the **single source of
/// truth** for the plane of a [`NodeKind`]; the renderer consumes the derived
/// `plane` string and must never re-derive it from `kind`.
///
/// * `code` — the program-structure plane: repos, packages, files, modules,
///   classes, interfaces, functions, methods.
/// * `contract` — the interface plane: API operations and GraphQL fields.
/// * `infra` — the infrastructure plane: Lambda functions, IAM roles, AppSync
///   API/resolver/datasource resources, and any other cloud resource.
/// * `data` — the database plane: tables and columns (Slice 16, D3).
///
/// Returned as a `&'static str` (no allocation) so callers can both compare and
/// clone cheaply.
pub fn plane_of(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Repo
        | NodeKind::Package
        | NodeKind::File
        | NodeKind::Module
        | NodeKind::Class
        | NodeKind::Interface
        | NodeKind::Function
        | NodeKind::Method => "code",
        NodeKind::ApiOperation | NodeKind::GraphqlField => "contract",
        NodeKind::LambdaFn
        | NodeKind::IamRole
        | NodeKind::AppSyncApi
        | NodeKind::AppSyncResolver
        | NodeKind::AppSyncDataSource
        | NodeKind::CloudResource
        | NodeKind::CloudAction => "infra",
        NodeKind::Table | NodeKind::Column => "data",
    }
}

/// The set of valid plane names a [`compute_subgraph`] filter may contain. Kept
/// in sync with [`plane_of`]'s range; an unknown plane in a filter is an error.
const PLANES: [&str; 4] = ["code", "contract", "infra", "data"];

/// The maximum BFS depth the server will honour, regardless of the requested
/// `depth`. Keeps a renderer feed bounded (spec: depth ≤ 3).
pub const MAX_DEPTH: u32 = 3;

/// The maximum number of nodes returned. When the frontier would push past this,
/// the walk stops and [`SubgraphDto::truncated`] is set. Protects the renderer
/// (and the IPC payload) from a pathological hub on a very large graph.
pub const MAX_NODES: usize = 500;

/// One node in the subgraph payload (the renderer's vertex). `plane` is derived
/// server-side from `kind` via [`plane_of`] — the renderer colours by `plane`
/// and must never re-derive it.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SubgraphNode {
    pub uid: String,
    pub name: String,
    pub kind: String,
    pub path: String,
    pub plane: String,
}

/// One edge in the subgraph payload (the renderer's link), carrying the visual
/// encoding inputs: `kind` (colour/shape), `provenance` + `confidence`
/// (opacity/width, dashed-when-ambiguous).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SubgraphEdge {
    pub src: String,
    pub dst: String,
    pub kind: String,
    pub provenance: String,
    pub confidence: f32,
}

/// The `subgraph` command result: nodes + edges + a truncation flag.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SubgraphDto {
    pub nodes: Vec<SubgraphNode>,
    pub edges: Vec<SubgraphEdge>,
    pub truncated: bool,
}

/// Serialise a `Copy` enum (NodeKind/EdgeKind/Provenance) to its bare serde
/// variant name (e.g. `"Function"`), matching the names `call_tool` emits so the
/// GUI's two payload sources agree.
fn variant_name<T: Serialize>(value: T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| match v {
            Value::String(s) => Some(s),
            _ => None,
        })
        .unwrap_or_default()
}

fn node_dto(n: &Node) -> SubgraphNode {
    SubgraphNode {
        uid: n.uid.as_str().to_string(),
        name: n.name.clone(),
        kind: variant_name(n.kind),
        path: n.path.clone(),
        plane: plane_of(n.kind).to_string(),
    }
}

/// Parse the optional plane filter (`"code"`/`"contract"`/`"infra"`).
///
/// As with [`parse_kinds`], an unrecognised plane name is an error rather than a
/// silent no-op. An empty/`None` filter means "all planes". Returns the set of
/// accepted plane names (owned, for cheap membership tests during the walk).
fn parse_planes(planes: &Option<Vec<String>>) -> Result<Option<HashSet<String>>, String> {
    let Some(names) = planes else {
        return Ok(None);
    };
    let mut out = HashSet::with_capacity(names.len());
    for name in names {
        if !PLANES.contains(&name.as_str()) {
            return Err(format!("unknown plane: {name}"));
        }
        out.insert(name.clone());
    }
    Ok(Some(out))
}

/// Parse the optional edge-kind filter (serde names) into `EdgeKind`s.
///
/// An unrecognised name is an error rather than a silent no-op — a typo'd filter
/// that quietly returned the whole graph would be a nasty surprise for the
/// renderer. An empty/`None` filter means "all edge kinds".
fn parse_kinds(kinds: &Option<Vec<String>>) -> Result<Vec<EdgeKind>, String> {
    let Some(names) = kinds else {
        return Ok(Vec::new());
    };
    let mut out = Vec::with_capacity(names.len());
    for name in names {
        // Round-trip through serde so we accept exactly the variant spellings the
        // rest of the system uses (`"Calls"`, `"Produces"`, …).
        let parsed: EdgeKind = serde_json::from_value(Value::String(name.clone()))
            .map_err(|_| format!("unknown edge kind: {name}"))?;
        out.push(parsed);
    }
    Ok(out)
}

/// Compute the bounded both-directions subgraph around `uid`.
///
/// See the module docs for the guarantees. `kinds` is the optional edge-kind
/// filter (serde names); `planes` is the optional plane filter
/// (`"code"`/`"contract"`/`"infra"`); `depth` is clamped to [`MAX_DEPTH`].
///
/// The plane filter restricts which nodes are admitted to the result *and*
/// traversed through — a node whose plane is not in the set is neither emitted
/// nor enqueued. The one exception is the focus node, which is always present
/// (the caller asked to inspect it): a filter that excludes the focus yields the
/// focus alone, never an error.
pub fn compute_subgraph(
    graph: &Graph,
    uid: &str,
    depth: u32,
    kinds: &Option<Vec<String>>,
    planes: &Option<Vec<String>>,
) -> Result<SubgraphDto, String> {
    let start = Uid(uid.to_string());
    if graph.get_node(&start).is_none() {
        return Err(format!("node not found: {uid}"));
    }

    let kind_filter = parse_kinds(kinds)?;
    let plane_filter = parse_planes(planes)?;
    // Whether a node passes the plane filter (no filter ⇒ everything passes).
    let plane_ok = |n: &Node| {
        plane_filter
            .as_ref()
            .map(|set| set.contains(plane_of(n.kind)))
            .unwrap_or(true)
    };
    let depth = depth.min(MAX_DEPTH);

    // BFS bookkeeping. `visited` gates node enqueueing; `edge_seen` de-dups the
    // emitted edge list (an edge can be reached from either endpoint).
    let mut visited: HashSet<Uid> = HashSet::new();
    let mut edge_seen: HashSet<(Uid, Uid, EdgeKind)> = HashSet::new();
    let mut nodes: Vec<SubgraphNode> = Vec::new();
    let mut edges: Vec<SubgraphEdge> = Vec::new();
    let mut truncated = false;

    // Seed with the focus node. `get_node` is Some (checked above).
    visited.insert(start.clone());
    nodes.push(node_dto(
        graph.get_node(&start).expect("focus node present"),
    ));

    // Queue holds `(uid, depth_remaining)`.
    let mut queue: VecDeque<(Uid, u32)> = VecDeque::new();
    queue.push_back((start, depth));

    'bfs: while let Some((current, remaining)) = queue.pop_front() {
        if remaining == 0 {
            continue;
        }
        for dir in [Direction::Outgoing, Direction::Incoming] {
            for (edge, target) in graph.neighbors(&current, dir, &kind_filter) {
                // A target whose plane is filtered out is not part of this view:
                // skip the node *and* its connecting edge (emitting an edge to a
                // node we never add would dangle in the renderer).
                if !plane_ok(target) {
                    continue;
                }

                // Record the edge once (keyed on its true src/dst, not the
                // traversal direction) so each appears a single time.
                let edge_key = (edge.src.clone(), edge.dst.clone(), edge.kind);
                if edge_seen.insert(edge_key) {
                    edges.push(SubgraphEdge {
                        src: edge.src.as_str().to_string(),
                        dst: edge.dst.as_str().to_string(),
                        kind: variant_name(edge.kind),
                        provenance: variant_name(edge.provenance),
                        confidence: edge.confidence.value(),
                    });
                }

                if visited.contains(&target.uid) {
                    continue;
                }
                // Enforce the node cap before admitting a new node. Hitting it
                // stops the whole walk and flags truncation — a partial-but-honest
                // neighbourhood beats an unbounded payload.
                if nodes.len() >= MAX_NODES {
                    truncated = true;
                    break 'bfs;
                }
                visited.insert(target.uid.clone());
                nodes.push(node_dto(target));
                queue.push_back((target.uid.clone(), remaining - 1));
            }
        }
    }

    Ok(SubgraphDto {
        nodes,
        edges,
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use strata_core::{Confidence, Edge, NodeKind, Provenance, Span};

    fn node(uid: &str) -> Node {
        node_kind(uid, NodeKind::Function)
    }

    /// A node with an explicit [`NodeKind`] (for plane-filter tests).
    fn node_kind(uid: &str, kind: NodeKind) -> Node {
        Node {
            uid: Uid(uid.into()),
            kind,
            name: uid.into(),
            fqn: uid.into(),
            path: format!("{uid}.ts"),
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

    /// A chain a → b → c → d → e (each Calls the next), used to test depth.
    fn chain() -> Graph {
        let mut g = Graph::new();
        for u in ["a", "b", "c", "d", "e"] {
            g.add_node(node(u));
        }
        g.add_edge(edge("a", "b", EdgeKind::Calls));
        g.add_edge(edge("b", "c", EdgeKind::Calls));
        g.add_edge(edge("c", "d", EdgeKind::Calls));
        g.add_edge(edge("d", "e", EdgeKind::Calls));
        g
    }

    fn uids(dto: &SubgraphDto) -> Vec<&str> {
        dto.nodes.iter().map(|n| n.uid.as_str()).collect()
    }

    #[test]
    fn unknown_uid_is_error() {
        let g = chain();
        let err = compute_subgraph(&g, "zzz", 2, &None, &None).unwrap_err();
        assert!(
            err.contains("zzz"),
            "error should name the missing uid: {err}"
        );
    }

    #[test]
    fn depth_zero_returns_only_the_focus_node() {
        let g = chain();
        let dto = compute_subgraph(&g, "c", 0, &None, &None).unwrap();
        assert_eq!(uids(&dto), vec!["c"]);
        assert!(dto.edges.is_empty());
        assert!(!dto.truncated);
    }

    #[test]
    fn bfs_expands_both_directions() {
        // From `c`, depth 1 must reach both the caller `b` (incoming) and the
        // callee `d` (outgoing).
        let g = chain();
        let dto = compute_subgraph(&g, "c", 1, &None, &None).unwrap();
        let mut got = uids(&dto);
        got.sort_unstable();
        assert_eq!(got, vec!["b", "c", "d"]);
        // The two incident edges (b→c, c→d) are present exactly once each.
        assert_eq!(dto.edges.len(), 2);
    }

    #[test]
    fn depth_two_reaches_two_hops_each_way() {
        let g = chain();
        let dto = compute_subgraph(&g, "c", 2, &None, &None).unwrap();
        let mut got = uids(&dto);
        got.sort_unstable();
        assert_eq!(got, vec!["a", "b", "c", "d", "e"]);
    }

    #[test]
    fn depth_is_clamped_to_max_depth() {
        // A long chain; even asking for depth 99 must not exceed MAX_DEPTH hops.
        let mut g = Graph::new();
        let labels: Vec<String> = (0..10).map(|i| format!("n{i}")).collect();
        for l in &labels {
            g.add_node(node(l));
        }
        for w in labels.windows(2) {
            g.add_edge(edge(&w[0], &w[1], EdgeKind::Calls));
        }
        // Start at n0; only outgoing matters here. With MAX_DEPTH = 3 we reach
        // n0..=n3 (3 hops), never n4+.
        let dto = compute_subgraph(&g, "n0", 99, &None, &None).unwrap();
        let got: HashSet<&str> = uids(&dto).into_iter().collect();
        assert!(got.contains("n3"), "3 hops reachable");
        assert!(!got.contains("n4"), "4th hop must be clamped off");
    }

    #[test]
    fn edge_kind_filter_restricts_traversal() {
        // c Calls d, and c Imports x. Filtering to Calls must not reach x.
        let mut g = chain();
        g.add_node(node("x"));
        g.add_edge(edge("c", "x", EdgeKind::Imports));

        let only_calls = compute_subgraph(&g, "c", 1, &Some(vec!["Calls".into()]), &None).unwrap();
        let got: HashSet<&str> = uids(&only_calls).into_iter().collect();
        assert!(!got.contains("x"), "Imports edge must be filtered out");
        assert!(got.contains("d") && got.contains("b"), "Calls edges remain");

        // Filtering to Imports reaches x but not the Calls neighbours.
        let only_imports =
            compute_subgraph(&g, "c", 1, &Some(vec!["Imports".into()]), &None).unwrap();
        let got2: HashSet<&str> = uids(&only_imports).into_iter().collect();
        assert!(got2.contains("x"), "Imports edge followed");
        assert!(
            !got2.contains("d"),
            "Calls neighbour excluded under Imports filter"
        );
    }

    #[test]
    fn unknown_edge_kind_filter_is_error() {
        let g = chain();
        let err =
            compute_subgraph(&g, "c", 1, &Some(vec!["Frobnicate".into()]), &None).unwrap_err();
        assert!(
            err.contains("Frobnicate"),
            "a bad filter name must error, not silently match nothing: {err}"
        );
    }

    #[test]
    fn node_cap_truncates() {
        // A star: one hub with many outgoing edges, more than MAX_NODES leaves.
        let mut g = Graph::new();
        g.add_node(node("hub"));
        for i in 0..(MAX_NODES + 50) {
            let leaf = format!("leaf{i}");
            g.add_node(node(&leaf));
            g.add_edge(edge("hub", &leaf, EdgeKind::Calls));
        }
        let dto = compute_subgraph(&g, "hub", 1, &None, &None).unwrap();
        assert!(dto.truncated, "exceeding MAX_NODES must set truncated");
        assert_eq!(
            dto.nodes.len(),
            MAX_NODES,
            "node set is capped at MAX_NODES exactly"
        );
    }

    #[test]
    fn edge_carries_provenance_and_confidence() {
        let g = chain();
        let dto = compute_subgraph(&g, "a", 1, &None, &None).unwrap();
        let e = dto.edges.first().expect("one edge");
        assert_eq!(e.kind, "Calls");
        assert_eq!(e.provenance, "Inferred");
        assert!((e.confidence - 0.9).abs() < 1e-6);
    }

    // ── Plane derivation (the single source of truth the UI consumes). ──

    #[test]
    fn plane_of_maps_every_kind_to_its_plane() {
        // Code plane: program structure.
        for k in [
            NodeKind::Repo,
            NodeKind::Package,
            NodeKind::File,
            NodeKind::Module,
            NodeKind::Class,
            NodeKind::Interface,
            NodeKind::Function,
            NodeKind::Method,
        ] {
            assert_eq!(plane_of(k), "code", "{k:?} must be in the code plane");
        }
        // Contract plane: interface operations.
        for k in [NodeKind::ApiOperation, NodeKind::GraphqlField] {
            assert_eq!(
                plane_of(k),
                "contract",
                "{k:?} must be in the contract plane"
            );
        }
        // Infra plane: cloud resources.
        for k in [
            NodeKind::LambdaFn,
            NodeKind::IamRole,
            NodeKind::AppSyncApi,
            NodeKind::AppSyncResolver,
            NodeKind::AppSyncDataSource,
            NodeKind::CloudResource,
        ] {
            assert_eq!(plane_of(k), "infra", "{k:?} must be in the infra plane");
        }
        // Data plane: database tables and columns (Slice 16, D3).
        for k in [NodeKind::Table, NodeKind::Column] {
            assert_eq!(plane_of(k), "data", "{k:?} must be in the data plane");
        }
    }

    #[test]
    fn node_dto_carries_derived_plane() {
        let g = {
            let mut g = Graph::new();
            g.add_node(node_kind("fn", NodeKind::Function));
            g.add_node(node_kind("op", NodeKind::ApiOperation));
            g.add_edge(edge("fn", "op", EdgeKind::Produces));
            g
        };
        let dto = compute_subgraph(&g, "fn", 1, &None, &None).unwrap();
        let fn_node = dto.nodes.iter().find(|n| n.uid == "fn").unwrap();
        let op_node = dto.nodes.iter().find(|n| n.uid == "op").unwrap();
        assert_eq!(fn_node.plane, "code");
        assert_eq!(op_node.plane, "contract");
    }

    /// A code function that produces a contract operation, which is backed by an
    /// infra Lambda — one node per plane around the focus `fn`.
    fn tri_plane() -> Graph {
        let mut g = Graph::new();
        g.add_node(node_kind("fn", NodeKind::Function));
        g.add_node(node_kind("op", NodeKind::ApiOperation));
        g.add_node(node_kind("lam", NodeKind::LambdaFn));
        g.add_edge(edge("fn", "op", EdgeKind::Produces));
        g.add_edge(edge("lam", "op", EdgeKind::Produces));
        g
    }

    #[test]
    fn plane_filter_restricts_admitted_nodes() {
        // Filtering to {code, contract} from `fn` reaches `op` (contract) but not
        // the infra `lam`; the connecting fn→op edge survives, lam→op does not.
        let g = tri_plane();
        let dto = compute_subgraph(
            &g,
            "fn",
            2,
            &None,
            &Some(vec!["code".into(), "contract".into()]),
        )
        .unwrap();
        let got: HashSet<&str> = uids(&dto).into_iter().collect();
        assert!(
            got.contains("fn") && got.contains("op"),
            "code+contract kept"
        );
        assert!(!got.contains("lam"), "infra node filtered out");
        // No edge may reference the filtered-out node.
        assert!(
            dto.edges.iter().all(|e| e.src != "lam" && e.dst != "lam"),
            "no dangling edge to a filtered node: {:?}",
            dto.edges
        );
    }

    #[test]
    fn plane_filter_keeps_focus_even_when_excluded() {
        // The focus is a code `fn`; filtering to infra only must still return the
        // focus (you asked to inspect it) — and nothing else here.
        let g = tri_plane();
        let dto = compute_subgraph(&g, "fn", 2, &None, &Some(vec!["infra".into()])).unwrap();
        // `op` is contract (excluded), so the walk can't reach `lam` through it.
        let got: HashSet<&str> = uids(&dto).into_iter().collect();
        assert!(got.contains("fn"), "focus node always present");
        assert!(!got.contains("op"), "contract node excluded");
    }

    #[test]
    fn unknown_plane_filter_is_error() {
        let g = tri_plane();
        let err = compute_subgraph(&g, "fn", 1, &None, &Some(vec!["galaxy".into()])).unwrap_err();
        assert!(
            err.contains("galaxy"),
            "a bad plane name must error, not silently match nothing: {err}"
        );
    }
}
