//! Symbol resolution: identifier string → graph node(s).
//!
//! Resolution prefers an exact fully-qualified-name (`fqn`) match; if none
//! exists it falls back to an exact `name` match. All matches at the winning
//! tier are returned so callers can disambiguate.

use strata_core::{Graph, Node};

/// The outcome of resolving an identifier against the graph.
#[derive(Debug, Clone, PartialEq)]
pub enum ResolveOutcome {
    /// No node matched by fqn or name.
    None,
    /// Exactly one node matched — unambiguous.
    One(Node),
    /// Several nodes matched the same identifier; the caller must disambiguate.
    Many(Vec<Node>),
}

/// Resolve `ident` to graph node(s).
///
/// 1. Collect all nodes whose `fqn` equals `ident` exactly.
/// 2. If that set is empty, collect all nodes whose `name` equals `ident`.
///
/// Results are sorted by `uid` for determinism. An exact fqn match therefore
/// always "beats" a name match (the name tier is never consulted when the fqn
/// tier is non-empty).
pub fn resolve_symbol(graph: &Graph, ident: &str) -> ResolveOutcome {
    let mut by_fqn: Vec<Node> = graph.nodes().filter(|n| n.fqn == ident).cloned().collect();
    let mut matches = if !by_fqn.is_empty() {
        by_fqn.sort_by(|a, b| a.uid.cmp(&b.uid));
        by_fqn
    } else {
        let mut by_name: Vec<Node> = graph.nodes().filter(|n| n.name == ident).cloned().collect();
        by_name.sort_by(|a, b| a.uid.cmp(&b.uid));
        by_name
    };

    match matches.len() {
        0 => ResolveOutcome::None,
        1 => ResolveOutcome::One(matches.remove(0)),
        _ => ResolveOutcome::Many(matches),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use strata_core::{Confidence, NodeKind, Provenance, Span, Uid};

    fn node(uid: &str, name: &str, fqn: &str) -> Node {
        Node {
            uid: Uid(uid.into()),
            kind: NodeKind::Function,
            name: name.into(),
            fqn: fqn.into(),
            path: "x.ts".into(),
            span: Span::default(),
            provenance: Provenance::Extracted,
            confidence: Confidence::new(1.0),
        }
    }

    #[test]
    fn exact_fqn_beats_name() {
        let mut g = Graph::new();
        // One node's *name* is "foo"; another node's *fqn* is "foo".
        g.add_node(node("u1", "foo", "mod.foo"));
        g.add_node(node("u2", "wrapper", "foo"));
        match resolve_symbol(&g, "foo") {
            ResolveOutcome::One(n) => assert_eq!(n.uid.as_str(), "u2"),
            other => panic!("expected single fqn match, got {other:?}"),
        }
    }

    #[test]
    fn name_match_when_no_fqn_match() {
        let mut g = Graph::new();
        g.add_node(node("u1", "foo", "mod.foo"));
        match resolve_symbol(&g, "foo") {
            ResolveOutcome::One(n) => assert_eq!(n.uid.as_str(), "u1"),
            other => panic!("expected single name match, got {other:?}"),
        }
    }

    #[test]
    fn two_nodes_same_name_are_ambiguous() {
        let mut g = Graph::new();
        g.add_node(node("u1", "foo", "a.foo"));
        g.add_node(node("u2", "foo", "b.foo"));
        match resolve_symbol(&g, "foo") {
            ResolveOutcome::Many(c) => {
                let ids: Vec<&str> = c.iter().map(|n| n.uid.as_str()).collect();
                assert_eq!(ids, vec!["u1", "u2"]);
            }
            other => panic!("expected ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn unknown_ident_resolves_to_none() {
        let g = Graph::new();
        assert_eq!(resolve_symbol(&g, "nope"), ResolveOutcome::None);
    }
}
