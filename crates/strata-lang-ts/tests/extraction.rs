//! Extraction tests (brief Definition of Done #1–#7). Each runs `analyze` on
//! an inline source string and asserts on the resulting `AnalyzedFile`.

use strata_core::{
    CallRef, GqlDocument, HttpCall, ImportRef, NodeKind, RawSymbol, ResolverEntry, SqlCandidate,
    UrlShape,
};
use strata_lang_ts::analyze;

/// Convenience: all symbols with a given kind.
fn symbols_of(file: &strata_core::AnalyzedFile, kind: NodeKind) -> Vec<&RawSymbol> {
    file.symbols.iter().filter(|s| s.kind == kind).collect()
}

fn names(symbols: &[&RawSymbol]) -> Vec<String> {
    symbols.iter().map(|s| s.name.clone()).collect()
}

// --- #1: function declaration + exported function ----------------------------

#[test]
fn extracts_function_declaration_and_exported_function() {
    let src = "function foo() {}\nexport function bar() {}";
    let file = analyze("src/a.ts", src);
    let funcs = symbols_of(&file, NodeKind::Function);
    let mut got = names(&funcs);
    got.sort();
    assert_eq!(got, vec!["bar".to_string(), "foo".to_string()]);

    // Spans are sensible: 1-based start line, distinct lines.
    let foo = funcs.iter().find(|s| s.name == "foo").unwrap();
    let bar = funcs.iter().find(|s| s.name == "bar").unwrap();
    assert_eq!(foo.span.start_line, 1);
    assert_eq!(bar.span.start_line, 2);
    assert_eq!(foo.fqn, "foo");
    assert_eq!(bar.fqn, "bar");
    assert!(foo.container_fqn.is_none());
}

#[test]
fn extracts_export_default_function() {
    let src = "export default function baz(){}";
    let file = analyze("src/a.ts", src);
    let funcs = symbols_of(&file, NodeKind::Function);
    assert_eq!(names(&funcs), vec!["baz".to_string()]);
}

// --- #2: const-assigned arrow / function expression --------------------------

#[test]
fn extracts_const_arrow_and_function_expression() {
    let src = "const f = () => {};\nconst g = function(){};";
    let file = analyze("src/a.ts", src);
    let funcs = symbols_of(&file, NodeKind::Function);
    let mut got = names(&funcs);
    got.sort();
    assert_eq!(got, vec!["f".to_string(), "g".to_string()]);
    for s in &funcs {
        assert_eq!(s.fqn, s.name);
        assert!(s.container_fqn.is_none());
    }
}

// --- #3: class + method ------------------------------------------------------

#[test]
fn extracts_class_and_method() {
    let src = "class C { m() {} }";
    let file = analyze("src/a.ts", src);

    let classes = symbols_of(&file, NodeKind::Class);
    assert_eq!(names(&classes), vec!["C".to_string()]);
    assert_eq!(classes[0].fqn, "C");
    assert!(classes[0].container_fqn.is_none());

    let methods = symbols_of(&file, NodeKind::Method);
    assert_eq!(methods.len(), 1);
    let m = methods[0];
    assert_eq!(m.name, "m");
    assert_eq!(m.fqn, "C.m");
    assert_eq!(m.container_fqn.as_deref(), Some("C"));
}

// --- #4: interface (TS) + grammar selection by extension (JS) ----------------

#[test]
fn extracts_interface_in_typescript() {
    let src = "interface I {}";
    let file = analyze("src/a.ts", src);
    let ifaces = symbols_of(&file, NodeKind::Interface);
    assert_eq!(names(&ifaces), vec!["I".to_string()]);
    assert_eq!(ifaces[0].fqn, "I");
}

#[test]
fn javascript_grammar_selected_by_extension() {
    // A .js file: no interface syntax, but functions/classes must still extract.
    let src = "function j(){}\nclass K { p(){} }";
    let file = analyze("src/a.js", src);

    let funcs = symbols_of(&file, NodeKind::Function);
    assert_eq!(names(&funcs), vec!["j".to_string()]);

    let classes = symbols_of(&file, NodeKind::Class);
    assert_eq!(names(&classes), vec!["K".to_string()]);

    let methods = symbols_of(&file, NodeKind::Method);
    assert_eq!(methods.len(), 1);
    assert_eq!(methods[0].fqn, "K.p");
}

// --- #5: imports (named, default, namespace, side-effect, require, re-export)-

fn import_with_specifier<'a>(
    file: &'a strata_core::AnalyzedFile,
    spec: &str,
) -> Vec<&'a ImportRef> {
    file.imports
        .iter()
        .filter(|i| i.specifier == spec)
        .collect()
}

#[test]
fn extracts_named_default_namespace_sideeffect_require_and_reexport() {
    let src = r#"import { a, b } from "./named";
import D from "./default";
import * as ns from "./ns";
import "./side";
const x = require("./req");
export { a } from "./reexport";"#;
    let file = analyze("src/a.ts", src);

    // Named import.
    let named = import_with_specifier(&file, "./named");
    assert_eq!(named.len(), 1);
    assert_eq!(
        named[0].imported_names,
        vec!["a".to_string(), "b".to_string()]
    );

    // Default import.
    let default = import_with_specifier(&file, "./default");
    assert_eq!(default.len(), 1);
    assert_eq!(default[0].imported_names, vec!["D".to_string()]);

    // Namespace import.
    let ns = import_with_specifier(&file, "./ns");
    assert_eq!(ns.len(), 1);
    assert_eq!(ns[0].imported_names, vec!["ns".to_string()]);

    // Side-effect import: no names.
    let side = import_with_specifier(&file, "./side");
    assert_eq!(side.len(), 1);
    assert!(side[0].imported_names.is_empty());

    // require: specifier captured, no names.
    let req = import_with_specifier(&file, "./req");
    assert_eq!(req.len(), 1);
    assert!(req[0].imported_names.is_empty());

    // Re-export: specifier captured.
    let reexport = import_with_specifier(&file, "./reexport");
    assert_eq!(reexport.len(), 1);
    assert_eq!(reexport[0].imported_names, vec!["a".to_string()]);

    // require must NOT also show up as a call.
    assert!(
        !file.calls.iter().any(|c| c.callee_name == "require"),
        "require should be an import, not a call: {:?}",
        file.calls
    );
}

// --- #6: calls with receivers and enclosing fqn ------------------------------

#[test]
fn extracts_calls_with_receiver_and_enclosing_fqn() {
    let src = "function a(){ b(); obj.c(); this.d(); }\ne();";
    let file = analyze("src/a.ts", src);

    let find = |callee: &str| -> &CallRef {
        file.calls
            .iter()
            .find(|c| c.callee_name == callee)
            .unwrap_or_else(|| panic!("missing call to {callee}: {:?}", file.calls))
    };

    let b = find("b");
    assert_eq!(b.receiver, None);
    assert_eq!(b.enclosing_fqn, "a");

    let c = find("c");
    assert_eq!(c.receiver.as_deref(), Some("obj"));
    assert_eq!(c.enclosing_fqn, "a");

    let d = find("d");
    assert_eq!(d.receiver.as_deref(), Some("this"));
    assert_eq!(d.enclosing_fqn, "a");

    let e = find("e");
    assert_eq!(e.receiver, None);
    assert_eq!(e.enclosing_fqn, "");

    // Exactly four calls (b, c, d at top, e at module level).
    assert_eq!(file.calls.len(), 4, "calls were: {:?}", file.calls);
}

// --- #6b: callee_span pinpoints the callee identifier, not the receiver -------

#[test]
fn callee_span_points_at_callee_identifier() {
    // Single line so columns are easy to reason about (0-based cols).
    // `  bar(); ns.fn();`
    //   col 2 = bar, col 9 = ns, col 12 = fn
    let src = "function host() {  bar(); ns.method(); }";
    let file = analyze("src/a.ts", src);

    let bar = file.calls.iter().find(|c| c.callee_name == "bar").unwrap();
    // Bare call: callee_span equals the identifier `bar` (cols 19..22).
    assert_eq!(bar.callee_span.start_col, 19);
    assert_eq!(bar.callee_span.end_col, 22);

    let method = file
        .calls
        .iter()
        .find(|c| c.callee_name == "method")
        .unwrap();
    // Member call: callee_span is the PROPERTY `method`, not the receiver `ns`.
    // The whole-call span starts at the receiver `ns` (col 26); the callee
    // identifier starts later, at `method` (col 29).
    assert_eq!(method.receiver.as_deref(), Some("ns"));
    assert!(
        method.callee_span.start_col > method.span.start_col,
        "callee_span ({}) must start after the call span start ({}) for a member call",
        method.callee_span.start_col,
        method.span.start_col
    );
    assert_eq!(
        method.callee_span.start_col, 29,
        "`method` starts at col 29"
    );
}

// --- #6c: import name_spans are parallel to imported_names ---------------------

#[test]
fn import_name_spans_are_parallel_and_target_bound_identifiers() {
    let src = "import { foo as bar } from \"./b\";";
    let file = analyze("src/a.ts", src);
    let import = file.imports.iter().find(|i| i.specifier == "./b").unwrap();
    assert_eq!(import.imported_names, vec!["bar".to_string()]);
    assert_eq!(import.name_spans.len(), import.imported_names.len());
    // `bar` (the local binding) is at col 16 in `import { foo as bar } ...`.
    assert_eq!(import.name_spans[0].start_col, 16);
}

#[test]
fn method_body_calls_have_method_enclosing_fqn() {
    let src = "class C { m(){ helper(); this.n(); } }";
    let file = analyze("src/a.ts", src);
    let helper = file
        .calls
        .iter()
        .find(|c| c.callee_name == "helper")
        .unwrap();
    assert_eq!(helper.enclosing_fqn, "C.m");
    let n = file.calls.iter().find(|c| c.callee_name == "n").unwrap();
    assert_eq!(n.enclosing_fqn, "C.m");
    assert_eq!(n.receiver.as_deref(), Some("this"));
}

// --- #8: object-literal methods must NOT be emitted as Method symbols ---------

#[test]
fn object_literal_methods_are_not_emitted_as_symbols() {
    let src = "const o = { doThing() {} };";
    let file = analyze("o.ts", src);
    let methods = symbols_of(&file, NodeKind::Method);
    assert!(
        !methods.iter().any(|s| s.name == "doThing"),
        "object-literal method 'doThing' must not be emitted as a Method symbol, \
         but got: {:?}",
        methods
    );
}

// --- #9: abstract class and its methods are extracted -------------------------

#[test]
fn abstract_class_and_its_methods_are_extracted() {
    let src = "abstract class A { concrete() { this.helper(); } helper() {} }";
    let file = analyze("a.ts", src);

    // Class "A" must be present.
    let classes = symbols_of(&file, NodeKind::Class);
    assert!(
        classes.iter().any(|s| s.name == "A"),
        "abstract class 'A' must be extracted as a Class symbol, got: {:?}",
        classes
    );

    // Method "concrete" with correct fqn and container.
    let methods = symbols_of(&file, NodeKind::Method);
    let concrete = methods
        .iter()
        .find(|s| s.name == "concrete")
        .unwrap_or_else(|| panic!("method 'concrete' not found; methods: {:?}", methods));
    assert_eq!(concrete.fqn, "A.concrete");
    assert_eq!(concrete.container_fqn.as_deref(), Some("A"));

    // Method "helper" with correct fqn and container.
    let helper = methods
        .iter()
        .find(|s| s.name == "helper")
        .unwrap_or_else(|| panic!("method 'helper' not found; methods: {:?}", methods));
    assert_eq!(helper.fqn, "A.helper");
    assert_eq!(helper.container_fqn.as_deref(), Some("A"));
}

// --- #7: messy/partial file does not panic; valid symbols survive ------------

#[test]
fn messy_file_does_not_panic_and_keeps_valid_symbols() {
    // `ok` is valid and appears before the syntax error.
    let src = "function ok(){}\nfunction broken( {\nclass D {}";
    let file = analyze("src/a.ts", src); // must not panic

    let funcs = symbols_of(&file, NodeKind::Function);
    assert!(
        funcs.iter().any(|s| s.name == "ok"),
        "valid symbol before error should survive: {:?}",
        file.symbols
    );
}

// === Slice 3 M2 — Definition of Done test 7: route-declaration extraction =====
//
// Detect Express/router route declarations `<recv>.<verb>("<literal>", handler)`.
// The heuristic (and its false-positive guard) is: the method name must be in the
// known HTTP-verb set AND the first argument must be a string literal. A missed
// route is acceptable (no PRODUCES edge); a spurious route is not.

#[test]
fn extracts_named_handler_route() {
    let src = "app.get(\"/users/:id\", getUser);";
    let file = analyze("src/routes.ts", src);
    assert_eq!(file.routes.len(), 1, "one route, got {:?}", file.routes);
    let r = &file.routes[0];
    assert_eq!(r.method, "GET");
    assert_eq!(r.path, "/users/:id");
    assert_eq!(r.handler_name.as_deref(), Some("getUser"));
    assert_eq!(r.enclosing_fqn, ""); // declared at module top level
    assert_eq!(r.span.start_line, 1);
}

#[test]
fn inline_handler_has_no_handler_name() {
    let src = "router.post(\"/users\", (req, res) => { res.end(); });";
    let file = analyze("src/routes.ts", src);
    assert_eq!(file.routes.len(), 1, "one route, got {:?}", file.routes);
    let r = &file.routes[0];
    assert_eq!(r.method, "POST");
    assert_eq!(r.path, "/users");
    assert_eq!(
        r.handler_name, None,
        "an inline arrow handler yields no handler_name"
    );
}

#[test]
fn upper_cases_all_known_verbs() {
    let src = concat!(
        "app.get(\"/a\", h);\n",
        "app.post(\"/b\", h);\n",
        "app.put(\"/c\", h);\n",
        "app.delete(\"/d\", h);\n",
        "app.patch(\"/e\", h);\n",
        "router.all(\"/f\", h);\n",
    );
    let file = analyze("src/routes.ts", src);
    let mut got: Vec<(String, String)> = file
        .routes
        .iter()
        .map(|r| (r.method.clone(), r.path.clone()))
        .collect();
    got.sort();
    let mut want: Vec<(String, String)> = vec![
        ("GET".into(), "/a".into()),
        ("POST".into(), "/b".into()),
        ("PUT".into(), "/c".into()),
        ("DELETE".into(), "/d".into()),
        ("PATCH".into(), "/e".into()),
        ("ALL".into(), "/f".into()),
    ];
    want.sort();
    assert_eq!(got, want);
}

#[test]
fn non_route_calls_do_not_false_positive() {
    // 1) A verb-named method whose first arg is NOT a string literal (a Map/cache
    //    .get(key)) must not be a route. 2) A method NOT in the verb set, even
    //    with a string-literal first arg (`svc.fetch("/x")`), must not be a route.
    //    3) A bare function call `get("/x")` (no receiver) is not a route either.
    let src = concat!(
        "cache.get(key);\n",
        "svc.fetch(\"/x\");\n",
        "get(\"/y\");\n",
        "store.put(config, value);\n",
    );
    let file = analyze("src/app.ts", src);
    assert!(
        file.routes.is_empty(),
        "no routes should be detected, got {:?}",
        file.routes
    );
}

#[test]
fn route_records_enclosing_function() {
    let src = "function register(app) { app.get(\"/health\", check); }";
    let file = analyze("src/routes.ts", src);
    assert_eq!(file.routes.len(), 1, "one route, got {:?}", file.routes);
    assert_eq!(file.routes[0].enclosing_fqn, "register");
    assert_eq!(file.routes[0].handler_name.as_deref(), Some("check"));
}

// === Slice 3 M3 — Definition of Done test 1: HTTP-call extraction ============
//
// Detect outgoing HTTP calls (`fetch`, `axios.<verb>`, `axios(config)`) and
// classify the URL argument: a string literal → `Literal`, a template string
// (interpolations → `{}`) → `Template`, anything computed → `Dynamic`. The
// method comes from `opts.method` (fetch), the verb (axios.<verb>), or the
// config (`axios(config)`); a bare `fetch(url)` defaults to GET.

/// The sole [`HttpCall`] from analyzing `src`, panicking if there isn't exactly
/// one — keeps each assertion below focused on a single recognised call.
fn one_http_call(src: &str) -> HttpCall {
    let file = analyze("src/client.ts", src);
    assert_eq!(
        file.http_calls.len(),
        1,
        "expected exactly one http_call, got {:?}",
        file.http_calls
    );
    file.http_calls.into_iter().next().unwrap()
}

#[test]
fn fetch_literal_with_method_opts() {
    // fetch("/users", { method: "POST" }) → POST, Literal("/users").
    let h = one_http_call("fetch(\"/users\", { method: \"POST\" });");
    assert_eq!(h.method.as_deref(), Some("POST"));
    assert_eq!(h.url, UrlShape::Literal("/users".into()));
    assert_eq!(h.enclosing_fqn, "");
}

#[test]
fn fetch_concatenated_url_is_dynamic() {
    // fetch("/users/" + id) → a concatenation, URL opaque → Dynamic (method GET).
    let h = one_http_call("fetch(\"/users/\" + id);");
    assert_eq!(h.method.as_deref(), Some("GET"));
    assert_eq!(h.url, UrlShape::Dynamic);
}

#[test]
fn fetch_template_url_becomes_pattern() {
    // fetch(`/users/${id}`) → Template("/users/{}"), method defaults to GET.
    let h = one_http_call("fetch(`/users/${id}`);");
    assert_eq!(h.method.as_deref(), Some("GET"));
    assert_eq!(h.url, UrlShape::Template("/users/{}".into()));
}

#[test]
fn axios_get_literal() {
    // axios.get("/health") → GET, Literal("/health").
    let h = one_http_call("axios.get(\"/health\");");
    assert_eq!(h.method.as_deref(), Some("GET"));
    assert_eq!(h.url, UrlShape::Literal("/health".into()));
}

#[test]
fn axios_post_member_verb() {
    // axios.post("/users", body) → POST, Literal("/users").
    let h = one_http_call("axios.post(\"/users\", body);");
    assert_eq!(h.method.as_deref(), Some("POST"));
    assert_eq!(h.url, UrlShape::Literal("/users".into()));
}

#[test]
fn axios_config_object() {
    // axios({ url: "/users/{}" template via string here, method: "PUT" }).
    let h = one_http_call("axios({ url: \"/users\", method: \"PUT\" });");
    assert_eq!(h.method.as_deref(), Some("PUT"));
    assert_eq!(h.url, UrlShape::Literal("/users".into()));
}

#[test]
fn non_http_calls_do_not_false_positive() {
    // A non-fetch/axios bare call and a non-verb member call must not be HTTP.
    let file = analyze(
        "src/app.ts",
        "doThing(\"/x\");\ncache.read(\"/y\");\nlogger.info(\"/z\");",
    );
    assert!(
        file.http_calls.is_empty(),
        "no http_calls expected, got {:?}",
        file.http_calls
    );
}

#[test]
fn http_call_records_enclosing_function() {
    let src = "function loadUser(id) { return fetch(`/users/${id}`); }";
    let file = analyze("src/client.ts", src);
    assert_eq!(
        file.http_calls.len(),
        1,
        "one call, got {:?}",
        file.http_calls
    );
    assert_eq!(file.http_calls[0].enclosing_fqn, "loadUser");
    assert_eq!(
        file.http_calls[0].url,
        UrlShape::Template("/users/{}".into())
    );
}

#[test]
fn route_registration_is_not_also_an_http_call() {
    // `app.get("/users/:id", getUser)` is a server ROUTE (producer), not an
    // outgoing HTTP request. It shares the `recv.get(string, …)` shape with
    // `axios.get(...)`, so without disambiguation it would be mis-read as a
    // consumer call and invent a confident-wrong CONSUMES edge from the route
    // file to the operation it produces (R1/R5). It must be a route ONLY.
    let file = analyze("src/routes.ts", "app.get(\"/users/:id\", getUser);");
    assert_eq!(
        file.routes.len(),
        1,
        "the route is recorded: {:?}",
        file.routes
    );
    assert!(
        file.http_calls.is_empty(),
        "a route registration must NOT also be an http_call, got {:?}",
        file.http_calls
    );
}

#[test]
fn axios_call_is_not_mistaken_for_a_route() {
    // The mirror guard: `axios.get("/health")` / `axios.post("/users", body)`
    // are HTTP client calls (consumers), NOT routes — the `axios` receiver
    // disambiguates them from `app.get(...)`.
    let file = analyze(
        "src/client.ts",
        "axios.get(\"/health\");\naxios.post(\"/users\", body);",
    );
    assert!(
        file.routes.is_empty(),
        "axios.* calls must NOT be recorded as routes, got {:?}",
        file.routes
    );
    assert_eq!(
        file.http_calls.len(),
        2,
        "both axios calls are http_calls, got {:?}",
        file.http_calls
    );
}

// === Slice 4 M2 — Definition of Done test 1: gql tagged-template extraction ===
//
// A `gql`/`graphql` tagged template is a consumer document; an interpolated one
// is recorded but marked unreliable (counted, never linked); a non-gql tag
// (`css`…) is not a document at all.

#[test]
fn gql_tag_no_interpolation_is_captured_document() {
    let src = "export function load() {\n  return gql`query { getUser }`;\n}\n";
    let file = analyze("src/q.ts", src);
    assert_eq!(
        file.gql_documents.len(),
        1,
        "one gql document, got {:?}",
        file.gql_documents
    );
    let d = &file.gql_documents[0];
    assert!(
        d.interpolation_free,
        "no interpolations → interpolation_free"
    );
    assert_eq!(d.text, "query { getUser }");
    assert_eq!(
        d.enclosing_fqn, "load",
        "document attributed to its enclosing function"
    );
}

#[test]
fn graphql_tag_alias_is_also_captured() {
    // The `graphql` tag (Relay/codegen style) is recognised exactly like `gql`.
    let src = "const M = graphql`mutation { createUser }`;\n";
    let file = analyze("src/m.ts", src);
    assert_eq!(file.gql_documents.len(), 1, "one graphql document");
    let d = &file.gql_documents[0];
    assert!(d.interpolation_free);
    assert_eq!(d.text, "mutation { createUser }");
    assert_eq!(
        d.enclosing_fqn, "",
        "module top level → empty enclosing fqn"
    );
}

#[test]
fn interpolated_gql_tag_is_recorded_unreliable() {
    // An interpolated template (fragment composition) cannot be parsed safely:
    // recorded with interpolation_free=false and NO text — counted, never linked.
    let src = "const q = gql`query { ${userFields} getUser }`;\n";
    let file = analyze("src/q.ts", src);
    assert_eq!(
        file.gql_documents.len(),
        1,
        "the interpolated doc is recorded"
    );
    let d = &file.gql_documents[0];
    assert!(
        !d.interpolation_free,
        "an interpolated template must be flagged unreliable"
    );
    assert_eq!(d.text, "", "interpolated template stores no reliable text");
}

#[test]
fn non_gql_tagged_templates_are_not_documents() {
    // `css`/`styled`/`html` tags share the tagged-template shape but are NOT
    // GraphQL documents — only `gql`/`graphql` are.
    let src = concat!(
        "const s = css`color: red`;\n",
        "const h = html`<div></div>`;\n",
        "const t = styled.div`padding: 0`;\n",
    );
    let file = analyze("src/styles.ts", src);
    assert!(
        file.gql_documents.is_empty(),
        "non-gql tags must not be recorded as documents, got {:?}",
        file.gql_documents
    );
}

// === Slice 4 M2 — Definition of Done test 2: resolver-map extraction ==========
//
// `{ Query: { field: handler } }` → ResolverEntry per field (named/inline/
// identifier handler); a non-function inner value (`{ Query: { timeout: 30 } }`)
// → no entry; a non-root outer key → nothing.

/// Find the single resolver entry for `(op_type, field)` (panics if absent).
fn resolver<'a>(entries: &'a [ResolverEntry], op_type: &str, field: &str) -> &'a ResolverEntry {
    entries
        .iter()
        .find(|e| e.op_type == op_type && e.field == field)
        .unwrap_or_else(|| panic!("no resolver entry {op_type}.{field}; got {entries:?}"))
}

#[test]
fn resolver_map_extracts_named_inline_and_identifier_handlers() {
    let src = concat!(
        "function getUser() {}\n",
        "function createUser() {}\n",
        "const resolvers = {\n",
        "  Query: { getUser, listUsers: () => [] },\n",
        "  Mutation: { createUser: createUser, removeUser: function () {} },\n",
        "};\n",
    );
    let file = analyze("src/resolvers.ts", src);
    assert_eq!(
        file.resolver_entries.len(),
        4,
        "four resolver fields, got {:?}",
        file.resolver_entries
    );

    // Shorthand `getUser` → handler name == field.
    let get_user = resolver(&file.resolver_entries, "Query", "getUser");
    assert_eq!(get_user.handler_name.as_deref(), Some("getUser"));

    // Inline arrow `listUsers: () => []` → no handler name.
    let list_users = resolver(&file.resolver_entries, "Query", "listUsers");
    assert_eq!(list_users.handler_name, None);

    // Identifier value `createUser: createUser` → handler name == the identifier.
    let create_user = resolver(&file.resolver_entries, "Mutation", "createUser");
    assert_eq!(create_user.handler_name.as_deref(), Some("createUser"));

    // Inline function expression → no handler name.
    let remove_user = resolver(&file.resolver_entries, "Mutation", "removeUser");
    assert_eq!(remove_user.handler_name, None);
}

#[test]
fn resolver_map_skips_non_function_inner_values() {
    // `{ Query: { timeout: 30 } }` is a plain config object, NOT a resolver map:
    // the inner value is a number, so it must yield no entry (false-positive
    // guard). A string-key resolver field with a function value IS extracted.
    let src = concat!(
        "const config = { Query: { timeout: 30, retries: 3 } };\n",
        "const resolvers = { Query: { \"getUser\": getUser } };\n",
    );
    let file = analyze("src/x.ts", src);
    // Only the string-key getUser resolver survives.
    assert_eq!(
        file.resolver_entries.len(),
        1,
        "only the function-valued field is a resolver entry, got {:?}",
        file.resolver_entries
    );
    let e = &file.resolver_entries[0];
    assert_eq!(e.op_type, "Query");
    assert_eq!(e.field, "getUser");
    assert_eq!(e.handler_name.as_deref(), Some("getUser"));
}

#[test]
fn non_root_outer_keys_yield_no_resolver_entries() {
    // A plain object with no Query/Mutation/Subscription outer key is not a
    // resolver map at all — even if its values are functions.
    let src =
        "const opts = { onClick: () => {}, handler: doThing, User: { name: () => \"x\" } };\n";
    let file = analyze("src/opts.ts", src);
    assert!(
        file.resolver_entries.is_empty(),
        "no root-typed outer key → no resolver entries, got {:?}",
        file.resolver_entries
    );
}

#[test]
fn resolver_map_records_enclosing_function() {
    let src = concat!(
        "export function buildSchema() {\n",
        "  return makeExecutableSchema({ resolvers: { Subscription: { onPing: () => {} } } });\n",
        "}\n",
    );
    let file = analyze("src/build.ts", src);
    assert_eq!(file.resolver_entries.len(), 1, "one subscription resolver");
    let e = &file.resolver_entries[0];
    assert_eq!(e.op_type, "Subscription");
    assert_eq!(e.field, "onPing");
    assert_eq!(
        e.enclosing_fqn, "buildSchema",
        "resolver attributed to the enclosing function"
    );
}

/// A GqlDocument equality smoke check so the struct stays comparable in tests.
#[test]
fn gql_document_value_is_constructible() {
    let d = GqlDocument {
        text: "query { x }".into(),
        interpolation_free: true,
        tagged: true,
        enclosing_fqn: String::new(),
        span: strata_core::Span::default(),
    };
    assert!(d.interpolation_free);
}

// === dogfood fix 1 — test 1: untagged template-constant GraphQL documents ======
//
// The dominant AppSync/Amplify style is an UNTAGGED template constant:
// `export const GET_X = `query GetX { getX { id } }``. The tagged-only extractor
// missed these entirely; now they are captured as `tagged: false` candidates,
// while css/sql constants and substituted templates are kept out, and the tagged
// extraction still reports `tagged: true`.

#[test]
fn untagged_template_constant_is_captured_as_untagged_candidate() {
    let src = "export const GET_X = `query GetX { getX { id } }`;\n";
    let file = analyze("src/queries.ts", src);
    assert_eq!(
        file.gql_documents.len(),
        1,
        "one untagged candidate document, got {:?}",
        file.gql_documents
    );
    let d = &file.gql_documents[0];
    assert!(
        !d.tagged,
        "an untagged template constant must be recorded tagged: false"
    );
    assert!(
        d.interpolation_free,
        "a substitution-free template is interpolation_free"
    );
    assert_eq!(d.text, "query GetX { getX { id } }");
    assert_eq!(
        d.enclosing_fqn, "",
        "module-top-level const → empty enclosing fqn"
    );
}

#[test]
fn untagged_bare_selection_set_constant_is_captured() {
    // A bare `{ … }` selection set (anonymous query) also passes the prefilter,
    // attributed to its enclosing function.
    let src = concat!(
        "export function build() {\n",
        "  const q = `{ getX { id } }`;\n",
        "  return q;\n",
        "}\n",
    );
    let file = analyze("src/q.ts", src);
    assert_eq!(file.gql_documents.len(), 1, "one untagged candidate");
    let d = &file.gql_documents[0];
    assert!(!d.tagged);
    assert_eq!(d.text, "{ getX { id } }");
    assert_eq!(d.enclosing_fqn, "build");
}

#[test]
fn tagged_extraction_still_reports_tagged_true() {
    // The existing `gql` tagged path is unchanged except it now carries
    // tagged: true (the honesty discriminator).
    let src = "export function load() {\n  return gql`query { getUser }`;\n}\n";
    let file = analyze("src/q.ts", src);
    assert_eq!(file.gql_documents.len(), 1);
    let d = &file.gql_documents[0];
    assert!(d.tagged, "a `gql` tagged template is tagged: true");
    assert!(d.interpolation_free);
    assert_eq!(d.text, "query { getUser }");
}

#[test]
fn untagged_css_and_sql_constants_are_not_documents() {
    // The cheap prefilter keeps non-GraphQL template constants out of the pipeline
    // without parsing them: a css color, a SQL select, an HTML fragment, a path.
    let src = concat!(
        "const CSS = `color: red`;\n",
        "const SQL = `select * from t`;\n",
        "const HTML = `<div></div>`;\n",
        "const PATH = `/users/list`;\n",
    );
    let file = analyze("src/styles.ts", src);
    assert!(
        file.gql_documents.is_empty(),
        "css/sql/html/path constants must not be untagged candidates, got {:?}",
        file.gql_documents
    );
}

#[test]
fn untagged_template_with_substitution_is_not_a_document() {
    // An untagged template WITH a `${…}` substitution is never emitted: its text
    // is unreliable and it is far more likely a computed non-GraphQL string.
    let src = "const Q = `query GetX { getX(id: \"${id}\") { id } }`;\n";
    let file = analyze("src/q.ts", src);
    assert!(
        file.gql_documents.is_empty(),
        "a substituted untagged template must not be a document, got {:?}",
        file.gql_documents
    );
}

#[test]
fn untagged_query_keyword_substring_does_not_false_match() {
    // The keyword-boundary guard: `queryClient`/`mutationObserver` etc. must NOT
    // be read as `query …`/`mutation …`. A `queryString`-prefixed literal is not a
    // GraphQL document.
    let src = "const C = `queryClientConfig = whatever`;\n";
    let file = analyze("src/c.ts", src);
    assert!(
        file.gql_documents.is_empty(),
        "a `queryClient…` substring must not match the `query` keyword, got {:?}",
        file.gql_documents
    );
}

// --- SQL candidate capture (data plane, Slice 16 D3 M2) ----------------------
//
// Each SQL-looking string literal becomes a `SqlCandidate` carrying the literal's
// inner text and its enclosing function fqn. A non-SQL string is NOT captured (the
// cheap keyword prefilter), and a `${…}`-interpolated template is NOT a single
// literal so it is honestly dropped (dynamic SQL).

/// All SQL candidates' inner texts.
fn sql_texts(file: &strata_core::AnalyzedFile) -> Vec<&str> {
    file.sql_candidates
        .iter()
        .map(|c| c.text.as_str())
        .collect()
}

#[test]
fn captures_sql_string_literal_with_enclosing_fqn() {
    let src = "function loadUser() { return db.query(\"SELECT email FROM users WHERE id = 1\"); }";
    let file = analyze("src/a.ts", src);
    assert_eq!(
        sql_texts(&file),
        vec!["SELECT email FROM users WHERE id = 1"],
        "the SQL string literal is captured with its quotes stripped"
    );
    let cand = &file.sql_candidates[0];
    assert_eq!(
        cand.enclosing_fqn, "loadUser",
        "the candidate carries the enclosing function fqn"
    );
    assert!(cand.span.start_line >= 1);
}

#[test]
fn captures_sql_in_template_string_without_substitutions() {
    // A backtick template with NO `${…}` is a single literal — captured.
    let src = "function f() { return db.run(`INSERT INTO orders (id) VALUES (1)`); }";
    let file = analyze("src/a.ts", src);
    assert_eq!(sql_texts(&file), vec!["INSERT INTO orders (id) VALUES (1)"]);
    assert_eq!(file.sql_candidates[0].enclosing_fqn, "f");
}

#[test]
fn does_not_capture_non_sql_string() {
    let src = "function f() { const msg = \"hello, please update your profile\"; log(msg); }";
    let file = analyze("src/a.ts", src);
    assert!(
        file.sql_candidates.is_empty(),
        "a non-SQL string must not be captured, got {:?}",
        file.sql_candidates
    );
}

#[test]
fn does_not_capture_interpolated_template_sql_dynamic() {
    // A `${table}` interpolation makes this NOT a single literal — dynamic SQL is
    // honestly dropped (never guessed).
    let src = "function f(table: string) { return db.run(`SELECT * FROM ${table}`); }";
    let file = analyze("src/a.ts", src);
    assert!(
        file.sql_candidates.is_empty(),
        "an interpolated template (dynamic SQL) must not be captured, got {:?}",
        file.sql_candidates
    );
}

#[test]
fn module_top_level_sql_has_empty_enclosing_fqn() {
    let src = "export const Q = \"SELECT id FROM users\";\n";
    let file = analyze("src/a.ts", src);
    assert_eq!(sql_texts(&file), vec!["SELECT id FROM users"]);
    assert_eq!(
        file.sql_candidates[0].enclosing_fqn, "",
        "a module-top-level SQL literal has an empty enclosing fqn (falls back to the module)"
    );
}

/// Suppress unused-import warnings for the shared imports this file uses across
/// many test fns (SqlCandidate is referenced via the field type only).
#[allow(dead_code)]
fn _sql_candidate_type_is_imported(c: SqlCandidate) -> String {
    c.text
}

// --- ORM model hints (Slice 25, D3, M2b) ------------------------------------
//
// TS TypeORM `@Entity("…")` class decorator, explicit literal name only. A bare
// `@Entity()`, a non-Entity decorator, and a non-decorated class yield NO hint.
// Drizzle `export const x = pgTable("…")` is DEFERRED (the analyzer emits no node
// for the const to link from) — it must yield no hint this slice.

use strata_core::OrmFramework;

#[test]
fn ts_typeorm_entity_decorator_yields_one_orm_hint() {
    let src = "@Entity(\"users\")\nclass User {\n  id: number;\n}\n";
    let file = analyze("user.entity.ts", src);
    assert_eq!(
        file.orm_models.len(),
        1,
        "exactly one ORM hint: {:?}",
        file.orm_models
    );
    let h = &file.orm_models[0];
    assert_eq!(h.model_fqn, "User", "the decorated class fqn");
    assert_eq!(h.table_name, "users", "the unquoted literal table name");
    assert_eq!(h.framework, OrmFramework::TypeOrm);
}

#[test]
fn ts_exported_entity_class_yields_one_orm_hint() {
    // The common `@Entity("x")\nexport class X` form — the grammar hoists the
    // decorator to the `export_statement`, so the extractor must scan the parent too.
    let src = "@Entity(\"users\")\nexport class User {\n  id: number;\n}\n";
    let file = analyze("user.entity.ts", src);
    assert_eq!(
        file.orm_models.len(),
        1,
        "exactly one ORM hint for an exported entity: {:?}",
        file.orm_models
    );
    let h = &file.orm_models[0];
    assert_eq!(h.model_fqn, "User");
    assert_eq!(h.table_name, "users");
    assert_eq!(h.framework, OrmFramework::TypeOrm);
}

#[test]
fn ts_entity_decorator_without_string_arg_yields_no_orm_hint() {
    // `@Entity()` (no explicit name) relies on the class-name convention → no hint
    // this slice (convention inference deferred).
    let src = "@Entity()\nclass User {\n  id: number;\n}\n";
    let file = analyze("user.entity.ts", src);
    assert!(
        file.orm_models.is_empty(),
        "no explicit name → no hint: {:?}",
        file.orm_models
    );
}

#[test]
fn ts_non_entity_decorator_yields_no_orm_hint() {
    // A class with a different decorator (`@Component(...)`) is not an ORM model.
    let src = "@Component(\"app\")\nclass AppComponent {}\n";
    let file = analyze("app.component.ts", src);
    assert!(
        file.orm_models.is_empty(),
        "a non-Entity decorator yields no hint: {:?}",
        file.orm_models
    );
}

#[test]
fn ts_plain_class_yields_no_orm_hint() {
    let src = "class Helper {\n  run() { return 1; }\n}\n";
    let file = analyze("util.ts", src);
    assert!(
        file.orm_models.is_empty(),
        "a plain class yields no hint: {:?}",
        file.orm_models
    );
}

#[test]
fn ts_drizzle_pgtable_is_deferred_no_orm_hint() {
    // Drizzle `export const users = pgTable("users", {})` — DEFERRED this slice (the
    // analyzer emits no symbol node for a non-function `const`, so there is no model
    // node to link a MapsTo from). It must yield NO hint (honest deferral, not a
    // silent half-link).
    let src = "export const users = pgTable(\"users\", { id: integer(\"id\") });\n";
    let file = analyze("schema.ts", src);
    assert!(
        file.orm_models.is_empty(),
        "Drizzle pgTable is deferred → no hint: {:?}",
        file.orm_models
    );
}
