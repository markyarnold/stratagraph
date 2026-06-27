//! Pure `build_graph` behavioural tests (no IO; file maps constructed inline).
//!
//! These define cross-file impact correctness — most importantly test 1, the
//! cross-file import+call payoff where `impact(foo)` finds `run` in another file.

use std::collections::{BTreeMap, BTreeSet};

use strata_core::{impact, Direction, EdgeKind, Graph, ImpactOptions, NodeKind, Provenance, Uid};
use strata_index::build_graph;
use strata_lang_ts::ResolveOptions;

const REPO: &str = "app";

// ── helpers ──────────────────────────────────────────────────────────────────

fn files(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(p, s)| (p.to_string(), s.to_string()))
        .collect()
}

fn uid_module(path: &str) -> Uid {
    Uid::new("ts", REPO, path, "<module>", "")
}

fn uid_symbol(path: &str, fqn: &str) -> Uid {
    Uid::new("ts", REPO, path, fqn, "")
}

fn uid_repo() -> Uid {
    Uid::new("ts", REPO, "", REPO, "")
}

fn uid_package(pkg: &str) -> Uid {
    Uid::new("ts", "<external>", "", pkg, "")
}

/// Find a directed edge of `kind` from `src` to `dst`, returning its provenance.
fn edge_provenance(g: &Graph, src: &Uid, dst: &Uid, kind: EdgeKind) -> Option<Provenance> {
    g.neighbors(src, Direction::Outgoing, &[kind])
        .into_iter()
        .find(|(e, _)| &e.dst == dst)
        .map(|(e, _)| e.provenance)
}

fn has_edge(g: &Graph, src: &Uid, dst: &Uid, kind: EdgeKind) -> bool {
    edge_provenance(g, src, dst, kind).is_some()
}

/// Outgoing edges of `kind` from `src`, sorted by destination uid.
fn out_edges(g: &Graph, src: &Uid, kind: EdgeKind) -> Vec<Uid> {
    let mut v: Vec<Uid> = g
        .neighbors(src, Direction::Outgoing, &[kind])
        .into_iter()
        .map(|(e, _)| e.dst.clone())
        .collect();
    v.sort();
    v
}

/// Collect (src, dst, kind, provenance) for every edge in the graph.
fn all_edges(g: &Graph) -> BTreeSet<(String, String, String, String)> {
    let mut set = BTreeSet::new();
    let uids: Vec<Uid> = g.nodes().map(|n| n.uid.clone()).collect();
    for uid in &uids {
        for (edge, _) in g.neighbors(uid, Direction::Outgoing, &[]) {
            set.insert((
                edge.src.to_string(),
                edge.dst.to_string(),
                format!("{:?}", edge.kind),
                format!("{:?}", edge.provenance),
            ));
        }
    }
    set
}

fn all_nodes(g: &Graph) -> BTreeSet<String> {
    g.nodes().map(|n| n.uid.to_string()).collect()
}

// ── MEMBER_OF chain Function -> Module -> Repo ──────────────────────────────

#[test]
fn member_of_chain_reaches_repo() {
    let f = files(&[("src/a.ts", "export function run() {}")]);
    let g = build_graph(&f, REPO, &ResolveOptions::default());

    let run = uid_symbol("src/a.ts", "run");
    let module = uid_module("src/a.ts");
    let repo = uid_repo();

    // Function -MEMBER_OF-> Module.
    let to_module: Vec<Uid> = g
        .neighbors(&run, Direction::Outgoing, &[EdgeKind::MemberOf])
        .into_iter()
        .map(|(e, _)| e.dst.clone())
        .collect();
    assert_eq!(to_module, vec![module.clone()]);

    // Module -MEMBER_OF-> Repo.
    let to_repo: Vec<Uid> = g
        .neighbors(&module, Direction::Outgoing, &[EdgeKind::MemberOf])
        .into_iter()
        .map(|(e, _)| e.dst.clone())
        .collect();
    assert_eq!(to_repo, vec![repo.clone()]);

    // And the reciprocal DEFINES edges exist.
    assert!(has_edge(&g, &repo, &module, EdgeKind::Defines));
    assert!(has_edge(&g, &module, &run, EdgeKind::Defines));
}

// ── Method attaches to its class (same file) ────────────────────────────────

#[test]
fn method_is_member_of_its_class() {
    let f = files(&[("src/svc.ts", "class A { run() {} }")]);
    let g = build_graph(&f, REPO, &ResolveOptions::default());

    let class = uid_symbol("src/svc.ts", "A");
    let method = uid_symbol("src/svc.ts", "A.run");
    assert!(has_edge(&g, &method, &class, EdgeKind::MemberOf));
    assert!(has_edge(&g, &class, &method, EdgeKind::Defines));
}

// ── Resolved cross-file import edge ─────────────────────────────────────────

#[test]
fn resolved_relative_import_links_modules() {
    let f = files(&[
        ("src/a.ts", "import { foo } from \"./b\";"),
        ("src/b.ts", "export function foo() {}"),
    ]);
    let g = build_graph(&f, REPO, &ResolveOptions::default());

    assert_eq!(
        edge_provenance(
            &g,
            &uid_module("src/a.ts"),
            &uid_module("src/b.ts"),
            EdgeKind::Imports
        ),
        Some(Provenance::Extracted),
        "a.ts must IMPORT b.ts"
    );
}

// ── External import -> Package node + IMPORTS edge ──────────────────────────

#[test]
fn external_import_creates_package_node_and_edge() {
    let f = files(&[("src/ui.tsx", "import React from \"react\";")]);
    let g = build_graph(&f, REPO, &ResolveOptions::default());

    let pkg = uid_package("react");
    let node = g.get_node(&pkg).expect("react Package node must exist");
    assert_eq!(node.kind, NodeKind::Package);
    assert_eq!(node.name, "react");

    assert_eq!(
        edge_provenance(&g, &uid_module("src/ui.tsx"), &pkg, EdgeKind::Imports),
        Some(Provenance::Extracted),
        "ui.tsx must IMPORT Package(react)"
    );
}

// ── Unresolved import -> no edge, no panic ──────────────────────────────────

#[test]
fn unresolved_import_produces_no_edge() {
    let f = files(&[("src/a.ts", "import \"./missing\";")]);
    let g = build_graph(&f, REPO, &ResolveOptions::default());

    let module = uid_module("src/a.ts");
    assert!(
        out_edges(&g, &module, EdgeKind::Imports).is_empty(),
        "no IMPORTS edge for an unresolved specifier"
    );
    // No invented target nodes: only Repo + Module(a).
    assert_eq!(g.node_count(), 2);
}

// ── Test 1: cross-file import + call (the payoff) ───────────────────────────

#[test]
fn cross_file_import_and_call_enables_impact() {
    let f = files(&[
        (
            "src/a.ts",
            "import { foo } from \"./b\"; export function run() { foo(); }",
        ),
        ("src/b.ts", "export function foo() {}"),
    ]);
    let g = build_graph(&f, REPO, &ResolveOptions::default());

    // Module(a) IMPORTS Module(b), Extracted.
    assert_eq!(
        edge_provenance(
            &g,
            &uid_module("src/a.ts"),
            &uid_module("src/b.ts"),
            EdgeKind::Imports
        ),
        Some(Provenance::Extracted),
        "a.ts must IMPORT b.ts"
    );

    // run (fqn "run" in a.ts) CALLS foo (fqn "foo" in b.ts), Inferred.
    let run = uid_symbol("src/a.ts", "run");
    let foo = uid_symbol("src/b.ts", "foo");
    assert_eq!(
        edge_provenance(&g, &run, &foo, EdgeKind::Calls),
        Some(Provenance::Inferred),
        "run must CALL the imported foo with Inferred provenance"
    );

    // The payoff: cross-file blast radius.
    let result = impact(&g, &foo, &ImpactOptions::default());
    assert!(
        result.affected.iter().any(|a| a.uid == run),
        "impact(foo) must include run (cross-file). affected = {:?}",
        result
            .affected
            .iter()
            .map(|a| a.uid.as_str())
            .collect::<Vec<_>>()
    );
}

// ── Test 2: same-file call resolves to the local definition ─────────────────

#[test]
fn same_file_call_resolves_locally() {
    let f = files(&[(
        "src/a.ts",
        "function helper() {} export function run() { helper(); }",
    )]);
    let g = build_graph(&f, REPO, &ResolveOptions::default());

    let run = uid_symbol("src/a.ts", "run");
    let helper = uid_symbol("src/a.ts", "helper");
    assert_eq!(
        edge_provenance(&g, &run, &helper, EdgeKind::Calls),
        Some(Provenance::Inferred),
        "run -> helper, same file, single candidate => Inferred"
    );
    // Exactly one CALLS edge from run.
    assert_eq!(out_edges(&g, &run, EdgeKind::Calls), vec![helper]);
}

// ── Test 3: this.method() resolves within the same class only ───────────────

#[test]
fn this_method_resolves_within_same_class() {
    let f = files(&[(
        "src/svc.ts",
        "class A { run() { this.help(); } help() {} } class B { help() {} }",
    )]);
    let g = build_graph(&f, REPO, &ResolveOptions::default());

    let a_run = uid_symbol("src/svc.ts", "A.run");
    let a_help = uid_symbol("src/svc.ts", "A.help");
    let b_help = uid_symbol("src/svc.ts", "B.help");

    assert_eq!(
        edge_provenance(&g, &a_run, &a_help, EdgeKind::Calls),
        Some(Provenance::Inferred),
        "A.run this.help() -> A.help"
    );
    assert!(
        !has_edge(&g, &a_run, &b_help, EdgeKind::Calls),
        "A.run must NOT call B.help (different class)"
    );
    assert_eq!(out_edges(&g, &a_run, EdgeKind::Calls), vec![a_help]);
}

// ── Test 7: ambiguous over-inclusion (local + imported same name) ───────────

#[test]
fn ambiguous_bare_call_emits_edges_to_all_candidates() {
    let f = files(&[
        (
            "src/a.ts",
            "import { helper } from \"./b\"; function helper() {} export function run() { helper(); }",
        ),
        ("src/b.ts", "export function helper() {}"),
    ]);
    let g = build_graph(&f, REPO, &ResolveOptions::default());

    let run = uid_symbol("src/a.ts", "run");
    let local_helper = uid_symbol("src/a.ts", "helper");
    let imported_helper = uid_symbol("src/b.ts", "helper");

    // Both candidates receive a CALLS edge, both Ambiguous.
    assert_eq!(
        edge_provenance(&g, &run, &local_helper, EdgeKind::Calls),
        Some(Provenance::Ambiguous)
    );
    assert_eq!(
        edge_provenance(&g, &run, &imported_helper, EdgeKind::Calls),
        Some(Provenance::Ambiguous)
    );
    assert_eq!(
        out_edges(&g, &run, EdgeKind::Calls),
        {
            let mut v = vec![local_helper, imported_helper];
            v.sort();
            v
        },
        "both same-named candidates included"
    );
}

// ── Unknown receiver `other.method()` over-includes repo-wide methods ───────

#[test]
fn unknown_receiver_includes_all_same_named_methods() {
    let f = files(&[
        (
            "src/a.ts",
            "import { svc } from \"./b\"; export function run() { svc.save(); }",
        ),
        (
            "src/b.ts",
            "export class S { save() {} } export class T { save() {} }",
        ),
    ]);
    let g = build_graph(&f, REPO, &ResolveOptions::default());

    let run = uid_symbol("src/a.ts", "run");
    let s_save = uid_symbol("src/b.ts", "S.save");
    let t_save = uid_symbol("src/b.ts", "T.save");

    // No type info for `svc`: recall-biased, both methods named `save` included.
    assert_eq!(
        edge_provenance(&g, &run, &s_save, EdgeKind::Calls),
        Some(Provenance::Ambiguous)
    );
    assert_eq!(
        edge_provenance(&g, &run, &t_save, EdgeKind::Calls),
        Some(Provenance::Ambiguous)
    );
}

// ── Test 8: determinism ─────────────────────────────────────────────────────

#[test]
fn build_graph_is_deterministic() {
    let f = files(&[
        (
            "src/a.ts",
            "import { foo } from \"./b\"; export function run() { foo(); this; }",
        ),
        (
            "src/b.ts",
            "export function foo() {} export class C { m() { this.m(); } }",
        ),
        (
            "src/c.ts",
            "import React from \"react\"; const x = () => {};",
        ),
    ]);
    let g1 = build_graph(&f, REPO, &ResolveOptions::default());
    let g2 = build_graph(&f, REPO, &ResolveOptions::default());

    assert_eq!(g1.node_count(), g2.node_count());
    assert_eq!(g1.edge_count(), g2.edge_count());
    assert_eq!(all_nodes(&g1), all_nodes(&g2));
    assert_eq!(all_edges(&g1), all_edges(&g2));
}
