//! Rust linking tests — the band-disciplined call/use/structural graph.
//!
//! Each builds an in-memory `path → AnalyzedFile` set with [`analyze`], runs
//! [`assemble_rust`], and asserts on the resulting [`Graph`]. The confidence bands
//! (design §4.1) are LAW: a heuristic edge is never Extracted-1.0 and an ambiguous
//! one is always < 0.40. The deliberate-ambiguity fixture (two same-named methods,
//! an unknown-receiver call), the trait-dispatch fixture, and the
//! macro-is-not-a-call fixture are MANDATORY and pin the honest outcomes.

use std::collections::BTreeMap;

use strata_core::{AnalyzedFile, Direction, EdgeKind, Graph, NodeKind, Provenance, Uid};
use strata_lang_rust::{analyze, assemble_rust};

const REPO: &str = "rustrepo";

fn build(files: &[(&str, &str)]) -> Graph {
    let analyzed: BTreeMap<String, AnalyzedFile> = files
        .iter()
        .map(|(p, s)| (p.to_string(), analyze(p, s)))
        .collect();
    let mut g = Graph::new();
    assemble_rust(&mut g, REPO, &analyzed);
    g
}

fn uid_module(path: &str) -> Uid {
    Uid::new("rust", REPO, path, "<module>", "")
}
fn uid_symbol(path: &str, fqn: &str) -> Uid {
    Uid::new("rust", REPO, path, fqn, "")
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
        "src/w.rs",
        "mod app { pub trait Svc { fn go(&self); } pub struct Worker; impl Worker { pub fn run(&self) { } } }",
    )]);

    assert!(
        g.get_node(&uid_module("src/w.rs")).is_some(),
        "module node missing"
    );
    assert_eq!(
        g.get_node(&uid_symbol("src/w.rs", "app::Svc"))
            .map(|n| n.kind),
        Some(NodeKind::Interface)
    );
    assert_eq!(
        g.get_node(&uid_symbol("src/w.rs", "app::Worker"))
            .map(|n| n.kind),
        Some(NodeKind::Class)
    );
    assert_eq!(
        g.get_node(&uid_symbol("src/w.rs", "app::Worker::run"))
            .map(|n| n.kind),
        Some(NodeKind::Method)
    );

    // The method is MEMBER_OF its (module-qualified) type.
    let run_members = g.neighbors(
        &uid_symbol("src/w.rs", "app::Worker::run"),
        Direction::Outgoing,
        &[EdgeKind::MemberOf],
    );
    assert!(
        run_members
            .iter()
            .any(|(e, _)| e.dst == uid_symbol("src/w.rs", "app::Worker")),
        "run must be MEMBER_OF app::Worker"
    );
}

// ── Rule: same-file bare call → Extracted 0.95 ───────────────────────────────

#[test]
fn same_file_bare_call_is_extracted() {
    let g = build(&[("src/c.rs", "fn target() { } fn caller() { target(); }")]);
    let edges = calls_from(&g, &uid_symbol("src/c.rs", "caller"));
    assert!(
        has_edge(
            &edges,
            &uid_symbol("src/c.rs", "target"),
            Provenance::Extracted,
            0.95
        ),
        "caller —Calls→ target (Extracted 0.95): {edges:?}"
    );
}

// ── Rule: self.m() → own-type method, Inferred 0.80 ──────────────────────────

#[test]
fn self_method_call_resolves_to_own_type_method_inferred() {
    let g = build(&[(
        "src/c.rs",
        "struct C; impl C { fn helper(&self) { } fn run(&self) { self.helper(); } }",
    )]);
    let edges = calls_from(&g, &uid_symbol("src/c.rs", "C::run"));
    assert!(
        has_edge(
            &edges,
            &uid_symbol("src/c.rs", "C::helper"),
            Provenance::Inferred,
            0.80
        ),
        "C::run —Calls→ C::helper via self (Inferred 0.80): {edges:?}"
    );
}

#[test]
fn self_method_call_to_unknown_method_makes_no_edge() {
    // `self.ghost()` where the type has no `ghost`: a method might come from a trait
    // impl we cannot see — honest miss, never guessed.
    let g = build(&[(
        "src/c.rs",
        "struct C; impl C { fn run(&self) { self.ghost(); } }",
    )]);
    let edges = calls_from(&g, &uid_symbol("src/c.rs", "C::run"));
    assert!(
        edges.is_empty(),
        "self.ghost() resolves to nothing → no edge, got {edges:?}"
    );
}

// ── Rule: cross-module unique repo-wide name → Inferred 0.80 ─────────────────

#[test]
fn cross_module_unique_name_is_inferred() {
    // `caller` in b.rs calls `only()` with no same-file def. Exactly ONE `only`
    // exists repo-wide (in a.rs) → a unique cross-module name match at Inferred 0.80
    // (a single confident heuristic guess, never a fact; resolving the exact
    // use-path binding needs the type system, so there is no stronger binding).
    let g = build(&[
        ("src/a.rs", "pub fn only() { }"),
        ("src/b.rs", "fn caller() { only(); }"),
    ]);
    let edges = calls_from(&g, &uid_symbol("src/b.rs", "caller"));
    assert!(
        has_edge(
            &edges,
            &uid_symbol("src/a.rs", "only"),
            Provenance::Inferred,
            0.80
        ),
        "caller —Calls→ a::only (unique cross-module, Inferred 0.80): {edges:?}"
    );
}

// ── Rule (MANDATORY): deliberate ambiguity — two same-named methods ──────────

#[test]
fn deliberate_ambiguity_unknown_receiver_is_ambiguous_fanout() {
    // Two `save` methods on different types and a caller doing `acct.save()` with an
    // unknown receiver type. With several same-named candidates the heuristic CANNOT
    // know which → it fans out to BOTH at the AMBIGUOUS band (< 0.40), never a
    // confident single edge. THIS pins the honest ambiguous outcome (the Rust
    // trait-dispatch / unknown-type-receiver case).
    let g = build(&[
        (
            "src/u.rs",
            "struct User; impl User { pub fn save(&self) { } }",
        ),
        (
            "src/acc.rs",
            "struct Account; impl Account { pub fn save(&self) { } }",
        ),
        (
            "src/r.rs",
            "struct Runner; impl Runner { fn run(&self, acct: Account) { acct.save(); } }",
        ),
    ]);
    let edges = calls_from(&g, &uid_symbol("src/r.rs", "Runner::run"));

    assert!(
        has_edge(
            &edges,
            &uid_symbol("src/u.rs", "User::save"),
            Provenance::Ambiguous,
            0.35
        ),
        "run —Calls→ User::save (Ambiguous 0.35): {edges:?}"
    );
    assert!(
        has_edge(
            &edges,
            &uid_symbol("src/acc.rs", "Account::save"),
            Provenance::Ambiguous,
            0.35
        ),
        "run —Calls→ Account::save (Ambiguous 0.35): {edges:?}"
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
    // Two methods `process` on different types, a bare `process()` with no same-file
    // def → fan out to both at Ambiguous.
    let g = build(&[
        ("src/a.rs", "struct A; impl A { pub fn process(&self) { } }"),
        ("src/b.rs", "struct B; impl B { pub fn process(&self) { } }"),
        ("src/c.rs", "fn run() { process(); }"),
    ]);
    let edges = calls_from(&g, &uid_symbol("src/c.rs", "run"));
    assert_eq!(
        edges.len(),
        2,
        "fans out to both process methods: {edges:?}"
    );
    for (_, prov, conf) in &edges {
        assert_eq!(*prov, Provenance::Ambiguous);
        assert!(*conf < 0.40);
    }
}

// ── Rule (MANDATORY): macros are NEVER linked ────────────────────────────────

#[test]
fn macro_invocation_creates_no_call_edge() {
    // A `format!`-style macro and a user macro that *name* a fn `run` (which exists)
    // must NOT create a call edge to `run`: a macro invocation is not a call, and its
    // expansion is never guessed. Only the explicit `run()` call below links.
    let g = build(&[(
        "src/c.rs",
        concat!(
            "macro_rules! call_run { () => { run() }; }\n",
            "fn run() { }\n",
            "fn driver() {\n",
            "    println!(\"{}\", 1);\n",
            "    call_run!();\n",
            "    run();\n",
            "}\n",
        ),
    )]);
    let edges = calls_from(&g, &uid_symbol("src/c.rs", "driver"));
    // Exactly ONE edge: the explicit run() call. The macro contributes none.
    assert_eq!(
        edges.len(),
        1,
        "only the explicit run() call links; macros contribute no edge: {edges:?}"
    );
    assert!(
        has_edge(
            &edges,
            &uid_symbol("src/c.rs", "run"),
            Provenance::Extracted,
            0.95
        ),
        "the explicit run() call is the only edge (Extracted 0.95): {edges:?}"
    );
    // println! must never appear as a call target node either.
    assert!(
        g.get_node(&uid_symbol("src/c.rs", "println")).is_none(),
        "a macro is never a symbol node"
    );
}

// ── Rule: trait dispatch on an unknown receiver with NO repo candidate → none ─

#[test]
fn trait_dispatch_with_no_repo_candidate_makes_no_edge() {
    // `obj.absent()` on an unknown receiver, where NO method `absent` exists
    // repo-wide → no edge, surfaced (never an invented trait dispatch).
    let g = build(&[("src/c.rs", "fn run(obj: Thing) { obj.absent(); }")]);
    let edges = calls_from(&g, &uid_symbol("src/c.rs", "run"));
    assert!(
        edges.is_empty(),
        "trait dispatch with no repo candidate → no edge, got {edges:?}"
    );
}

// ── use → IMPORTS edge (module-granular, best-effort) ────────────────────────

#[test]
fn use_links_to_repo_module() {
    // b.rs `use crate::models::User;` and models.rs declares a type in module path
    // `models` (the indexer keys module path off the fqn prefix `models::User` →
    // `models`) → a module-granular IMPORTS edge b→models. (Best-effort visibility;
    // it seeds no call binding.)
    let g = build(&[
        ("src/models.rs", "mod models { pub struct User; }"),
        ("src/b.rs", "use crate::models::User;\npub struct B;"),
    ]);
    let imports = g.neighbors(
        &uid_module("src/b.rs"),
        Direction::Outgoing,
        &[EdgeKind::Imports],
    );
    assert!(
        imports
            .iter()
            .any(|(e, _)| e.dst == uid_module("src/models.rs")),
        "b imports the module declaring path models: {:?}",
        imports
            .iter()
            .map(|(e, _)| e.dst.as_str())
            .collect::<Vec<_>>()
    );
}

#[test]
fn use_to_external_crate_invents_no_edge() {
    // `use std::collections::HashMap;` resolves to no repo module (std is external)
    // → no IMPORTS edge invented.
    let g = build(&[("src/b.rs", "use std::collections::HashMap;\npub struct B;")]);
    let imports = g.neighbors(
        &uid_module("src/b.rs"),
        Direction::Outgoing,
        &[EdgeKind::Imports],
    );
    assert!(
        imports.is_empty(),
        "an external use invents no IMPORTS edge: {:?}",
        imports
            .iter()
            .map(|(e, _)| e.dst.as_str())
            .collect::<Vec<_>>()
    );
}

// ── Rule: type-qualified `Type::method()` resolves to the exact type's method ─

#[test]
fn type_qualified_call_resolves_to_exact_type_method() {
    // TWO types A, B each define a method `read`, plus a free fn `read`. A
    // type-qualified call `A::read()` carries a `::`-scoped path qualifier, so the
    // linker can take the qualifier's last segment (`A`) and resolve to EXACTLY
    // `A::read` — never `B::read`, never the free fn. The same call written as a
    // field receiver (`b.read()`) cannot name a type and stays an Ambiguous fan-out
    // (the unknown-receiver / trait-dispatch case), and a bare `read()` follows the
    // existing repo-wide name rule.
    let g = build(&[
        ("src/a.rs", "struct A; impl A { pub fn read(&self) { } }"),
        ("src/b.rs", "struct B; impl B { pub fn read(&self) { } }"),
        ("src/f.rs", "pub fn read() { }"),
        (
            "src/call.rs",
            concat!(
                "fn scoped() { A::read(); }\n",
                "fn field(b: B) { b.read(); }\n",
                "fn bare() { read(); }\n",
            ),
        ),
    ]);

    // `A::read()` → exactly ONE edge, to A::read, at the type-qualified band.
    // A's method and the call live in different files, so the band is Inferred 0.80
    // (CONF_TYPE_QUALIFIED), NOT Extracted (same-file) and NOT Ambiguous.
    let scoped = calls_from(&g, &uid_symbol("src/call.rs", "scoped"));
    assert_eq!(
        scoped.len(),
        1,
        "A::read() resolves to exactly one target (the unique type A's read): {scoped:?}"
    );
    assert!(
        has_edge(
            &scoped,
            &uid_symbol("src/a.rs", "A::read"),
            Provenance::Inferred,
            0.80
        ),
        "A::read() —Calls→ A::read (Inferred 0.80 type-qualified), not B::read/free fn: {scoped:?}"
    );

    // `b.read()` → Ambiguous fan-out (field receiver, unknown type) — UNCHANGED.
    // Fans out to BOTH methods A::read and B::read (the two `Method`s named read).
    let field = calls_from(&g, &uid_symbol("src/call.rs", "field"));
    assert!(
        field.len() >= 2
            && field
                .iter()
                .all(|(_, p, c)| *p == Provenance::Ambiguous && *c < 0.40),
        "b.read() stays an Ambiguous method fan-out (needs receiver-type inference): {field:?}"
    );

    // `bare read()` → the existing repo-wide name rule. Three callables named `read`
    // (A::read, B::read, free read) → Ambiguous fan-out (unchanged behavior).
    let bare = calls_from(&g, &uid_symbol("src/call.rs", "bare"));
    assert!(
        !bare.is_empty()
            && bare
                .iter()
                .all(|(_, p, c)| *p == Provenance::Ambiguous && *c < 0.40),
        "bare read() follows the existing repo-wide name rule (ambiguous here): {bare:?}"
    );
}

#[test]
fn type_qualified_constructor_resolves_to_exact_type() {
    // The dominant case: a constructor `A::new()` with TWO types each defining
    // `new`. Resolves to EXACTLY A::new (the type the qualifier names), never B::new.
    let g = build(&[
        ("src/a.rs", "struct A; impl A { pub fn new() -> A { A } }"),
        ("src/b.rs", "struct B; impl B { pub fn new() -> B { B } }"),
        ("src/call.rs", "fn mk() { A::new(); }"),
    ]);
    let edges = calls_from(&g, &uid_symbol("src/call.rs", "mk"));
    assert_eq!(
        edges.len(),
        1,
        "A::new() resolves to exactly one target (A's new), not B's: {edges:?}"
    );
    assert!(
        has_edge(
            &edges,
            &uid_symbol("src/a.rs", "A::new"),
            Provenance::Inferred,
            0.80
        ),
        "A::new() —Calls→ A::new (Inferred 0.80 type-qualified): {edges:?}"
    );
}

#[test]
fn type_qualified_same_file_target_is_extracted() {
    // When the unique type+method target lives in the SAME file as the call, the
    // type-qualified resolution earns the Extracted band (0.95) — the strongest
    // static signal — rather than the cross-file Inferred 0.80.
    let g = build(&[(
        "src/c.rs",
        concat!(
            "struct A; impl A { pub fn make() -> A { A } }\n",
            "fn factory() { A::make(); }\n",
        ),
    )]);
    let edges = calls_from(&g, &uid_symbol("src/c.rs", "factory"));
    assert_eq!(edges.len(), 1, "exactly one target: {edges:?}");
    assert!(
        has_edge(
            &edges,
            &uid_symbol("src/c.rs", "A::make"),
            Provenance::Extracted,
            0.95
        ),
        "A::make() —Calls→ A::make (Extracted 0.95, same file): {edges:?}"
    );
}

#[test]
fn scoped_self_call_resolves_to_enclosing_type_method() {
    // `Self::helper()` inside `impl A` is a `::`-scoped path whose qualifier is
    // `Self` — it resolves to a method `helper` on the ENCLOSING type A, exactly
    // like `self.helper()`, at the self-method band (Inferred 0.80).
    let g = build(&[(
        "src/a.rs",
        "struct A; impl A { fn helper() { } fn run() { Self::helper(); } }",
    )]);
    let edges = calls_from(&g, &uid_symbol("src/a.rs", "A::run"));
    assert!(
        has_edge(
            &edges,
            &uid_symbol("src/a.rs", "A::helper"),
            Provenance::Inferred,
            0.80
        ),
        "Self::helper() —Calls→ A::helper (Inferred 0.80, enclosing type): {edges:?}"
    );
}

#[test]
fn scoped_free_fn_call_resolves_via_fallback() {
    // A scoped free-fn call `mymod::func()` where `func` is a unique free fn
    // repo-wide and NO type named `mymod` has a method `func`. Today this hits the
    // methods-only `Some(_)` arm and resolves to nothing; with the fix it FALLS BACK
    // to the repo-wide name rule → a unique Inferred 0.80 edge to the free fn.
    let g = build(&[
        ("src/m.rs", "mod mymod { pub fn func() { } }"),
        ("src/call.rs", "fn caller() { mymod::func(); }"),
    ]);
    let edges = calls_from(&g, &uid_symbol("src/call.rs", "caller"));
    assert!(
        has_edge(
            &edges,
            &uid_symbol("src/m.rs", "mymod::func"),
            Provenance::Inferred,
            0.80
        ),
        "mymod::func() —Calls→ mymod::func (Inferred 0.80 via name fallback): {edges:?}"
    );
}

// ── Guard (confident-wrong): a type-qualified call to a KNOWN type that lacks the
// method must NOT confidently bind to an unrelated method of the same name ───────

#[test]
fn type_qualified_call_to_known_type_lacking_method_is_not_confident() {
    // `Foo::bar()` where the named type `Foo` exists but owns NO method `bar`, while
    // `bar` happens to be a UNIQUE method on an unrelated type `Qux`. The qualifier
    // `Foo::` never named `Qux`, so binding a confident Inferred 0.80 edge to
    // `Qux::bar` is a confident-WRONG edge — StrataGraph's never-confident-wrong promise
    // forbids it. The named type exists but has no such in-repo method (a trait
    // method we cannot see, or a stale/typo call), so the honest outcome is to fan
    // out same-named methods at the AMBIGUOUS band (< 0.40), never a confident pick.
    // (On develop this fanned out at Ambiguous 0.35; the type-qualified slice
    // regressed it to Inferred 0.80 — this guard pins it back to honest.)
    let g = build(&[
        (
            "src/foo.rs",
            "struct Foo; impl Foo { pub fn other(&self) { } }",
        ),
        (
            "src/qux.rs",
            "struct Qux; impl Qux { pub fn bar(&self) { } }",
        ),
        ("src/call.rs", "fn caller() { Foo::bar(); }"),
    ]);
    let edges = calls_from(&g, &uid_symbol("src/call.rs", "caller"));

    // The crux: NO confident (Inferred, ≥ 0.40) edge to Qux::bar — the type the
    // `Foo::` qualifier never named.
    assert!(
        !edges
            .iter()
            .any(|(d, p, c)| d == &uid_symbol("src/qux.rs", "Qux::bar")
                && *p == Provenance::Inferred
                && *c >= 0.40),
        "Foo::bar() must NOT confidently bind to the unrelated Qux::bar \
         (confident-wrong): {edges:?}"
    );
    // More broadly: every edge from this call site (if any) is honestly Ambiguous
    // (< 0.40) — never a confident pick of a method the qualifier did not name.
    for (_, prov, conf) in &edges {
        assert_eq!(
            *prov,
            Provenance::Ambiguous,
            "Foo::bar() to a known type lacking the method stays Ambiguous: {edges:?}"
        );
        assert!(
            *conf < 0.40,
            "edge conf {conf} must be < 0.40 (honest, not confident): {edges:?}"
        );
    }
}

#[test]
fn external_or_unknown_type_qualifier_free_fn_still_resolves() {
    // The legit fallback the fix must preserve: `mymod::func()` where `mymod` is NOT
    // a type anywhere (it is a module path) and `func` is a unique free function
    // repo-wide → still resolves to a confident Inferred 0.80 edge. The
    // known-type-lacks-method guard above must not collateral-damage this genuine
    // free-fn case (its qualifier names no type, so the `callables_named` fallback
    // still fires).
    let g = build(&[
        ("src/m.rs", "mod mymod { pub fn func() { } }"),
        ("src/call.rs", "fn caller() { mymod::func(); }"),
    ]);
    let edges = calls_from(&g, &uid_symbol("src/call.rs", "caller"));
    assert!(
        has_edge(
            &edges,
            &uid_symbol("src/m.rs", "mymod::func"),
            Provenance::Inferred,
            0.80
        ),
        "mymod::func() (mymod is not a type, func unique) still resolves \
         Inferred 0.80: {edges:?}"
    );
}

// ── The §4.1 band invariant holds over ALL Rust edges, non-vacuously ─────────

#[test]
fn rust_edges_satisfy_band_invariant_non_vacuously() {
    // A fixture exercising EVERY rule so the band check is non-vacuous: a same-file
    // Extracted call, a self-method Inferred call, a unique cross-module Inferred
    // call, and an ambiguous fan-out.
    let g = build(&[
        (
            "src/only.rs",
            "struct Only; impl Only { pub fn uniq(&self) { } }",
        ),
        (
            "src/dup1.rs",
            "struct D1; impl D1 { pub fn dup(&self) { } }",
        ),
        (
            "src/dup2.rs",
            "struct D2; impl D2 { pub fn dup(&self) { } }",
        ),
        (
            "src/c.rs",
            concat!(
                "struct C;\n",
                "impl C {\n",
                "    fn inner(&self) { }\n",
                "    fn run(&self) {\n",
                "        inner();\n", // same-file Extracted (bare name match)
                "        self.inner();\n", // self-method Inferred
                "        uniq();\n",  // unique cross-module Inferred
                "        dup();\n",   // ambiguous fan-out (2 candidates)
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
        "expected Inferred (self/cross-module) call edges"
    );
    assert!(seen_ambiguous, "expected an Ambiguous fan-out call edge");
}

// ── Determinism: building twice yields identical graphs ──────────────────────

#[test]
fn building_twice_yields_identical_graphs() {
    let files: &[(&str, &str)] = &[
        ("src/a.rs", "fn f() { g(); } fn g() { }"),
        (
            "src/b.rs",
            "mod n { struct B; impl B { fn u(&self) { helper(); } } pub fn helper() { } }",
        ),
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
