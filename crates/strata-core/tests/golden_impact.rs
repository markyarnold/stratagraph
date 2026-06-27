use serde_json::Value;
use strata_core::{
    impact, Confidence, Edge, EdgeKind, Graph, ImpactOptions, Node, NodeKind, Provenance, Span, Uid,
};

fn load(path: &str) -> Value {
    let text = std::fs::read_to_string(path).expect("fixture file should exist");
    serde_json::from_str(&text).expect("fixture should be valid JSON")
}

fn build_graph(spec: &Value) -> Graph {
    let mut g = Graph::new();
    for n in spec["nodes"].as_array().unwrap() {
        let uid = Uid(n["uid"].as_str().unwrap().to_string());
        g.add_node(Node {
            uid,
            kind: NodeKind::Function,
            name: n["name"].as_str().unwrap().to_string(),
            fqn: n["name"].as_str().unwrap().to_string(),
            path: "fixture".into(),
            span: Span::default(),
            provenance: Provenance::Extracted,
            confidence: Confidence::new(1.0),
        });
    }
    for e in spec["edges"].as_array().unwrap() {
        let provenance = if e["ambiguous"].as_bool().unwrap() {
            Provenance::Ambiguous
        } else {
            Provenance::Inferred
        };
        g.add_edge(Edge {
            src: Uid(e["src"].as_str().unwrap().to_string()),
            dst: Uid(e["dst"].as_str().unwrap().to_string()),
            kind: EdgeKind::Calls,
            provenance,
            confidence: Confidence::new(e["confidence"].as_f64().unwrap() as f32),
        });
    }
    g
}

#[test]
fn impact_matches_golden_blast_radius() {
    let graph = build_graph(&load("tests/fixtures/chain.graph.json"));
    let expected = load("tests/fixtures/chain.impact.json");

    let result = impact(
        &graph,
        &Uid(expected["target"].as_str().unwrap().to_string()),
        &ImpactOptions::default(),
    );

    let expected_affected = expected["affected"].as_array().unwrap();
    assert_eq!(
        result.affected.len(),
        expected_affected.len(),
        "blast radius size must match the golden fixture (recall guard)"
    );
    for (got, want) in result.affected.iter().zip(expected_affected) {
        assert_eq!(got.uid.as_str(), want["uid"].as_str().unwrap());
        assert_eq!(got.depth, want["depth"].as_u64().unwrap() as usize);
        assert!((got.confidence - want["confidence"].as_f64().unwrap() as f32).abs() < 1e-5);
        assert_eq!(got.ambiguous, want["ambiguous"].as_bool().unwrap());
    }
}

#[test]
fn impact_is_deterministic() {
    let graph = build_graph(&load("tests/fixtures/chain.graph.json"));
    let target = Uid("ts|app|src/c.ts|c|()".into());
    let a = impact(&graph, &target, &ImpactOptions::default());
    let b = impact(&graph, &target, &ImpactOptions::default());
    assert_eq!(a, b, "identical inputs must produce identical output");
}
