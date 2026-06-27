//! Rust extraction tests. Each runs `analyze` on an inline source string and
//! asserts on the resulting `AnalyzedFile`. TDD red-first per construct
//! (fns, structs/enums/unions, traits + method sigs, impls, mods, uses, calls).
//!
//! The discipline mirrors `strata-lang-cs/tests/extraction.rs`: a missed symbol
//! is acceptable degradation; an *invented* one is not. The fqn convention
//! (module-path-qualified, type-nested with `::`) is pinned here, and the
//! load-bearing macros-are-NOT-calls invariant is mandatory.

use strata_core::{CallRef, ImportRef, NodeKind, RawSymbol};
use strata_lang_rust::analyze;

fn symbols_of(file: &strata_core::AnalyzedFile, kind: NodeKind) -> Vec<&RawSymbol> {
    file.symbols.iter().filter(|s| s.kind == kind).collect()
}

fn names(symbols: &[&RawSymbol]) -> Vec<String> {
    symbols.iter().map(|s| s.name.clone()).collect()
}

fn find_call<'a>(file: &'a strata_core::AnalyzedFile, callee: &str) -> &'a CallRef {
    file.calls
        .iter()
        .find(|c| c.callee_name == callee)
        .unwrap_or_else(|| panic!("missing call to {callee}: {:?}", file.calls))
}

fn sym<'a>(file: &'a strata_core::AnalyzedFile, name: &str) -> &'a RawSymbol {
    file.symbols
        .iter()
        .find(|s| s.name == name)
        .unwrap_or_else(|| panic!("missing symbol {name}: {:?}", file.symbols))
}

// --- #1: free fns (incl. async/pub/generic) ----------------------------------

#[test]
fn extracts_free_functions_async_pub_generic() {
    let src = concat!(
        "pub fn plain() {}\n",
        "pub async fn fetch() {}\n",
        "fn generic<T>(x: T) -> T { x }\n",
    );
    let file = analyze("src/lib.rs", src);
    let funcs = symbols_of(&file, NodeKind::Function);
    let mut got = names(&funcs);
    got.sort();
    assert_eq!(
        got,
        vec![
            "fetch".to_string(),
            "generic".to_string(),
            "plain".to_string()
        ],
        "async/pub/generic free fns all extract as Function: {:?}",
        file.symbols
    );
    // A free fn at crate root has no container, and its fqn is its bare name (no
    // generics in the fqn).
    for f in &funcs {
        assert!(f.container_fqn.is_none(), "{} is a free fn", f.name);
    }
    assert_eq!(
        sym(&file, "generic").fqn,
        "generic",
        "generics stripped from fqn"
    );
}

// --- #2: struct/enum/union extract as Class-kind (documented) ----------------

#[test]
fn structs_enums_unions_are_class_kind() {
    let src = concat!(
        "pub struct MyStruct { field: u32 }\n",
        "pub enum MyEnum { A, B(u32) }\n",
        "pub union MyUnion { a: u32, b: f32 }\n",
    );
    let file = analyze("src/types.rs", src);
    let classes = symbols_of(&file, NodeKind::Class);
    let mut got = names(&classes);
    got.sort();
    // All three are Class-kind (struct/enum/union collapse to Class).
    assert_eq!(
        got,
        vec![
            "MyEnum".to_string(),
            "MyStruct".to_string(),
            "MyUnion".to_string()
        ],
        "struct + enum + union all extract as Class: {:?}",
        file.symbols
    );
    // None are Interface (that kind is reserved for traits).
    assert!(symbols_of(&file, NodeKind::Interface).is_empty());
    for c in &classes {
        assert!(c.container_fqn.is_none(), "{} is a top-level type", c.name);
    }
}

// --- #3: trait → Interface; its method sigs are Methods of the trait ---------

#[test]
fn trait_is_interface_with_method_sigs_as_methods() {
    let src = concat!(
        "pub trait MyTrait {\n",
        "    fn required(&self) -> u32;\n",
        "    fn defaulted(&self) { }\n",
        "}\n",
    );
    let file = analyze("src/t.rs", src);

    // The trait is its own Interface kind.
    let interfaces = symbols_of(&file, NodeKind::Interface);
    assert_eq!(names(&interfaces), vec!["MyTrait".to_string()]);
    assert!(
        interfaces[0].container_fqn.is_none(),
        "a trait is top-level"
    );

    // Both the required signature and the defaulted method are Methods of the trait.
    let required = sym(&file, "required");
    assert_eq!(required.kind, NodeKind::Method);
    assert_eq!(
        required.container_fqn.as_deref(),
        Some("MyTrait"),
        "a trait method signature is a member of its trait"
    );
    assert_eq!(required.fqn, "MyTrait::required");
    let defaulted = sym(&file, "defaulted");
    assert_eq!(defaulted.kind, NodeKind::Method);
    assert_eq!(defaulted.container_fqn.as_deref(), Some("MyTrait"));
}

// --- #4: impl Type → methods of Type (assoc fns + self-methods) --------------

#[test]
fn impl_block_methods_attribute_to_the_self_type() {
    let src = concat!(
        "pub struct Worker;\n",
        "impl Worker {\n",
        "    pub fn new() -> Self { Worker }\n", // associated fn (no self)
        "    pub fn run(&self) { }\n",           // method (&self)
        "    pub fn consume(self) { }\n",        // method (self)
        "    pub fn modify(&mut self) { }\n",    // method (&mut self)
        "}\n",
    );
    let file = analyze("src/w.rs", src);
    let methods = symbols_of(&file, NodeKind::Method);
    let mut got = names(&methods);
    got.sort();
    // assoc fn `new` AND the self-methods are ALL Methods of Worker (no
    // special-casing: an associated fn without self is still a Method).
    assert_eq!(
        got,
        vec![
            "consume".to_string(),
            "modify".to_string(),
            "new".to_string(),
            "run".to_string()
        ],
        "assoc fns + self-methods all extract as Method of Worker: {:?}",
        file.symbols
    );
    for m in &methods {
        assert_eq!(
            m.container_fqn.as_deref(),
            Some("Worker"),
            "{} container",
            m.name
        );
        assert_eq!(m.fqn, format!("Worker::{}", m.name), "{} fqn", m.name);
    }
}

// --- #5: impl Trait for Type → methods attributed to Type --------------------

#[test]
fn impl_trait_for_type_attributes_methods_to_the_type() {
    let src = concat!(
        "pub struct Worker;\n",
        "pub trait Runnable { fn go(&self); }\n",
        "impl Runnable for Worker {\n",
        "    fn go(&self) { }\n",
        "}\n",
    );
    let file = analyze("src/w.rs", src);
    // The trait method's `go` impl is a Method of Worker (the concrete self type),
    // NOT of the trait — the trait sig `go` is the trait's member; the impl's `go`
    // is Worker's.
    let worker_go = file
        .symbols
        .iter()
        .find(|s| s.name == "go" && s.container_fqn.as_deref() == Some("Worker"))
        .unwrap_or_else(|| panic!("impl Runnable for Worker::go missing: {:?}", file.symbols));
    assert_eq!(worker_go.kind, NodeKind::Method);
    assert_eq!(worker_go.fqn, "Worker::go");
    // And the trait's own `go` signature is a Method of Runnable.
    let trait_go = file
        .symbols
        .iter()
        .find(|s| s.name == "go" && s.container_fqn.as_deref() == Some("Runnable"))
        .unwrap_or_else(|| panic!("trait Runnable::go sig missing: {:?}", file.symbols));
    assert_eq!(trait_go.fqn, "Runnable::go");
}

// --- #6: generic impl strips generics from the self type ---------------------

#[test]
fn generic_impl_strips_generics_from_self_type() {
    // `impl<T> Container<T>` → methods attribute to `Container` (generics stripped),
    // so a call to `get` resolves by the plain type name.
    let src = concat!(
        "pub struct Container<T> { inner: T }\n",
        "impl<T: Clone> Container<T> {\n",
        "    pub fn get(&self) -> T { self.inner.clone() }\n",
        "}\n",
    );
    let file = analyze("src/c.rs", src);
    let get = sym(&file, "get");
    assert_eq!(
        get.container_fqn.as_deref(),
        Some("Container"),
        "generics are stripped from the impl self type"
    );
    assert_eq!(get.fqn, "Container::get");
    // The struct itself is `Container` (generics stripped there too).
    assert_eq!(sym(&file, "Container").fqn, "Container");
}

// --- #7: module nesting qualifies fqns (THE convention) ----------------------

#[test]
fn module_nesting_qualifies_fqns() {
    let src = concat!(
        "pub mod outer {\n",
        "    pub mod inner {\n",
        "        pub struct Thing;\n",
        "        impl Thing { pub fn method(&self) { } }\n",
        "        pub fn free() { }\n",
        "    }\n",
        "}\n",
    );
    let file = analyze("src/lib.rs", src);
    assert_eq!(
        sym(&file, "Thing").fqn,
        "outer::inner::Thing",
        "nested modules compose into a ::-path prefix on the type fqn"
    );
    assert_eq!(
        sym(&file, "method").fqn,
        "outer::inner::Thing::method",
        "the member fqn is module::Type::member"
    );
    assert_eq!(
        sym(&file, "method").container_fqn.as_deref(),
        Some("outer::inner::Thing"),
        "the method's container is the module-qualified type fqn"
    );
    assert_eq!(
        sym(&file, "free").fqn,
        "outer::inner::free",
        "a free fn in a nested module is module::fn"
    );
}

#[test]
fn file_module_declaration_contributes_no_symbol() {
    // `mod foo;` declares a sibling file (indexed separately) — it has no body, so
    // it must NOT invent a `foo` symbol.
    let src = "mod foo;\npub fn here() {}\n";
    let file = analyze("src/lib.rs", src);
    assert!(
        !file.symbols.iter().any(|s| s.name == "foo"),
        "a file-module declaration is not a symbol: {:?}",
        file.symbols
    );
    assert!(file.symbols.iter().any(|s| s.name == "here"));
}

// --- #8: use paths (plain, aliased, groups, globs, self/super/crate) ----------

fn using_with_specifier<'a>(file: &'a strata_core::AnalyzedFile, spec: &str) -> Vec<&'a ImportRef> {
    file.imports
        .iter()
        .filter(|i| i.specifier == spec)
        .collect()
}

#[test]
fn extracts_plain_aliased_grouped_glob_and_prefixed_uses() {
    let src = concat!(
        "use std::collections::HashMap;\n",
        "use a::b::{c, d as e};\n",
        "use std::io::*;\n",
        "use super::thing;\n",
        "use crate::other::Foo;\n",
        "use serde::Serialize as Ser;\n",
    );
    let file = analyze("src/lib.rs", src);

    // Plain qualified path → specifier is the full path, bound name the last segment.
    let hm = using_with_specifier(&file, "std::collections::HashMap");
    assert_eq!(hm.len(), 1, "one plain use, got {:?}", file.imports);
    assert_eq!(hm[0].imported_names, vec!["HashMap".to_string()]);

    // Group `{c, d as e}` → one import per item, prefixed by `a::b`.
    let c = using_with_specifier(&file, "a::b::c");
    assert_eq!(c.len(), 1, "group member c preserved");
    assert_eq!(c[0].imported_names, vec!["c".to_string()]);
    let d = using_with_specifier(&file, "a::b::d");
    assert_eq!(d.len(), 1, "group member d preserved with its prefix");
    assert_eq!(
        d[0].imported_names,
        vec!["e".to_string()],
        "the alias is the bound name"
    );

    // Glob `std::io::*` → specifier is the base path, bound name "*".
    let glob = using_with_specifier(&file, "std::io");
    assert_eq!(glob.len(), 1, "glob base path recorded");
    assert_eq!(
        glob[0].imported_names,
        vec!["*".to_string()],
        "a glob binds no specific name (recorded as *)"
    );

    // `super::thing` and `crate::other::Foo` keep their prefixes verbatim.
    assert_eq!(using_with_specifier(&file, "super::thing").len(), 1);
    let foo = using_with_specifier(&file, "crate::other::Foo");
    assert_eq!(foo.len(), 1);
    assert_eq!(foo[0].imported_names, vec!["Foo".to_string()]);

    // Top-level alias `serde::Serialize as Ser`.
    let ser = using_with_specifier(&file, "serde::Serialize");
    assert_eq!(ser.len(), 1, "top-level alias use recorded");
    assert_eq!(ser[0].imported_names, vec!["Ser".to_string()]);
}

// --- #9: calls with receivers + enclosing fqn --------------------------------

#[test]
fn extracts_calls_with_receiver_and_enclosing_fqn() {
    let src = concat!(
        "mod m {\n",
        "    pub struct C;\n",
        "    impl C {\n",
        "        pub fn method(&self) {\n",
        "            helper();\n",       // bare
        "            self.other();\n",   // self-receiver
        "            svc.do_it();\n",    // member-receiver
        "            a::b::c();\n",      // scoped path call
        "            C::assoc();\n",     // Type::assoc
        "            build().trim();\n", // chained: receiver is itself a call
        "        }\n",
        "    }\n",
        "}\n",
    );
    let file = analyze("src/lib.rs", src);

    let helper = find_call(&file, "helper");
    assert_eq!(helper.receiver, None);
    assert!(
        !helper.receiver_is_path,
        "a bare call has no receiver → receiver_is_path is false"
    );
    assert_eq!(
        helper.enclosing_fqn, "m::C::method",
        "enclosing is the module-qualified method fqn"
    );

    let other = find_call(&file, "other");
    assert_eq!(other.receiver.as_deref(), Some("self"));
    assert!(
        !other.receiver_is_path,
        "self.other() is a `.` field receiver → receiver_is_path false"
    );
    assert_eq!(other.enclosing_fqn, "m::C::method");

    let do_it = find_call(&file, "do_it");
    assert_eq!(do_it.receiver.as_deref(), Some("svc"));
    assert!(
        !do_it.receiver_is_path,
        "svc.do_it() is a `.` field receiver → receiver_is_path false"
    );

    // `a::b::c()` → callee `c`, receiver the path qualifier `a::b`.
    let c = find_call(&file, "c");
    assert_eq!(c.callee_name, "c");
    assert_eq!(
        c.receiver.as_deref(),
        Some("a::b"),
        "scoped-path call records the path qualifier as receiver"
    );
    assert!(
        c.receiver_is_path,
        "a::b::c() is a `::`-scoped path qualifier → receiver_is_path true"
    );

    // `C::assoc()` → callee `assoc`, receiver `C`.
    let assoc = find_call(&file, "assoc");
    assert_eq!(assoc.receiver.as_deref(), Some("C"), "Type::assoc receiver");
    assert!(
        assoc.receiver_is_path,
        "C::assoc() is a `::`-scoped type qualifier → receiver_is_path true"
    );

    // Chained `build().trim()` → callee `trim`, receiver the immediate value text
    // `build()` (the honest receiver chain, even though it is itself a call).
    let trim = find_call(&file, "trim");
    assert_eq!(trim.callee_name, "trim");
    assert_eq!(
        trim.receiver.as_deref(),
        Some("build()"),
        "the receiver is the full immediate-value text"
    );
    assert!(
        !trim.receiver_is_path,
        "build().trim() is a `.` field receiver on an expression → receiver_is_path false"
    );
    // The inner `build()` is also recorded as a bare call.
    let build = find_call(&file, "build");
    assert_eq!(build.receiver, None);
    assert!(!build.receiver_is_path);
}

// --- #10 (MANDATORY): macros are NOT calls -----------------------------------

#[test]
fn macro_invocation_is_not_a_call() {
    // `println!`, `vec!`, `assert!`, and a user `my_macro!()` are macro invocations,
    // NOT function calls. NONE may produce a CallRef, and the macro's expansion is
    // never guessed. This is the load-bearing honesty pin for the Rust plane.
    let src = concat!(
        "macro_rules! my_macro { () => {}; }\n",
        "fn run() {\n",
        "    println!(\"hi {}\", 1);\n",
        "    vec![1, 2, 3];\n",
        "    assert!(true);\n",
        "    my_macro!();\n",
        "    real_call();\n",
        "}\n",
        "fn real_call() {}\n",
    );
    let file = analyze("src/lib.rs", src);

    for faked in ["println", "vec", "assert", "my_macro"] {
        assert!(
            !file.calls.iter().any(|c| c.callee_name == faked),
            "macro `{faked}!` must NOT be recorded as a call: {:?}",
            file.calls
        );
    }
    // The ONE real function call IS recorded.
    assert!(
        file.calls.iter().any(|c| c.callee_name == "real_call"),
        "a real fn call is still recorded: {:?}",
        file.calls
    );
    assert_eq!(
        file.calls.len(),
        1,
        "exactly one call (real_call); all macros excluded: {:?}",
        file.calls
    );
    // `macro_rules!` defs are not symbols this slice.
    assert!(
        !file.symbols.iter().any(|s| s.name == "my_macro"),
        "macro_rules! is not a symbol: {:?}",
        file.symbols
    );
}

#[test]
fn computed_callee_yields_no_static_call() {
    // `factory()()` — the outer call's function is itself a call, a computed/dynamic
    // target. The outer call must NOT invent a callee; the inner `factory()` IS a
    // real bare call.
    let src = "fn m() { factory()(); }";
    let file = analyze("src/lib.rs", src);
    assert!(
        file.calls
            .iter()
            .any(|c| c.callee_name == "factory" && c.receiver.is_none()),
        "the inner factory() call is recorded: {:?}",
        file.calls
    );
    assert_eq!(
        file.calls.len(),
        1,
        "only the inner call is recorded, got {:?}",
        file.calls
    );
}

// --- #11: contract-plane vecs stay empty this slice (the honesty story) -------

#[test]
fn contract_plane_vecs_are_empty_for_rust() {
    // Even an axum-looking route attribute + a reqwest call yields NO contract-plane
    // records this slice — Rust contract extraction is deferred.
    let src = concat!(
        "pub struct Api;\n",
        "impl Api {\n",
        "    pub async fn list(&self) -> String {\n",
        "        client.get(\"/upstream\").send().await\n",
        "    }\n",
        "}\n",
    );
    let file = analyze("src/api.rs", src);
    assert!(file.routes.is_empty(), "no Rust routes this slice");
    assert!(file.http_calls.is_empty(), "no Rust http_calls this slice");
    assert!(file.gql_documents.is_empty(), "no Rust gql docs this slice");
    assert!(
        file.resolver_entries.is_empty(),
        "no Rust resolver entries this slice"
    );
    // But the code-plane extraction is real: the method and its calls exist.
    assert!(file.symbols.iter().any(|s| s.name == "list"));
    assert!(file.calls.iter().any(|c| c.callee_name == "get"));
}

// --- #12: messy/partial file does not panic; valid symbols survive -----------

#[test]
fn messy_file_does_not_panic_and_keeps_valid_symbols() {
    // `Ok` is a valid struct before a broken declaration.
    let src = "pub struct Ok; impl Ok { pub fn m(&self) {} }\npub fn broken( {";
    let file = analyze("src/a.rs", src); // must not panic
    assert!(
        file.symbols.iter().any(|s| s.name == "Ok"),
        "valid symbol before error should survive: {:?}",
        file.symbols
    );
}

// --- SQL candidate capture (data plane, Slice 16 D3 M2) ----------------------
//
// Each SQL-looking string literal becomes a `SqlCandidate` carrying the literal's
// inner text + the enclosing fn's module-qualified fqn. A non-SQL string is NOT
// captured.

fn sql_texts(file: &strata_core::AnalyzedFile) -> Vec<&str> {
    file.sql_candidates
        .iter()
        .map(|c| c.text.as_str())
        .collect()
}

#[test]
fn rust_captures_sql_string_literal_with_enclosing_fqn() {
    let src = concat!(
        "mod app {\n",
        "    pub struct Repo;\n",
        "    impl Repo {\n",
        "        pub fn load(&self) {\n",
        "            let q = \"SELECT email FROM users\";\n",
        "            run(q);\n",
        "        }\n",
        "    }\n",
        "}\n",
    );
    let file = analyze("repo.rs", src);
    assert_eq!(
        sql_texts(&file),
        vec!["SELECT email FROM users"],
        "the SQL string literal is captured with its quotes stripped"
    );
    assert_eq!(
        file.sql_candidates[0].enclosing_fqn, "app::Repo::load",
        "the candidate carries the module-qualified enclosing method fqn"
    );
}

#[test]
fn rust_captures_sql_in_raw_string() {
    // A raw string `r#"…"#` — its inner content is captured delimiter-free.
    let src = "fn m() { let q = r#\"INSERT INTO orders (id) VALUES (1)\"#; run(q); }";
    let file = analyze("r.rs", src);
    assert_eq!(sql_texts(&file), vec!["INSERT INTO orders (id) VALUES (1)"]);
}

#[test]
fn rust_does_not_capture_non_sql_string() {
    let src = "fn m() { let s = \"please update from the menu\"; log(s); }";
    let file = analyze("r.rs", src);
    assert!(
        file.sql_candidates.is_empty(),
        "a non-SQL prose string must not be captured, got {:?}",
        file.sql_candidates
    );
}
