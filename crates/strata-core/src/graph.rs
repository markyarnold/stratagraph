use std::collections::HashMap;

use crate::ids::Uid;
use crate::model::{Edge, EdgeKind, Node};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Outgoing,
    Incoming,
}

/// In-memory typed graph: node map + edge list + forward/reverse adjacency.
///
/// The representation is intentionally simple and correct for slice 1.
/// It can be swapped for a compressed sparse adjacency structure later
/// behind this same API without changing callers.
#[derive(Debug, Default, Clone)]
pub struct Graph {
    nodes: HashMap<Uid, Node>,
    edges: Vec<Edge>,
    out_adj: HashMap<Uid, Vec<usize>>,
    in_adj: HashMap<Uid, Vec<usize>>,
}

impl Graph {
    pub fn new() -> Graph {
        Graph::default()
    }

    /// Insert or replace a node. Adjacency buckets are created eagerly so
    /// isolated nodes are still queryable.
    pub fn add_node(&mut self, node: Node) {
        self.out_adj.entry(node.uid.clone()).or_default();
        self.in_adj.entry(node.uid.clone()).or_default();
        self.nodes.insert(node.uid.clone(), node);
    }

    pub fn get_node(&self, uid: &Uid) -> Option<&Node> {
        self.nodes.get(uid)
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }

    pub fn add_edge(&mut self, edge: Edge) {
        let idx = self.edges.len();
        self.out_adj.entry(edge.src.clone()).or_default().push(idx);
        self.in_adj.entry(edge.dst.clone()).or_default().push(idx);
        self.edges.push(edge);
    }

    /// Neighbours of `uid` following `dir`, filtered to `kinds`
    /// (an empty `kinds` slice means "all edge kinds").
    pub fn neighbors(&self, uid: &Uid, dir: Direction, kinds: &[EdgeKind]) -> Vec<(&Edge, &Node)> {
        let adj = match dir {
            Direction::Outgoing => &self.out_adj,
            Direction::Incoming => &self.in_adj,
        };
        let Some(indices) = adj.get(uid) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for &i in indices {
            let edge = &self.edges[i];
            if !kinds.is_empty() && !kinds.contains(&edge.kind) {
                continue;
            }
            let other = match dir {
                Direction::Outgoing => &edge.dst,
                Direction::Incoming => &edge.src,
            };
            if let Some(node) = self.nodes.get(other) {
                out.push((edge, node));
            }
        }
        out
    }

    pub fn nodes(&self) -> impl Iterator<Item = &Node> {
        self.nodes.values()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Confidence, NodeKind, Provenance, Span};

    fn node(uid: &str, name: &str) -> Node {
        Node {
            uid: Uid(uid.to_string()),
            kind: NodeKind::Function,
            name: name.to_string(),
            fqn: name.to_string(),
            path: "x.ts".to_string(),
            span: Span::default(),
            provenance: Provenance::Extracted,
            confidence: Confidence::new(1.0),
        }
    }

    fn edge(src: &str, dst: &str, kind: EdgeKind) -> Edge {
        Edge {
            src: Uid(src.to_string()),
            dst: Uid(dst.to_string()),
            kind,
            provenance: Provenance::Inferred,
            confidence: Confidence::new(0.9),
        }
    }

    #[test]
    fn add_node_is_idempotent_by_uid() {
        let mut g = Graph::new();
        g.add_node(node("a", "a"));
        g.add_node(node("a", "a-renamed"));
        assert_eq!(g.node_count(), 1);
        assert_eq!(g.get_node(&Uid("a".into())).unwrap().name, "a-renamed");
    }

    #[test]
    fn neighbors_follow_direction_and_kind() {
        let mut g = Graph::new();
        g.add_node(node("a", "a"));
        g.add_node(node("b", "b"));
        g.add_edge(edge("a", "b", EdgeKind::Calls)); // a calls b

        let out = g.neighbors(&Uid("a".into()), Direction::Outgoing, &[EdgeKind::Calls]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].1.name, "b");

        let incoming = g.neighbors(&Uid("b".into()), Direction::Incoming, &[EdgeKind::Calls]);
        assert_eq!(incoming.len(), 1);
        assert_eq!(incoming[0].1.name, "a");

        let wrong_kind = g.neighbors(&Uid("a".into()), Direction::Outgoing, &[EdgeKind::Imports]);
        assert!(wrong_kind.is_empty());
    }
}
