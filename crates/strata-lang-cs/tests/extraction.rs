//! C# extraction tests. Each runs `analyze` on an inline source string and
//! asserts on the resulting `AnalyzedFile`. TDD red-first per construct
//! (classes/interfaces/structs/records, methods/ctors/local-functions, usings,
//! namespaces, calls).
//!
//! The discipline mirrors `strata-lang-py/tests/extraction.rs`: a missed symbol
//! is acceptable degradation; an *invented* one is not. The namespace fqn
//! convention (namespace-qualified, type-nested with `.`) is pinned here.

use strata_core::{CallRef, ImportRef, NodeKind, RawSymbol};
use strata_lang_cs::analyze;

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

// --- #1: classes (incl. bases) + interface kind ------------------------------

#[test]
fn extracts_classes_interfaces_and_bases() {
    let src = concat!(
        "public interface IService { void Run(); }\n",
        "public abstract class Base { }\n",
        "public class Worker : Base, IService { }\n",
    );
    let file = analyze("src/W.cs", src);

    // Interface is its own kind.
    let interfaces = symbols_of(&file, NodeKind::Interface);
    assert_eq!(names(&interfaces), vec!["IService".to_string()]);

    // Classes (Base, Worker). Bases are parsed for the header but no Extends edge
    // is emitted this slice (model carries no bases field) — the symbol still
    // extracts.
    let classes = symbols_of(&file, NodeKind::Class);
    let mut got = names(&classes);
    got.sort();
    assert_eq!(got, vec!["Base".to_string(), "Worker".to_string()]);
    // The *types* (class/interface) have no container; the interface's `Run`
    // method legitimately has the interface as its container (it is a member).
    for ty in classes.iter().chain(interfaces.iter()) {
        assert!(
            ty.container_fqn.is_none(),
            "{} is a top-level type",
            ty.name
        );
    }
    let run = sym(&file, "Run");
    assert_eq!(
        run.container_fqn.as_deref(),
        Some("IService"),
        "an interface method is a member of its interface"
    );
}

// --- #2: structs + records extract as Class-kind (documented) ----------------

#[test]
fn structs_and_records_are_class_kind() {
    let src = concat!(
        "public struct Vec { public int X; }\n",
        "public record Money(int Amount);\n",
        "public record struct Point(int X, int Y);\n",
    );
    let file = analyze("src/types.cs", src);
    let classes = symbols_of(&file, NodeKind::Class);
    let mut got = names(&classes);
    got.sort();
    // All three are Class-kind (struct/record/record-struct collapse to Class).
    assert_eq!(
        got,
        vec!["Money".to_string(), "Point".to_string(), "Vec".to_string()],
        "struct + record + record struct all extract as Class: {:?}",
        file.symbols
    );
    // None are Interface.
    assert!(symbols_of(&file, NodeKind::Interface).is_empty());
}

// --- #3: methods (incl. static/async/constructor) ----------------------------

#[test]
fn extracts_methods_static_async_and_constructor() {
    let src = concat!(
        "public class C {\n",
        "    public C() { }\n",
        "    public C(int a) { }\n",
        "    public void Run() { }\n",
        "    public static string Build() { return \"\"; }\n",
        "    public async Task RunAsync() { }\n",
        "}\n",
    );
    let file = analyze("src/C.cs", src);

    let methods = symbols_of(&file, NodeKind::Method);
    let mut got = names(&methods);
    got.sort();
    // Constructors are Methods named after the class `C` (two overloads collapse
    // to the same name); Run/Build/RunAsync are methods. static/async are not
    // special-cased.
    assert_eq!(
        got,
        vec![
            "Build".to_string(),
            "C".to_string(),
            "C".to_string(),
            "Run".to_string(),
            "RunAsync".to_string()
        ],
        "ctors + methods all extract as Method: {:?}",
        file.symbols
    );
    // Every method's container is the class C, and its fqn is `C.<name>`.
    for m in &methods {
        assert_eq!(
            m.container_fqn.as_deref(),
            Some("C"),
            "{} container",
            m.name
        );
        assert_eq!(m.fqn, format!("C.{}", m.name), "{} fqn", m.name);
    }
}

// --- #4: overloads collapse to one fqn per name ------------------------------

#[test]
fn overloads_collapse_to_one_fqn() {
    // Two `Process` overloads on the same type. They MUST produce two RawSymbols
    // with the SAME fqn (`C.Process`) — the deliberate name-level collapse
    // (arity-aware splitting is a Roslyn refinement).
    let src = concat!(
        "public class C {\n",
        "    public void Process() { }\n",
        "    public void Process(int n) { }\n",
        "}\n",
    );
    let file = analyze("src/C.cs", src);
    let processes: Vec<&RawSymbol> = file
        .symbols
        .iter()
        .filter(|s| s.name == "Process")
        .collect();
    assert_eq!(
        processes.len(),
        2,
        "both overloads are extracted as symbols"
    );
    assert!(
        processes.iter().all(|s| s.fqn == "C.Process"),
        "both overloads share the collapsed fqn `C.Process`: {:?}",
        processes
    );
}

// --- #5: namespace qualifies type and member fqns (THE convention) -----------

#[test]
fn namespace_qualifies_type_and_member_fqns() {
    // Block namespace.
    let block = analyze(
        "src/B.cs",
        "namespace App.Services { public class Worker { public void Run() { } } }",
    );
    let worker = sym(&block, "Worker");
    assert_eq!(
        worker.fqn, "App.Services.Worker",
        "the namespace is a dotted prefix on the type fqn"
    );
    let run = sym(&block, "Run");
    assert_eq!(
        run.fqn, "App.Services.Worker.Run",
        "the member fqn is namespace.Type.member"
    );
    assert_eq!(
        run.container_fqn.as_deref(),
        Some("App.Services.Worker"),
        "the method's container is the namespace-qualified type fqn"
    );

    // File-scoped namespace yields the SAME convention.
    let scoped = analyze(
        "src/F.cs",
        "namespace App.Services;\npublic class Worker { public void Run() { } }\n",
    );
    assert_eq!(sym(&scoped, "Worker").fqn, "App.Services.Worker");
    assert_eq!(sym(&scoped, "Run").fqn, "App.Services.Worker.Run");
}

#[test]
fn nested_namespaces_compose_with_dots() {
    let file = analyze(
        "src/N.cs",
        "namespace A { namespace B { public class C { } } }",
    );
    assert_eq!(
        sym(&file, "C").fqn,
        "A.B.C",
        "nested namespaces compose into a dotted prefix"
    );
}

// --- #6: usings (plain, aliased, global, qualified) --------------------------

fn using_with_specifier<'a>(file: &'a strata_core::AnalyzedFile, spec: &str) -> Vec<&'a ImportRef> {
    file.imports
        .iter()
        .filter(|i| i.specifier == spec)
        .collect()
}

#[test]
fn extracts_plain_qualified_global_and_alias_usings() {
    let src = concat!(
        "using System;\n",
        "using System.Collections.Generic;\n",
        "using Alias = System.Text.StringBuilder;\n",
        "global using System.Linq;\n",
    );
    let file = analyze("src/U.cs", src);

    // `using System;` → specifier "System", bound name "System".
    let sys = using_with_specifier(&file, "System");
    assert_eq!(sys.len(), 1, "one `using System;`, got {:?}", file.imports);
    assert_eq!(sys[0].imported_names, vec!["System".to_string()]);

    // Qualified namespace specifier preserved verbatim.
    let gen = using_with_specifier(&file, "System.Collections.Generic");
    assert_eq!(gen.len(), 1, "qualified using preserved");

    // `using Alias = System.Text.StringBuilder;` → specifier is the TARGET, bound
    // name is the alias.
    let alias = using_with_specifier(&file, "System.Text.StringBuilder");
    assert_eq!(
        alias.len(),
        1,
        "alias using records the target as specifier"
    );
    assert_eq!(
        alias[0].imported_names,
        vec!["Alias".to_string()],
        "the alias is the bound name"
    );

    // `global using System.Linq;` → recorded like a plain using.
    let linq = using_with_specifier(&file, "System.Linq");
    assert_eq!(linq.len(), 1, "global using recorded");
    assert_eq!(linq[0].imported_names, vec!["System.Linq".to_string()]);
}

// --- #7: calls with receivers + enclosing fqn --------------------------------

#[test]
fn extracts_calls_with_receiver_and_enclosing_fqn() {
    let src = concat!(
        "namespace N {\n",
        "public class C {\n",
        "    public void M() {\n",
        "        Helper();\n",            // bare
        "        this.Other();\n",        // this-receiver
        "        svc.Do();\n",            // member-receiver
        "        Console.WriteLine();\n", // static-type receiver
        "        Build().Trim();\n",      // chained: receiver is itself a call
        "    }\n",
        "}\n",
        "}\n",
    );
    let file = analyze("src/C.cs", src);

    let helper = find_call(&file, "Helper");
    assert_eq!(helper.receiver, None);
    assert_eq!(
        helper.enclosing_fqn, "N.C.M",
        "enclosing is the qualified method fqn"
    );

    let other = find_call(&file, "Other");
    assert_eq!(other.receiver.as_deref(), Some("this"));
    assert_eq!(other.enclosing_fqn, "N.C.M");

    let do_call = find_call(&file, "Do");
    assert_eq!(do_call.receiver.as_deref(), Some("svc"));

    let writeline = find_call(&file, "WriteLine");
    assert_eq!(
        writeline.receiver.as_deref(),
        Some("Console"),
        "static-type receiver recorded verbatim"
    );

    // Chained `Build().Trim()` → callee `Trim`, receiver the immediate object text
    // `Build()` (the honest receiver chain, even though it is itself a call).
    let trim = find_call(&file, "Trim");
    assert_eq!(trim.callee_name, "Trim");
    assert_eq!(
        trim.receiver.as_deref(),
        Some("Build()"),
        "the receiver is the full immediate-object text"
    );
    // The inner `Build()` is also recorded as a bare call.
    let build = find_call(&file, "Build");
    assert_eq!(build.receiver, None);
}

// --- #8: local function is a Function, not a method --------------------------

#[test]
fn local_function_is_a_function_not_a_method() {
    let src = concat!(
        "public class C {\n",
        "    public void M() {\n",
        "        void inner() { }\n",
        "        inner();\n",
        "    }\n",
        "}\n",
    );
    let file = analyze("src/C.cs", src);
    // `inner` is a Function with no container (a local function is a closure, not a
    // type member).
    let inner = sym(&file, "inner");
    assert_eq!(inner.kind, NodeKind::Function);
    assert!(
        inner.container_fqn.is_none(),
        "a local function is not a type member"
    );
    // `M` is still a Method of C.
    assert_eq!(sym(&file, "M").kind, NodeKind::Method);
}

// --- #9: reflection / dynamic targets are NEVER invented ---------------------

#[test]
fn reflection_call_does_not_invent_the_reflected_method() {
    // `t.GetMethod("Run")` must record a member call to `GetMethod` (receiver `t`),
    // and must NOT invent a call to a method named `Run` from the string argument.
    let src = concat!(
        "public class C {\n",
        "    public void Reflect(System.Type t) {\n",
        "        var mi = t.GetMethod(\"Run\");\n",
        "        mi.Invoke(this, null);\n",
        "    }\n",
        "}\n",
    );
    let file = analyze("src/C.cs", src);

    // The reflective API calls themselves are recorded honestly.
    assert!(
        file.calls
            .iter()
            .any(|c| c.callee_name == "GetMethod" && c.receiver.as_deref() == Some("t")),
        "the GetMethod call is recorded: {:?}",
        file.calls
    );
    assert!(
        file.calls
            .iter()
            .any(|c| c.callee_name == "Invoke" && c.receiver.as_deref() == Some("mi")),
        "the Invoke call is recorded: {:?}",
        file.calls
    );
    // The reflected method name from the STRING ARGUMENT must never become a call.
    assert!(
        !file.calls.iter().any(|c| c.callee_name == "Run"),
        "the reflected `\"Run\"` string must not be invented as a call: {:?}",
        file.calls
    );
}

#[test]
fn computed_callee_yields_no_static_call() {
    // `factory()()` — the outer invocation's function is itself an invocation, a
    // computed/dynamic target. The outer call must NOT invent a callee; the inner
    // `factory()` IS a real bare call.
    let src = "public class C { public void M() { factory()(); } }";
    let file = analyze("src/C.cs", src);
    assert!(
        file.calls
            .iter()
            .any(|c| c.callee_name == "factory" && c.receiver.is_none()),
        "the inner factory() call is recorded: {:?}",
        file.calls
    );
    // Exactly one call (the inner factory) — the outer dynamic invocation adds none.
    assert_eq!(
        file.calls.len(),
        1,
        "only the inner call is recorded, got {:?}",
        file.calls
    );
}

// --- #10: contract-plane vecs stay empty this slice (the honesty story) -------

#[test]
fn contract_plane_vecs_are_empty_for_csharp() {
    // Even an ASP.NET-looking attribute + an HttpClient call yields NO
    // contract-plane records this slice — C# contract extraction is deferred.
    let src = concat!(
        "public class Api {\n",
        "    [HttpGet(\"/users\")]\n",
        "    public string List() {\n",
        "        return client.GetAsync(\"/upstream\").Result.ToString();\n",
        "    }\n",
        "}\n",
    );
    let file = analyze("src/Api.cs", src);
    assert!(file.routes.is_empty(), "no C# routes this slice");
    assert!(file.http_calls.is_empty(), "no C# http_calls this slice");
    assert!(file.gql_documents.is_empty(), "no C# gql docs this slice");
    assert!(
        file.resolver_entries.is_empty(),
        "no C# resolver entries this slice"
    );
    // But the code-plane extraction is real: the method and its calls exist.
    assert!(file.symbols.iter().any(|s| s.name == "List"));
    assert!(file.calls.iter().any(|c| c.callee_name == "GetAsync"));
}

// --- #11: messy/partial file does not panic; valid symbols survive -----------

#[test]
fn messy_file_does_not_panic_and_keeps_valid_symbols() {
    // `Ok` is a valid class before a broken declaration.
    let src = "public class Ok { public void M() { } }\npublic class Broken { public void (";
    let file = analyze("src/A.cs", src); // must not panic
    assert!(
        file.symbols.iter().any(|s| s.name == "Ok"),
        "valid symbol before error should survive: {:?}",
        file.symbols
    );
}

// --- SQL candidate capture (data plane, Slice 16 D3 M2) ----------------------
//
// Each SQL-looking constant string literal becomes a `SqlCandidate` carrying the
// literal's inner text + the enclosing method's namespace-qualified fqn. A non-SQL
// string is NOT captured; a `$"…"` interpolated string is NOT a constant literal →
// dynamic SQL, dropped.

fn sql_texts(file: &strata_core::AnalyzedFile) -> Vec<&str> {
    file.sql_candidates
        .iter()
        .map(|c| c.text.as_str())
        .collect()
}

#[test]
fn cs_captures_sql_string_literal_with_enclosing_fqn() {
    let src = "namespace App { class Repo { void Load() { var q = \"SELECT email FROM users\"; Run(q); } } }";
    let file = analyze("Repo.cs", src);
    assert_eq!(
        sql_texts(&file),
        vec!["SELECT email FROM users"],
        "the SQL string literal is captured with its quotes stripped"
    );
    assert_eq!(
        file.sql_candidates[0].enclosing_fqn, "App.Repo.Load",
        "the candidate carries the namespace-qualified enclosing method fqn"
    );
}

#[test]
fn cs_captures_sql_in_verbatim_string() {
    // A verbatim `@"…"` literal is a single leaf node — its `@"` prefix is stripped.
    let src = "namespace App { class R { void M() { var q = @\"INSERT INTO orders (id) VALUES (1)\"; Run(q); } } }";
    let file = analyze("R.cs", src);
    assert_eq!(sql_texts(&file), vec!["INSERT INTO orders (id) VALUES (1)"]);
    assert_eq!(file.sql_candidates[0].enclosing_fqn, "App.R.M");
}

#[test]
fn cs_captures_sql_in_raw_string() {
    let src = "namespace App { class R { void M() { var q = \"\"\"DELETE FROM sessions\"\"\"; Run(q); } } }";
    let file = analyze("R.cs", src);
    assert_eq!(sql_texts(&file), vec!["DELETE FROM sessions"]);
}

#[test]
fn cs_does_not_capture_non_sql_string() {
    let src = "namespace App { class R { void M() { var s = \"please update from the menu\"; Log(s); } } }";
    let file = analyze("R.cs", src);
    assert!(
        file.sql_candidates.is_empty(),
        "a non-SQL prose string must not be captured, got {:?}",
        file.sql_candidates
    );
}

#[test]
fn cs_does_not_capture_interpolated_string_sql_dynamic() {
    let src = "namespace App { class R { void M(string t) { var q = $\"SELECT * FROM {t}\"; Run(q); } } }";
    let file = analyze("R.cs", src);
    assert!(
        file.sql_candidates.is_empty(),
        "a $\"…\" interpolated string (dynamic SQL) must not be captured, got {:?}",
        file.sql_candidates
    );
}
