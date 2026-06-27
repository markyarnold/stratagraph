//! Python linking tests — the band-disciplined call/import/structural graph.
//!
//! Each builds an in-memory `path → AnalyzedFile` set with [`analyze`], runs
//! [`assemble_python`], and asserts on the resulting [`Graph`]. The confidence
//! bands (design §4.1) are LAW: a heuristic edge is never Extracted-1.0 and an
//! ambiguous one is always < 0.40. The deliberate-ambiguity fixture (two
//! same-named functions, a bare call) is mandatory and pins the ambiguous
//! outcome.

use std::collections::BTreeMap;

use strata_core::{AnalyzedFile, Direction, EdgeKind, Graph, NodeKind, Provenance, Uid};
use strata_lang_py::{analyze, assemble_python};

const REPO: &str = "pyrepo";

fn build(files: &[(&str, &str)]) -> Graph {
    let analyzed: BTreeMap<String, AnalyzedFile> = files
        .iter()
        .map(|(p, s)| (p.to_string(), analyze(p, s)))
        .collect();
    let mut g = Graph::new();
    assemble_python(&mut g, REPO, &analyzed);
    g
}

fn uid_module(path: &str) -> Uid {
    Uid::new("py", REPO, path, "<module>", "")
}
fn uid_symbol(path: &str, fqn: &str) -> Uid {
    Uid::new("py", REPO, path, fqn, "")
}

/// The `(dst, provenance, confidence)` of every outgoing `Calls` edge from `src`.
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

// ── Structural: nodes + Module/Repo/Class/Method wiring ──────────────────────

#[test]
fn builds_module_symbol_nodes_and_structural_edges() {
    let g = build(&[(
        "pkg/dog.py",
        "class Dog:\n    def speak(self):\n        pass\n\ndef free():\n    pass\n",
    )]);

    // Module node exists (py-tagged).
    assert!(
        g.get_node(&uid_module("pkg/dog.py")).is_some(),
        "module node missing"
    );
    // Class, Method, Function nodes exist with the right kinds.
    assert_eq!(
        g.get_node(&uid_symbol("pkg/dog.py", "Dog")).map(|n| n.kind),
        Some(NodeKind::Class)
    );
    assert_eq!(
        g.get_node(&uid_symbol("pkg/dog.py", "Dog.speak"))
            .map(|n| n.kind),
        Some(NodeKind::Method)
    );
    assert_eq!(
        g.get_node(&uid_symbol("pkg/dog.py", "free"))
            .map(|n| n.kind),
        Some(NodeKind::Function)
    );

    // The method is a MEMBER_OF its class; the free function is a MEMBER_OF the
    // module. (DEFINES is the reciprocal — checked via incoming.)
    let speak_members = g.neighbors(
        &uid_symbol("pkg/dog.py", "Dog.speak"),
        Direction::Outgoing,
        &[EdgeKind::MemberOf],
    );
    assert!(
        speak_members
            .iter()
            .any(|(e, _)| e.dst == uid_symbol("pkg/dog.py", "Dog")),
        "speak must be MEMBER_OF Dog"
    );
}

// ── Rule: same-module bare call → Extracted 0.95 ─────────────────────────────

#[test]
fn same_module_bare_call_is_extracted() {
    // `caller` calls `target`, both defined in the same file. A same-file bare
    // call binds to the local def deterministically → Extracted 0.95 (the
    // EXTRACTED band floor; NOT 1.0 — never outranks a RESOLVED 0.97 fact).
    let g = build(&[(
        "a.py",
        "def target():\n    pass\n\ndef caller():\n    target()\n",
    )]);
    let edges = calls_from(&g, &uid_symbol("a.py", "caller"));
    assert!(
        has_edge(
            &edges,
            &uid_symbol("a.py", "target"),
            Provenance::Extracted,
            0.95
        ),
        "caller —Calls→ target (Extracted 0.95): {edges:?}"
    );
}

// ── Rule: self.m() → own-class method, Inferred 0.80 ─────────────────────────

#[test]
fn self_method_call_resolves_to_own_class_method_inferred() {
    let g = build(&[(
        "c.py",
        "class C:\n    def helper(self):\n        pass\n    def run(self):\n        self.helper()\n",
    )]);
    let edges = calls_from(&g, &uid_symbol("c.py", "C.run"));
    assert!(
        has_edge(
            &edges,
            &uid_symbol("c.py", "C.helper"),
            Provenance::Inferred,
            0.80
        ),
        "C.run —Calls→ C.helper via self (Inferred 0.80): {edges:?}"
    );
}

#[test]
fn self_method_call_to_unknown_method_makes_no_edge() {
    // `self.ghost()` where the class has no `ghost` method: nothing to point at.
    // We do not invent an edge (a method might come from a base class we cannot
    // see — honest miss, never guessed).
    let g = build(&[(
        "c.py",
        "class C:\n    def run(self):\n        self.ghost()\n",
    )]);
    let edges = calls_from(&g, &uid_symbol("c.py", "C.run"));
    assert!(
        edges.is_empty(),
        "self.ghost() resolves to nothing → no edge, got {edges:?}"
    );
}

// ── Rule: cross-module via relative import match → Inferred 0.80 ─────────────

#[test]
fn imported_name_call_resolves_cross_module_inferred() {
    // `pkg/b.py` does `from .a import helper`; a bare `helper()` then resolves to
    // `pkg/a.py`'s `helper` via the import binding → Inferred 0.80.
    let g = build(&[
        ("pkg/a.py", "def helper():\n    pass\n"),
        (
            "pkg/b.py",
            "from .a import helper\n\ndef use():\n    helper()\n",
        ),
    ]);
    let edges = calls_from(&g, &uid_symbol("pkg/b.py", "use"));
    assert!(
        has_edge(
            &edges,
            &uid_symbol("pkg/a.py", "helper"),
            Provenance::Inferred,
            0.80
        ),
        "use —Calls→ a.helper via relative import (Inferred 0.80): {edges:?}"
    );
    // An IMPORTS edge from b's module to a's module also exists.
    let imports = g.neighbors(
        &uid_module("pkg/b.py"),
        Direction::Outgoing,
        &[EdgeKind::Imports],
    );
    assert!(
        imports.iter().any(|(e, _)| e.dst == uid_module("pkg/a.py")),
        "b imports a (module-granular IMPORTS edge)"
    );
}

// ── Rule: bare-name unique repo-wide → Inferred 0.80 ─────────────────────────

#[test]
fn bare_call_unique_repo_wide_is_inferred() {
    // `caller` in `b.py` calls `only()` with NO import and no local def. There is
    // exactly ONE `only` function across the repo (in `a.py`) → a unique bare-name
    // match at Inferred 0.80 (a single confident heuristic guess, never a fact).
    let g = build(&[
        ("a.py", "def only():\n    pass\n"),
        ("b.py", "def caller():\n    only()\n"),
    ]);
    let edges = calls_from(&g, &uid_symbol("b.py", "caller"));
    assert!(
        has_edge(
            &edges,
            &uid_symbol("a.py", "only"),
            Provenance::Inferred,
            0.80
        ),
        "caller —Calls→ a.only (unique bare name, Inferred 0.80): {edges:?}"
    );
}

// ── Rule (MANDATORY): deliberate ambiguity — two same-named funcs, bare call ──

#[test]
fn deliberate_ambiguity_bare_call_is_ambiguous_fanout() {
    // Two `process` functions in different modules and a caller that does a bare
    // `process()` with NO import to disambiguate. With several same-named
    // candidates the heuristic CANNOT know which → it fans out to BOTH at the
    // AMBIGUOUS band (< 0.40), never a confident single edge. THIS pins the
    // honest ambiguous outcome.
    let g = build(&[
        ("a.py", "def process():\n    pass\n"),
        ("b.py", "def process():\n    pass\n"),
        ("c.py", "def run():\n    process()\n"),
    ]);
    let edges = calls_from(&g, &uid_symbol("c.py", "run"));

    // Both candidates are reached, each Ambiguous and strictly below 0.40.
    assert!(
        has_edge(
            &edges,
            &uid_symbol("a.py", "process"),
            Provenance::Ambiguous,
            0.35
        ),
        "run —Calls→ a.process (Ambiguous 0.35): {edges:?}"
    );
    assert!(
        has_edge(
            &edges,
            &uid_symbol("b.py", "process"),
            Provenance::Ambiguous,
            0.35
        ),
        "run —Calls→ b.process (Ambiguous 0.35): {edges:?}"
    );
    assert_eq!(
        edges.len(),
        2,
        "exactly the two ambiguous candidates: {edges:?}"
    );
    // And every edge is in the Ambiguous band — non-vacuous proof.
    for (_, prov, conf) in &edges {
        assert_eq!(*prov, Provenance::Ambiguous);
        assert!(*conf < 0.40, "ambiguous edge conf {conf} must be < 0.40");
    }
}

// ── Rule: unknown receiver → Ambiguous (or nothing) ──────────────────────────

#[test]
fn unknown_receiver_method_call_is_ambiguous() {
    // `obj.save()` with an unknown receiver type: like TS's UnknownReceiver, the
    // heuristic can only fan out to same-named methods repo-wide at the Ambiguous
    // band. Two `save` methods exist → both reached, Ambiguous.
    let g = build(&[
        ("a.py", "class A:\n    def save(self):\n        pass\n"),
        ("b.py", "class B:\n    def save(self):\n        pass\n"),
        ("c.py", "def run(obj):\n    obj.save()\n"),
    ]);
    let edges = calls_from(&g, &uid_symbol("c.py", "run"));
    assert_eq!(edges.len(), 2, "fans out to both save methods: {edges:?}");
    for (_, prov, conf) in &edges {
        assert_eq!(*prov, Provenance::Ambiguous, "unknown receiver → Ambiguous");
        assert!(*conf < 0.40);
    }
}

// ── Rule: dynamic dispatch & star imports are NEVER linked ───────────────────

#[test]
fn star_import_does_not_seed_a_confident_call() {
    // `from a import *` then `helper()` — a star import binds names dynamically.
    // We must NOT treat `helper` as import-matched (that would be an invented
    // confident link). It falls through to the bare-name repo-wide rule: here
    // `helper` is unique repo-wide, so it is at most Inferred — and crucially is
    // NOT promoted to a star-import "fact".
    let g = build(&[
        ("a.py", "def helper():\n    pass\n"),
        ("b.py", "from a import *\n\ndef use():\n    helper()\n"),
    ]);
    let edges = calls_from(&g, &uid_symbol("b.py", "use"));
    // Whatever edge exists, it is never Extracted/Resolved (never a star-import
    // fact). Unique repo-wide ⇒ Inferred is acceptable.
    for (_, prov, conf) in &edges {
        assert!(
            matches!(prov, Provenance::Inferred | Provenance::Ambiguous),
            "a star-import-bound call must never be a fact, got {prov:?} {conf}"
        );
    }
}

// ── The §4.1 band invariant holds over ALL Python edges, non-vacuously ───────

#[test]
fn python_edges_satisfy_band_invariant_non_vacuously() {
    // A fixture exercising EVERY rule so the band check is non-vacuous: a
    // same-module Extracted call, a self-method Inferred call, an import-matched
    // Inferred call, and an ambiguous fan-out.
    let g = build(&[
        (
            "pkg/a.py",
            "def helper():\n    pass\n\ndef dup():\n    pass\n",
        ),
        ("pkg/b.py", "def dup():\n    pass\n"),
        (
            "pkg/c.py",
            concat!(
                "from .a import helper\n",
                "\n",
                "class C:\n",
                "    def inner(self):\n",
                "        pass\n",
                "    def run(self):\n",
                "        self.inner()\n", // self-method Inferred
                "        helper()\n",     // import-matched Inferred
                "        local()\n",      // same-module Extracted
                "        dup()\n",        // ambiguous fan-out (2 candidates)
                "\n",
                "def local():\n",
                "    pass\n",
            ),
        ),
    ]);

    // Confirm the graph contains edges of every band so the invariant is not
    // vacuously satisfied.
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
    assert!(seen_extracted, "expected a same-module Extracted call edge");
    assert!(seen_inferred, "expected Inferred (self/import) call edges");
    assert!(seen_ambiguous, "expected an Ambiguous fan-out call edge");
}

// ── Determinism: building twice yields identical graphs ──────────────────────

#[test]
fn building_twice_yields_identical_graphs() {
    let files: &[(&str, &str)] = &[
        ("a.py", "def f():\n    g()\n\ndef g():\n    pass\n"),
        ("b.py", "from .a import f\n\ndef use():\n    f()\n"),
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
