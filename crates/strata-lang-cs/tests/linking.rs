//! C# linking tests — the band-disciplined call/using/structural graph.
//!
//! Each builds an in-memory `path → AnalyzedFile` set with [`analyze`], runs
//! [`assemble_csharp`], and asserts on the resulting [`Graph`]. The confidence
//! bands (design §4.1) are LAW: a heuristic edge is never Extracted-1.0 and an
//! ambiguous one is always < 0.40. The deliberate-ambiguity fixture (two
//! same-named methods, an unknown-receiver call) and the reflection-unmatched
//! fixture are MANDATORY and pin the honest outcomes.

use std::collections::BTreeMap;

use strata_core::{AnalyzedFile, Direction, EdgeKind, Graph, NodeKind, Provenance, Uid};
use strata_lang_cs::{analyze, assemble_csharp};

const REPO: &str = "csrepo";

fn build(files: &[(&str, &str)]) -> Graph {
    let analyzed: BTreeMap<String, AnalyzedFile> = files
        .iter()
        .map(|(p, s)| (p.to_string(), analyze(p, s)))
        .collect();
    let mut g = Graph::new();
    assemble_csharp(&mut g, REPO, &analyzed);
    g
}

fn uid_module(path: &str) -> Uid {
    Uid::new("cs", REPO, path, "<module>", "")
}
fn uid_symbol(path: &str, fqn: &str) -> Uid {
    Uid::new("cs", REPO, path, fqn, "")
}

fn calls_from(g: &Graph, src: &Uid) -> Vec<(Uid, Provenance, f32)> {
    g.neighbors(src, Direction::Outgoing, &[EdgeKind::Calls])
        .into_iter()
        .map(|(e, _)| (e.dst.clone(), e.provenance, e.confidence.value()))
        .collect()
}

fn has_edge(edges: &[(Uid, Provenance, f32)], dst: &Uid, prov: Provenance, conf: f32) -> bool {
    edges
        .iter()
        .any(|(d, p, c)| d == dst && *p == prov && (*c - conf).abs() < 1e-6)
}

// ── Structural: nodes + Module/Repo/Class/Method/Interface wiring ────────────

#[test]
fn builds_module_symbol_nodes_and_structural_edges() {
    let g = build(&[(
        "src/W.cs",
        "namespace App { public interface ISvc { } public class Worker : ISvc { public void Run() { } } }",
    )]);

    assert!(
        g.get_node(&uid_module("src/W.cs")).is_some(),
        "module node missing"
    );
    assert_eq!(
        g.get_node(&uid_symbol("src/W.cs", "App.ISvc"))
            .map(|n| n.kind),
        Some(NodeKind::Interface)
    );
    assert_eq!(
        g.get_node(&uid_symbol("src/W.cs", "App.Worker"))
            .map(|n| n.kind),
        Some(NodeKind::Class)
    );
    assert_eq!(
        g.get_node(&uid_symbol("src/W.cs", "App.Worker.Run"))
            .map(|n| n.kind),
        Some(NodeKind::Method)
    );

    // The method is MEMBER_OF its (namespace-qualified) class.
    let run_members = g.neighbors(
        &uid_symbol("src/W.cs", "App.Worker.Run"),
        Direction::Outgoing,
        &[EdgeKind::MemberOf],
    );
    assert!(
        run_members
            .iter()
            .any(|(e, _)| e.dst == uid_symbol("src/W.cs", "App.Worker")),
        "Run must be MEMBER_OF App.Worker"
    );
}

// ── Rule: same-file bare call → Extracted 0.95 ───────────────────────────────

#[test]
fn same_file_bare_call_is_extracted() {
    let g = build(&[(
        "src/C.cs",
        "public class C { void Target() { } void Caller() { Target(); } }",
    )]);
    let edges = calls_from(&g, &uid_symbol("src/C.cs", "C.Caller"));
    assert!(
        has_edge(
            &edges,
            &uid_symbol("src/C.cs", "C.Target"),
            Provenance::Extracted,
            0.95
        ),
        "Caller —Calls→ Target (Extracted 0.95): {edges:?}"
    );
}

// ── Rule: this.M() → own-type method, Inferred 0.80 ──────────────────────────

#[test]
fn this_method_call_resolves_to_own_type_method_inferred() {
    let g = build(&[(
        "src/C.cs",
        "public class C { void Helper() { } void Run() { this.Helper(); } }",
    )]);
    let edges = calls_from(&g, &uid_symbol("src/C.cs", "C.Run"));
    assert!(
        has_edge(
            &edges,
            &uid_symbol("src/C.cs", "C.Helper"),
            Provenance::Inferred,
            0.80
        ),
        "C.Run —Calls→ C.Helper via this (Inferred 0.80): {edges:?}"
    );
}

#[test]
fn this_method_call_to_unknown_method_makes_no_edge() {
    // `this.Ghost()` where the type has no `Ghost`: a method might come from a base
    // type we cannot see — honest miss, never guessed.
    let g = build(&[(
        "src/C.cs",
        "public class C { void Run() { this.Ghost(); } }",
    )]);
    let edges = calls_from(&g, &uid_symbol("src/C.cs", "C.Run"));
    assert!(
        edges.is_empty(),
        "this.Ghost() resolves to nothing → no edge, got {edges:?}"
    );
}

// ── Rule: cross-file unique repo-wide name → Inferred 0.80 ───────────────────

#[test]
fn cross_file_unique_name_is_inferred() {
    // `Caller` in B.cs calls `Only()` with no same-file def. Exactly ONE `Only`
    // exists repo-wide (in A.cs) → a unique cross-file name match at Inferred 0.80
    // (a single confident heuristic guess, never a fact; C# `using` imports a
    // namespace not a symbol, so there is no stronger import binding to use).
    let g = build(&[
        ("src/A.cs", "public class A { public void Only() { } }"),
        ("src/B.cs", "public class B { void Caller() { Only(); } }"),
    ]);
    let edges = calls_from(&g, &uid_symbol("src/B.cs", "B.Caller"));
    assert!(
        has_edge(
            &edges,
            &uid_symbol("src/A.cs", "A.Only"),
            Provenance::Inferred,
            0.80
        ),
        "Caller —Calls→ A.Only (unique cross-file, Inferred 0.80): {edges:?}"
    );
}

// ── Rule (MANDATORY): deliberate ambiguity — two same-named methods ──────────

#[test]
fn deliberate_ambiguity_unknown_receiver_is_ambiguous_fanout() {
    // Two `Save` methods on different types and a caller doing `acct.Save()` with
    // an unknown receiver type. With several same-named candidates the heuristic
    // CANNOT know which → it fans out to BOTH at the AMBIGUOUS band (< 0.40), never
    // a confident single edge. THIS pins the honest ambiguous outcome.
    let g = build(&[
        ("src/U.cs", "public class User { public void Save() { } }"),
        (
            "src/Acc.cs",
            "public class Account { public void Save() { } }",
        ),
        (
            "src/R.cs",
            "public class Runner { void Run(Account acct) { acct.Save(); } }",
        ),
    ]);
    let edges = calls_from(&g, &uid_symbol("src/R.cs", "Runner.Run"));

    assert!(
        has_edge(
            &edges,
            &uid_symbol("src/U.cs", "User.Save"),
            Provenance::Ambiguous,
            0.35
        ),
        "Run —Calls→ User.Save (Ambiguous 0.35): {edges:?}"
    );
    assert!(
        has_edge(
            &edges,
            &uid_symbol("src/Acc.cs", "Account.Save"),
            Provenance::Ambiguous,
            0.35
        ),
        "Run —Calls→ Account.Save (Ambiguous 0.35): {edges:?}"
    );
    assert_eq!(
        edges.len(),
        2,
        "exactly the two ambiguous candidates: {edges:?}"
    );
    for (_, prov, conf) in &edges {
        assert_eq!(*prov, Provenance::Ambiguous);
        assert!(*conf < 0.40, "ambiguous edge conf {conf} must be < 0.40");
    }
}

#[test]
fn bare_call_with_two_repo_wide_candidates_is_ambiguous() {
    // Two module-level-style methods `Process` on different types, a bare
    // `Process()` with no same-file def → fan out to both at Ambiguous.
    let g = build(&[
        ("src/A.cs", "public class A { public void Process() { } }"),
        ("src/B.cs", "public class B { public void Process() { } }"),
        ("src/C.cs", "public class C { void Run() { Process(); } }"),
    ]);
    let edges = calls_from(&g, &uid_symbol("src/C.cs", "C.Run"));
    assert_eq!(
        edges.len(),
        2,
        "fans out to both Process methods: {edges:?}"
    );
    for (_, prov, conf) in &edges {
        assert_eq!(*prov, Provenance::Ambiguous);
        assert!(*conf < 0.40);
    }
}

// ── Rule (MANDATORY): reflection is NEVER linked ─────────────────────────────

#[test]
fn reflection_invoke_is_unmatched_not_invented() {
    // `mi.Invoke(...)` is a reflective dispatch. There is no `Invoke` method
    // defined anywhere in this repo, and there is a `Run` method that the
    // reflection *would* target at runtime — but the heuristic must NOT connect the
    // reflective call to `Run` (the target is a runtime string). The `Invoke` call
    // resolves to NOTHING (no `Invoke` defined) → no edge, surfaced. And crucially
    // no edge to `Run` is invented from the reflection.
    let g = build(&[(
        "src/C.cs",
        concat!(
            "public class C {\n",
            "    public void Run() { }\n",
            "    public void Reflect(System.Type t) {\n",
            "        var mi = t.GetMethod(\"Run\");\n",
            "        mi.Invoke(this, null);\n",
            "    }\n",
            "}\n",
        ),
    )]);
    let edges = calls_from(&g, &uid_symbol("src/C.cs", "C.Reflect"));
    // No edge points at Run (the reflected target was never invented).
    assert!(
        !edges
            .iter()
            .any(|(d, _, _)| *d == uid_symbol("src/C.cs", "C.Run")),
        "reflection must not invent an edge to the reflected method Run: {edges:?}"
    );
    // GetMethod and Invoke have no repo definition → they resolve to nothing.
    // (There is no `GetMethod`/`Invoke` method in the repo, so unknown-receiver
    // fan-out finds zero candidates → no edge.)
    assert!(
        edges.is_empty(),
        "reflective calls resolve to nothing (no repo def) → no edges, got {edges:?}"
    );
}

// ── Using → IMPORTS edge (module-granular, best-effort) ──────────────────────

#[test]
fn using_links_to_repo_namespace_module() {
    // B.cs `using App.Models;` and A.cs declares a type in namespace `App.Models`
    // → a module-granular IMPORTS edge B→A. (Best-effort visibility; it seeds no
    // call binding.)
    let g = build(&[
        (
            "src/Models.cs",
            "namespace App.Models { public class User { } }",
        ),
        (
            "src/B.cs",
            "using App.Models;\nnamespace App { public class B { } }",
        ),
    ]);
    let imports = g.neighbors(
        &uid_module("src/B.cs"),
        Direction::Outgoing,
        &[EdgeKind::Imports],
    );
    assert!(
        imports
            .iter()
            .any(|(e, _)| e.dst == uid_module("src/Models.cs")),
        "B imports the module declaring namespace App.Models: {:?}",
        imports
            .iter()
            .map(|(e, _)| e.dst.as_str())
            .collect::<Vec<_>>()
    );
}

#[test]
fn using_to_external_namespace_invents_no_edge() {
    // `using System;` resolves to no repo module (System is external) → no IMPORTS
    // edge invented.
    let g = build(&[(
        "src/B.cs",
        "using System;\nnamespace App { public class B { } }",
    )]);
    let imports = g.neighbors(
        &uid_module("src/B.cs"),
        Direction::Outgoing,
        &[EdgeKind::Imports],
    );
    assert!(
        imports.is_empty(),
        "an external using invents no IMPORTS edge: {:?}",
        imports
            .iter()
            .map(|(e, _)| e.dst.as_str())
            .collect::<Vec<_>>()
    );
}

// ── The §4.1 band invariant holds over ALL C# edges, non-vacuously ───────────

#[test]
fn csharp_edges_satisfy_band_invariant_non_vacuously() {
    // A fixture exercising EVERY rule so the band check is non-vacuous: a same-file
    // Extracted call, a this-method Inferred call, a unique cross-file Inferred
    // call, and an ambiguous fan-out.
    let g = build(&[
        (
            "src/Only.cs",
            "public class Only { public void Uniq() { } }",
        ),
        ("src/Dup1.cs", "public class D1 { public void Dup() { } }"),
        ("src/Dup2.cs", "public class D2 { public void Dup() { } }"),
        (
            "src/C.cs",
            concat!(
                "public class C {\n",
                "    void Inner() { }\n",
                "    void Run() {\n",
                "        Inner();\n",      // same-file Extracted
                "        this.Inner();\n", // this-method Inferred
                "        Uniq();\n",       // unique cross-file Inferred
                "        Dup();\n",        // ambiguous fan-out (2 candidates)
                "    }\n",
                "}\n",
            ),
        ),
    ]);

    let mut seen_extracted = false;
    let mut seen_inferred = false;
    let mut seen_ambiguous = false;
    let mut violations: Vec<String> = Vec::new();
    let uids: Vec<Uid> = g.nodes().map(|n| n.uid.clone()).collect();
    for uid in &uids {
        for (edge, _) in g.neighbors(uid, Direction::Outgoing, &[]) {
            let conf = edge.confidence.value();
            let ok = match edge.provenance {
                Provenance::Extracted => {
                    seen_extracted = true;
                    (0.95..=1.0).contains(&conf)
                }
                Provenance::Resolved | Provenance::Observed => (0.90..=1.0).contains(&conf),
                Provenance::Inferred => {
                    seen_inferred = true;
                    (0.40..=0.80).contains(&conf)
                }
                Provenance::Ambiguous => {
                    seen_ambiguous = true;
                    conf < 0.40
                }
                Provenance::Model => true,
            };
            if !ok {
                violations.push(format!(
                    "{:?} edge {}->{} conf {:.4} (band violated)",
                    edge.provenance,
                    edge.src.as_str(),
                    edge.dst.as_str(),
                    conf
                ));
            }
        }
    }
    assert!(violations.is_empty(), "band violations: {violations:#?}");
    assert!(seen_extracted, "expected a same-file Extracted call edge");
    assert!(
        seen_inferred,
        "expected Inferred (this/cross-file) call edges"
    );
    assert!(seen_ambiguous, "expected an Ambiguous fan-out call edge");
}

// ── Determinism: building twice yields identical graphs ──────────────────────

#[test]
fn building_twice_yields_identical_graphs() {
    let files: &[(&str, &str)] = &[
        ("src/A.cs", "public class A { void F() { G(); } void G() { } }"),
        ("src/B.cs", "namespace N { public class B { void U() { Helper(); } } public class H { public void Helper() { } } }"),
    ];
    let g1 = build(files);
    let g2 = build(files);
    let edge_set = |g: &Graph| -> std::collections::BTreeSet<(String, String, String)> {
        let mut s = std::collections::BTreeSet::new();
        for n in g.nodes() {
            for (e, _) in g.neighbors(&n.uid, Direction::Outgoing, &[]) {
                s.insert((
                    e.src.as_str().to_string(),
                    e.dst.as_str().to_string(),
                    format!("{:?}", e.kind),
                ));
            }
        }
        s
    };
    assert_eq!(g1.node_count(), g2.node_count());
    assert_eq!(g1.edge_count(), g2.edge_count());
    assert_eq!(edge_set(&g1), edge_set(&g2), "identical edge sets");
}
