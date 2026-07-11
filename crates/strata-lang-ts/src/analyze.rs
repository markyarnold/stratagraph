//! Pure per-file extraction of symbols, imports, and intra-file calls.
//!
//! Operates on a source string via Tree-sitter; performs no IO. Grammar is
//! selected by file extension. Extraction is a manual cursor/recursive walk
//! over the parse tree (no Tree-sitter queries), which keeps the direct
//! dependency set minimal and gives precise control over enclosing-scope
//! tracking for call sites.

use strata_core::{
    AnalyzedFile, CallRef, GqlDocument, HttpCall, ImportRef, NodeKind, OrmFramework, OrmModelHint,
    RawSymbol, ResolverEntry, RouteDecl, Span, SqlCandidate, UrlShape,
};
use tree_sitter::{Language, Node, Parser};

/// The HTTP-verb method names a route-declaration call may use, lower-case as
/// written in the call (`app.get`, `router.post`, …). `all` is Express's
/// match-any-method registrar. Kept deliberately small — a name OUTSIDE this set
/// is never treated as a route (false-positive guard); the conservative cost is a
/// missed route, never an invented one.
const ROUTE_METHODS: [&str; 8] = [
    "get", "post", "put", "delete", "patch", "head", "options", "all",
];

/// Which Tree-sitter grammar to use for a given file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Grammar {
    /// tree-sitter-typescript, `typescript` dialect (`.ts`).
    TypeScript,
    /// tree-sitter-typescript, `tsx` dialect (`.tsx`).
    Tsx,
    /// tree-sitter-javascript (`.js`, `.jsx`, `.mjs`, `.cjs`).
    JavaScript,
}

impl Grammar {
    /// Pick a grammar from a file path's extension. Defaults to TypeScript for
    /// anything unrecognized so callers always get a best-effort parse.
    fn for_path(path: &str) -> Grammar {
        let ext = path.rsplit('.').next().unwrap_or("");
        match ext {
            "tsx" => Grammar::Tsx,
            "js" | "jsx" | "mjs" | "cjs" => Grammar::JavaScript,
            // "ts", "d.ts" (matches "ts"), "mts", "cts", and unknown -> TypeScript.
            _ => Grammar::TypeScript,
        }
    }

    fn language(self) -> Language {
        match self {
            Grammar::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Grammar::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Grammar::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
        }
    }
}

/// Analyze a single TypeScript/JavaScript source file.
///
/// Pure: no filesystem access. Returns whatever can be extracted; on a parse
/// failure (grammar load) returns an empty `AnalyzedFile` rather than panicking.
pub fn analyze(path: &str, source: &str) -> AnalyzedFile {
    let grammar = Grammar::for_path(path);
    let mut parser = Parser::new();
    if parser.set_language(&grammar.language()).is_err() {
        return AnalyzedFile::default();
    }
    let Some(tree) = parser.parse(source, None) else {
        return AnalyzedFile::default();
    };

    let bytes = source.as_bytes();
    let mut out = AnalyzedFile::default();
    let root = tree.root_node();
    walk(root, bytes, None, "", &mut out);
    out
}

/// Convert a Tree-sitter node's range into a core `Span` (1-based lines,
/// 0-based columns — matching the convention used elsewhere in the workspace).
fn span_of(node: Node) -> Span {
    let start = node.start_position();
    let end = node.end_position();
    Span {
        start_line: start.row as u32 + 1,
        start_col: start.column as u32,
        end_line: end.row as u32 + 1,
        end_col: end.column as u32,
    }
}

/// Source text of a node.
fn text<'a>(node: Node, bytes: &'a [u8]) -> &'a str {
    node.utf8_text(bytes).unwrap_or("")
}

/// Build a fully-qualified name from an optional container and a leaf name.
fn make_fqn(container: Option<&str>, name: &str) -> String {
    match container {
        Some(c) if !c.is_empty() => format!("{c}.{name}"),
        _ => name.to_string(),
    }
}

/// Recursive walk. `container` is the enclosing class fqn for member symbols;
/// `enclosing_fqn` is the nearest enclosing function/method fqn for call sites
/// (empty string at module top level).
fn walk(
    node: Node,
    bytes: &[u8],
    container: Option<&str>,
    enclosing_fqn: &str,
    out: &mut AnalyzedFile,
) {
    match node.kind() {
        "function_declaration" | "generator_function_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, bytes).to_string();
                let fqn = name.clone();
                out.symbols.push(RawSymbol {
                    kind: NodeKind::Function,
                    name,
                    fqn: fqn.clone(),
                    container_fqn: None,
                    span: span_of(node),
                });
                // Descend into the body with this function as the enclosing scope.
                walk_children(node, bytes, None, &fqn, out);
                return;
            }
        }
        "class_declaration" | "class" | "abstract_class_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, bytes).to_string();
                let fqn = name.clone();
                out.symbols.push(RawSymbol {
                    kind: NodeKind::Class,
                    name,
                    fqn: fqn.clone(),
                    container_fqn: None,
                    span: span_of(node),
                });
                // ORM model hint (Slice 25, D3, M2b): a TypeORM `@Entity("…")` class
                // decorator with an explicit first string-literal arg. Captured
                // additively; a bare `@Entity()` or a non-Entity decorator yields none
                // (explicit literal only — convention/dynamic never invented, R1/R5).
                if let Some(hint) = extract_orm_model_hint(node, bytes, &fqn) {
                    out.orm_models.push(hint);
                }
                // Members are extracted with this class as their container.
                walk_children(node, bytes, Some(&fqn), enclosing_fqn, out);
                return;
            }
        }
        "interface_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, bytes).to_string();
                out.symbols.push(RawSymbol {
                    kind: NodeKind::Interface,
                    name: name.clone(),
                    fqn: name,
                    container_fqn: None,
                    span: span_of(node),
                });
                return;
            }
        }
        "method_definition" => {
            // Only emit a Method symbol when this is a real class member
            // (parent kind is "class_body"). Object-literal methods share the
            // same node kind but have a parent of kind "object" — those must
            // NOT produce a symbol; we still descend so calls inside are walked.
            let is_class_member = node.parent().map(|p| p.kind()) == Some("class_body");
            if is_class_member {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = text(name_node, bytes).to_string();
                    let fqn = make_fqn(container, &name);
                    out.symbols.push(RawSymbol {
                        kind: NodeKind::Method,
                        name,
                        fqn: fqn.clone(),
                        container_fqn: container.map(str::to_string),
                        span: span_of(node),
                    });
                    // Method body: enclosing scope becomes the method fqn; no new container.
                    walk_children(node, bytes, None, &fqn, out);
                    return;
                }
            }
            // Object-literal method (or unrecognised parent): descend but emit nothing.
            walk_children(node, bytes, container, enclosing_fqn, out);
            return;
        }
        "lexical_declaration" | "variable_declaration" => {
            // `const f = () => {}` / `const g = function(){}` -> Function "f"/"g".
            extract_const_functions(node, bytes, enclosing_fqn, out);
            return;
        }
        "import_statement" => {
            if let Some(import) = extract_import(node, bytes) {
                out.imports.push(import);
            }
            return;
        }
        "export_statement" => {
            // Re-export: `export { a } from "./m"` carries a `source` field.
            if node.child_by_field_name("source").is_some() {
                if let Some(import) = extract_reexport(node, bytes) {
                    out.imports.push(import);
                }
                // A re-export has no declaration to descend into.
                return;
            }
            // Otherwise fall through: `export function f(){}`,
            // `export default function f(){}`, `export class C {}`, etc.
            // The declaration child is walked normally below.
        }
        "object" => {
            // An object literal MAY be an Apollo-style resolver map
            // (`{ Query: { field: handler } }`). Extracted additively and
            // conservatively (M2); a non-resolver object yields nothing. We still
            // descend so nested calls/functions inside are walked normally.
            extract_resolver_entries(node, bytes, enclosing_fqn, out);
            walk_children(node, bytes, container, enclosing_fqn, out);
            return;
        }
        "call_expression" => {
            // `require("./m")` is recorded as an import, not a call.
            if let Some(import) = extract_require(node, bytes) {
                out.imports.push(import);
            } else if let Some(call) = extract_call(node, bytes, enclosing_fqn) {
                out.calls.push(call);
            }
            // A route declaration (`app.get("/x", h)`) is recorded *additively* —
            // it does not replace the ordinary `obj.method()` CallRef above, so
            // existing call extraction is unchanged.
            let is_route = if let Some(route) = extract_route(node, bytes, enclosing_fqn) {
                out.routes.push(route);
                true
            } else {
                false
            };
            // An outgoing HTTP call (`fetch(...)`, `axios.get(...)`) is also
            // recorded additively (consumer linking, M3). Same node, separate
            // record — the ordinary CallRef above is unaffected.
            //
            // A route registration is NOT an outgoing request: `app.get("/x", h)`
            // and `axios.get("/x")` share the `recv.get(string, …)` shape, so a
            // node already recognised as a *route* (server-side producer) must not
            // ALSO be read as a *consumer* HTTP call — that would invent a
            // confident-wrong CONSUMES edge from the route file to the operation it
            // produces (R1/R5). Routes short-circuit HTTP-call extraction.
            if !is_route {
                if let Some(http) = extract_http_call(node, bytes, enclosing_fqn) {
                    out.http_calls.push(http);
                }
            }
            // A `gql`/`graphql` tagged-template document (consumer linking, M2).
            // A tagged template parses as a `call_expression` whose `function` is
            // the tag identifier and whose `arguments` field IS the template
            // string (the grammar labels it `arguments`). Recorded additively —
            // the bare `gql(...)` CallRef above is unaffected.
            if let Some(doc) = extract_gql_document(node, bytes, enclosing_fqn) {
                out.gql_documents.push(doc);
            }
            // Still descend (arguments may contain nested calls / functions).
            walk_children(node, bytes, container, enclosing_fqn, out);
            return;
        }
        // A string / template-string literal that looks like SQL → a data-plane
        // `SqlCandidate` (Slice 16, D3, M2). Recorded additively, then we STILL fall
        // through to `walk_children`: a `template_string`'s `${…}` substitution can
        // contain nested calls (`` `SELECT ${foo()}` ``), and those must still be
        // walked (the candidate itself is dropped as dynamic, but `foo()` is a real
        // call). A plain `string`'s fragment children match no arm, so re-walking
        // them is a harmless no-op.
        "string" | "template_string" => {
            if let Some(cand) = extract_sql_candidate(node, bytes, enclosing_fqn) {
                out.sql_candidates.push(cand);
            }
        }
        _ => {}
    }

    walk_children(node, bytes, container, enclosing_fqn, out);
}

/// Extract a [`SqlCandidate`] from a `string` / `template_string` node when its
/// literal text passes the cheap SQL-keyword prefilter
/// ([`looks_like_sql`](strata_core::looks_like_sql)).
///
/// A `template_string` with **any** `${…}` interpolation (a
/// `template_substitution` child) is **not** a single literal — that is dynamic
/// SQL, honestly dropped (we never guess a table from an interpolated query, R1/R5).
/// The inner text is reconstructed from the `string_fragment`/`escape_sequence`
/// children, so quotes/backticks are stripped exactly as the gql-document extractor
/// does.
fn extract_sql_candidate(node: Node, bytes: &[u8], enclosing_fqn: &str) -> Option<SqlCandidate> {
    let mut literal = String::new();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "string_fragment" | "escape_sequence" => literal.push_str(text(child, bytes)),
            // An interpolation makes this NOT a single literal → dynamic SQL, drop.
            "template_substitution" => return None,
            _ => {}
        }
    }
    if !strata_core::looks_like_sql(&literal) {
        return None;
    }
    Some(SqlCandidate {
        text: literal,
        enclosing_fqn: enclosing_fqn.to_string(),
        span: span_of(node),
    })
}

/// Walk every child of `node` with the given context.
fn walk_children(
    node: Node,
    bytes: &[u8],
    container: Option<&str>,
    enclosing_fqn: &str,
    out: &mut AnalyzedFile,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk(child, bytes, container, enclosing_fqn, out);
    }
}

/// Extract `const f = () => {}` / `const g = function(){}` style function
/// definitions from a (lexical|variable) declaration node. Each declarator
/// whose value is an arrow function or function expression yields a Function
/// symbol named after the binding; its body is walked with that name as the
/// enclosing scope. Non-function declarators are still walked for nested calls.
fn extract_const_functions(node: Node, bytes: &[u8], enclosing_fqn: &str, out: &mut AnalyzedFile) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() != "variable_declarator" {
            continue;
        }
        let name_node = child.child_by_field_name("name");
        let value = child.child_by_field_name("value");
        let is_fn = matches!(
            value.map(|v| v.kind()),
            Some("arrow_function") | Some("function_expression") | Some("function")
        );
        if let (Some(name_node), true) = (name_node, is_fn) {
            // Only bind a name for simple identifier patterns.
            if name_node.kind() == "identifier" {
                let name = text(name_node, bytes).to_string();
                let fqn = name.clone();
                out.symbols.push(RawSymbol {
                    kind: NodeKind::Function,
                    name,
                    fqn: fqn.clone(),
                    container_fqn: None,
                    // Span the whole declarator (binding through body) for a
                    // useful, stable range.
                    span: span_of(child),
                });
                if let Some(value) = value {
                    walk_children(value, bytes, None, &fqn, out);
                }
                continue;
            }
        }
        // An untagged GraphQL-document candidate: `const Q = `query … `` (a bare
        // template-string value, no tag, no substitutions, passing the prefilter).
        // Recorded additively — the declarator is still walked below for any nested
        // calls (a `template_substitution` would have bailed extraction anyway).
        if let Some(doc) = extract_untagged_gql_document(child, bytes, enclosing_fqn) {
            out.gql_documents.push(doc);
        }
        // Not a named function binding: walk the declarator for nested calls.
        walk(child, bytes, None, enclosing_fqn, out);
    }
}

/// Pull the module specifier out of a `source: (string ...)` field.
fn specifier_of(source: Node, bytes: &[u8]) -> Option<String> {
    // The string node has a string_fragment child holding the raw text.
    let mut cursor = source.walk();
    for child in source.children(&mut cursor) {
        if child.kind() == "string_fragment" {
            return Some(text(child, bytes).to_string());
        }
    }
    // Fallback: strip surrounding quotes from the raw string node.
    let raw = text(source, bytes);
    let trimmed = raw.trim_matches(|c| c == '"' || c == '\'' || c == '`');
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Extract a standard `import ... from "..."` (or side-effect `import "..."`).
fn extract_import(node: Node, bytes: &[u8]) -> Option<ImportRef> {
    let source = node.child_by_field_name("source")?;
    let specifier = specifier_of(source, bytes)?;
    let mut names = Vec::new();
    let mut name_spans = Vec::new();
    if let Some(clause) = find_child(node, "import_clause") {
        collect_import_names(clause, bytes, &mut names, &mut name_spans);
    }
    Some(ImportRef {
        specifier,
        imported_names: names,
        span: span_of(node),
        name_spans,
    })
}

/// Collect the bound names (and their identifier spans) from an `import_clause`:
/// - bare `identifier` child -> default import
/// - `named_imports` -> each `import_specifier`'s name (or alias)
/// - `namespace_import` -> the `* as ns` identifier
///
/// `names` and `spans` are kept strictly parallel: the span is the locally-bound
/// identifier's node range, which precise resolution targets.
fn collect_import_names(
    clause: Node,
    bytes: &[u8],
    names: &mut Vec<String>,
    spans: &mut Vec<Span>,
) {
    let mut cursor = clause.walk();
    for child in clause.children(&mut cursor) {
        match child.kind() {
            "identifier" => {
                names.push(text(child, bytes).to_string());
                spans.push(span_of(child));
            }
            "namespace_import" => {
                if let Some(id) = find_child(child, "identifier") {
                    names.push(text(id, bytes).to_string());
                    spans.push(span_of(id));
                }
            }
            "named_imports" => {
                let mut inner = child.walk();
                for spec in child.children(&mut inner) {
                    if spec.kind() == "import_specifier" {
                        let (name, span) = import_specifier_name(spec, bytes);
                        names.push(name);
                        spans.push(span);
                    }
                }
            }
            _ => {}
        }
    }
}

/// The locally-bound name of an `import_specifier` and its identifier span: the
/// `alias` if present (`a as b` -> "b"), else its `name`.
fn import_specifier_name(spec: Node, bytes: &[u8]) -> (String, Span) {
    if let Some(alias) = spec.child_by_field_name("alias") {
        return (text(alias, bytes).to_string(), span_of(alias));
    }
    if let Some(name) = spec.child_by_field_name("name") {
        return (text(name, bytes).to_string(), span_of(name));
    }
    (text(spec, bytes).to_string(), span_of(spec))
}

/// Extract a re-export `export { a } from "./m"` -> specifier "./m", names ["a"].
///
/// The recorded span per name is the *original* (`name`) identifier inside the
/// export clause, which is where SCIP places the occurrence resolving to the
/// re-exported symbol's definition.
fn extract_reexport(node: Node, bytes: &[u8]) -> Option<ImportRef> {
    let source = node.child_by_field_name("source")?;
    let specifier = specifier_of(source, bytes)?;
    let mut names = Vec::new();
    let mut name_spans = Vec::new();
    if let Some(clause) = find_child(node, "export_clause") {
        let mut cursor = clause.walk();
        for spec in clause.children(&mut cursor) {
            if spec.kind() == "export_specifier" {
                if let Some(name) = spec.child_by_field_name("name") {
                    names.push(text(name, bytes).to_string());
                    name_spans.push(span_of(name));
                }
            }
        }
    }
    Some(ImportRef {
        specifier,
        imported_names: names,
        span: span_of(node),
        name_spans,
    })
}

/// Extract a call site: plain `foo()`, `obj.m()`, and `this.m()`.
/// `require(...)` is handled separately by `extract_require` before this runs.
///
/// `span` covers the whole call expression; `callee_span` pinpoints the callee
/// *identifier* (the `property` for member calls), which precise resolution
/// targets — for `obj.method()` the SCIP occurrence is on `method`, not `obj`.
fn extract_call(node: Node, bytes: &[u8], enclosing_fqn: &str) -> Option<CallRef> {
    let function = node.child_by_field_name("function")?;
    match function.kind() {
        "identifier" => {
            let callee = text(function, bytes).to_string();
            Some(CallRef {
                callee_name: callee,
                receiver: None,
                enclosing_fqn: enclosing_fqn.to_string(),
                span: span_of(node),
                callee_span: span_of(function),
                // A bare call has no receiver; the path/field discriminator is moot.
                receiver_is_path: false,
            })
        }
        "member_expression" => {
            let property = function.child_by_field_name("property")?;
            let callee = text(property, bytes).to_string();
            let object = function.child_by_field_name("object");
            let receiver = object.map(|o| match o.kind() {
                "this" => "this".to_string(),
                _ => text(o, bytes).to_string(),
            });
            Some(CallRef {
                callee_name: callee,
                receiver,
                enclosing_fqn: enclosing_fqn.to_string(),
                span: span_of(node),
                callee_span: span_of(property),
                // TS `.` is overloaded: `Type.method()` (static) and `obj.method()`
                // (instance) are syntactically identical member_expressions, so we
                // cannot tell a type qualifier from a value receiver without
                // receiver-type inference. Always `false` (a field receiver);
                // separating static calls is deferred to receiver inference (A3).
                receiver_is_path: false,
            })
        }
        _ => None,
    }
}

/// Extract a web-framework route declaration from a `call_expression`.
///
/// Recognised shape: `<recv>.<method>("<string-literal>", …)` where `<method>`
/// is in [`ROUTE_METHODS`] AND the **first argument is a string literal**. Both
/// conditions are required — this is the false-positive guard: a call like
/// `cache.get(key)` (non-literal first arg) or `svc.fetch("/x")` (method not in
/// the verb set) is NOT a route. Being conservative, a missed route just yields
/// no `PRODUCES` edge later, whereas a spurious route would invent one.
///
/// `handler_name` is the 2nd-or-later argument's identifier when it is a plain
/// identifier (a named handler reference); an inline function/arrow handler, or
/// no handler argument, yields `None`.
fn extract_route(node: Node, bytes: &[u8], enclosing_fqn: &str) -> Option<RouteDecl> {
    let function = node.child_by_field_name("function")?;
    if function.kind() != "member_expression" {
        return None;
    }
    let property = function.child_by_field_name("property")?;
    // The method name must be a bare identifier in the verb set.
    if property.kind() != "property_identifier" && property.kind() != "identifier" {
        return None;
    }
    let method = text(property, bytes);
    if !ROUTE_METHODS.contains(&method) {
        return None;
    }

    // Disambiguation guard: a known HTTP-*client* receiver (`axios.get(...)`)
    // shares the `recv.<verb>(string, …)` shape with a route registration
    // (`app.get(...)`), but it is an outgoing *request*, not a route. Exclude it
    // here so it flows to `extract_http_call` instead — and so the same node is
    // never recorded as BOTH a producer route and a consumer call. The receiver
    // is the only reliable static signal (`axios` vs `app`/`router`).
    if let Some(object) = function.child_by_field_name("object") {
        if object.kind() == "identifier" && HTTP_CLIENT_OBJECTS.contains(&text(object, bytes)) {
            return None;
        }
    }

    let args = node.child_by_field_name("arguments")?;
    let named: Vec<Node> = named_args(args);
    // Guard: the first argument must be a string literal path.
    let first = named.first()?;
    if first.kind() != "string" {
        return None;
    }
    let path = specifier_of(*first, bytes)?;

    // Handler: the next plain-identifier argument, if any.
    let handler_name = named
        .get(1)
        .filter(|n| n.kind() == "identifier")
        .map(|n| text(*n, bytes).to_string());

    Some(RouteDecl {
        method: method.to_ascii_uppercase(),
        path,
        handler_name,
        enclosing_fqn: enclosing_fqn.to_string(),
        span: span_of(node),
    })
}

/// The decorator names that mark a TypeORM entity class. A class decorated with
/// exactly `@Entity(...)` whose first argument is a string literal declares an
/// explicit table name; any other decorator (`@Component`, `@Injectable`, …) is NOT
/// an ORM model — the conservative false-positive guard (a missed model is a missed
/// link, never an invented one).
const ORM_ENTITY_DECORATORS: [&str; 1] = ["Entity"];

/// Extract an [`OrmModelHint`] from a TS `class_declaration` when it (or its
/// enclosing `export_statement`) carries an `@Entity("<table>")` decorator with an
/// explicit first string-literal argument (Slice 25, D3, M2b — TypeORM). A bare
/// `@Entity()` (no string arg, relying on the class-name convention) or a non-`Entity`
/// decorator yields `None`: only an explicit literal name is captured (convention
/// inference deferred; dynamic args never guessed, R1/R5). `class_fqn` is the
/// decorated class's fqn.
///
/// The grammar attaches `decorator` children to the `class_declaration` for a bare
/// `@Entity()\nclass X`, but **hoists them to the enclosing `export_statement`** for
/// the common `@Entity()\nexport class X` form. So we scan the class node's
/// decorators first, then (if none matched) its parent when that parent is an
/// `export_statement`.
fn extract_orm_model_hint(node: Node, bytes: &[u8], class_fqn: &str) -> Option<OrmModelHint> {
    // Decorators directly on the class (`@Entity()\nclass X`).
    if let Some(table) = entity_table_from_decorators(node, bytes) {
        return Some(OrmModelHint {
            model_fqn: class_fqn.to_string(),
            table_name: table,
            framework: OrmFramework::TypeOrm,
            span: span_of(node),
        });
    }
    // Decorators hoisted to the enclosing `export_statement`
    // (`@Entity()\nexport class X`).
    if let Some(parent) = node.parent() {
        if parent.kind() == "export_statement" {
            if let Some(table) = entity_table_from_decorators(parent, bytes) {
                return Some(OrmModelHint {
                    model_fqn: class_fqn.to_string(),
                    table_name: table,
                    framework: OrmFramework::TypeOrm,
                    span: span_of(node),
                });
            }
        }
    }
    None
}

/// Scan `node`'s direct `decorator` children for an `@Entity("<table>")` with an
/// explicit first string-literal argument; return the unquoted table name. `None`
/// when no `Entity` decorator is present, or its first argument is not a string
/// literal (a bare `@Entity()` or `@Entity(options)` → no explicit name, never
/// guessed). Shared by the class-node and export-statement scans.
fn entity_table_from_decorators(node: Node, bytes: &[u8]) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() != "decorator" {
            continue;
        }
        // A decorator that is a *call* (`@Entity(...)`); a bare `@Foo` (no call) has no
        // arguments and is not an explicit-name entity.
        let Some(call) = child.named_child(0) else {
            continue;
        };
        if call.kind() != "call_expression" {
            continue;
        }
        let Some(func) = call.child_by_field_name("function") else {
            continue;
        };
        if func.kind() != "identifier" || !ORM_ENTITY_DECORATORS.contains(&text(func, bytes)) {
            continue;
        }
        let Some(args) = call.child_by_field_name("arguments") else {
            continue;
        };
        // The first argument must be a string literal (the explicit table name); a
        // bare `@Entity()` (no args) or `@Entity(options)` (non-string first arg) is
        // not an explicit-name mapping → no hint.
        let first = named_args(args).into_iter().next()?;
        if first.kind() != "string" {
            return None;
        }
        return specifier_of(first, bytes);
    }
    None
}

/// The named (non-punctuation) children of an `arguments` node, in order.
fn named_args<'a>(args: Node<'a>) -> Vec<Node<'a>> {
    let mut cursor = args.walk();
    args.children(&mut cursor)
        .filter(|c| c.is_named())
        .collect()
}

/// HTTP verbs an `axios.<verb>(...)` (or `client.<verb>(...)`) call may use,
/// lower-case as written. Mirrors the route verbs minus `all` (no `axios.all`
/// request verb in this sense). A member call whose property is outside this set
/// is not treated as an HTTP request — the same conservative false-positive
/// guard the route extractor uses.
const HTTP_CLIENT_VERBS: [&str; 7] = ["get", "post", "put", "delete", "patch", "head", "options"];

/// Receiver identifiers that denote an HTTP *client* (an outgoing request), used
/// to disambiguate `axios.get("/x")` (a consumer call) from `app.get("/x", h)`
/// (a server route) which share the `recv.<verb>(string, …)` shape. Deliberately
/// minimal: only unambiguous global client names — package default-imports that
/// are never a server route receiver (`app`/`router`). A custom client instance is
/// not listed (it would need type info to tell from `app`); such a call is simply
/// not extracted as an HTTP call rather than risk misreading a route as a request.
/// (`node-fetch` needs no entry: it is imported AS `fetch`, which the bare-call
/// branch already handles.)
const HTTP_CLIENT_OBJECTS: [&str; 4] = ["axios", "got", "ky", "superagent"];

/// Extract an outgoing HTTP call from a `call_expression`.
///
/// Recognised shapes (consumer linking, M3):
/// - `fetch(url, opts?)` — method from a string-literal `opts.method`, else GET.
/// - `axios.<verb>(url, …)` / `client.<verb>(url, …)` where `<verb>` is in
///   [`HTTP_CLIENT_VERBS`] — method is the verb; first arg is the URL.
/// - `axios(config)` — method/url read from an object-literal `config`'s
///   `method`/`url` properties.
///
/// `url` is classified by [`url_shape`]: a string literal → `Literal`, a template
/// string → `Template` (interpolations canonicalised to `{}`), anything else →
/// `Dynamic`. Returns `None` when the call is not a recognised HTTP shape.
fn extract_http_call(node: Node, bytes: &[u8], enclosing_fqn: &str) -> Option<HttpCall> {
    let function = node.child_by_field_name("function")?;
    let args = node.child_by_field_name("arguments")?;
    let named = named_args(args);

    match function.kind() {
        // Bare call: `fetch(...)`/`got(...)`/`ky(...)` or `axios(config)`.
        "identifier" => {
            let name = text(function, bytes);
            match name {
                // got/ky's primary call form is `client(url, opts?)` with the same
                // semantics as bare fetch: method from a literal `opts.method`,
                // else GET.
                "fetch" | "got" | "ky" => {
                    let url = url_shape(*named.first()?, bytes);
                    // method from opts.method literal (2nd arg object), else GET.
                    let method = named
                        .get(1)
                        .and_then(|o| object_string_prop(*o, bytes, "method"))
                        .map(|m| m.to_ascii_uppercase())
                        .or_else(|| Some("GET".to_string()));
                    Some(HttpCall {
                        method,
                        url,
                        enclosing_fqn: enclosing_fqn.to_string(),
                        span: span_of(node),
                    })
                }
                "axios" => {
                    // axios(config): read url + method off the config object.
                    let config = named.first()?;
                    let url = object_url_prop(*config, bytes)?;
                    let method = object_string_prop(*config, bytes, "method")
                        .map(|m| m.to_ascii_uppercase())
                        .or_else(|| Some("GET".to_string()));
                    Some(HttpCall {
                        method,
                        url,
                        enclosing_fqn: enclosing_fqn.to_string(),
                        span: span_of(node),
                    })
                }
                _ => None,
            }
        }
        // Member call: `axios.get(url, …)` / `client.post(url, …)`.
        "member_expression" => {
            let property = function.child_by_field_name("property")?;
            if property.kind() != "property_identifier" && property.kind() != "identifier" {
                return None;
            }
            let verb = text(property, bytes);
            if !HTTP_CLIENT_VERBS.contains(&verb) {
                return None;
            }
            let url = url_shape(*named.first()?, bytes);
            Some(HttpCall {
                method: Some(verb.to_ascii_uppercase()),
                url,
                enclosing_fqn: enclosing_fqn.to_string(),
                span: span_of(node),
            })
        }
        _ => None,
    }
}

/// The tag identifiers that mark a GraphQL document tagged template. A template
/// tagged with exactly `gql` or `graphql` is a GraphQL document; any other tag
/// (`css`, `styled`, `html`, …) is NOT — the conservative false-positive guard.
const GQL_TAG_NAMES: [&str; 2] = ["gql", "graphql"];

/// Extract a `gql`/`graphql` tagged-template document from a `call_expression`.
///
/// A tagged template `` gql`query { … }` `` parses as a `call_expression` whose
/// `function` field is the tag identifier and whose `arguments` field is the
/// `template_string` itself (the tree-sitter grammar labels the template as the
/// call's `arguments`). We require:
/// - the `function` is a bare `identifier` in [`GQL_TAG_NAMES`], AND
/// - the `arguments` field is a `template_string`.
///
/// Both conditions guard against false positives: a `css`/`styled` tag, or a
/// plain `gql(variable)` call, is not a document. A template with **no**
/// interpolations captures its joined literal text and is `interpolation_free`;
/// a template **with** any `${…}` is recorded `interpolation_free: false` with
/// empty text — counted but never parsed/linked (design R1/R5).
fn extract_gql_document(node: Node, bytes: &[u8], enclosing_fqn: &str) -> Option<GqlDocument> {
    let function = node.child_by_field_name("function")?;
    if function.kind() != "identifier" {
        return None;
    }
    if !GQL_TAG_NAMES.contains(&text(function, bytes)) {
        return None;
    }
    // The `arguments` field of a tagged template is the template string node.
    let template = node.child_by_field_name("arguments")?;
    if template.kind() != "template_string" {
        return None;
    }

    // Scan the template's children: any `template_substitution` means it is
    // interpolated (unreliable text); otherwise join the literal fragments.
    let mut cursor = template.walk();
    let mut interpolation_free = true;
    let mut literal = String::new();
    for child in template.children(&mut cursor) {
        match child.kind() {
            "string_fragment" => literal.push_str(text(child, bytes)),
            "escape_sequence" => literal.push_str(text(child, bytes)),
            "template_substitution" => interpolation_free = false,
            _ => {} // backticks and other delimiters are ignored
        }
    }

    Some(GqlDocument {
        // An interpolated template's text is unreliable; store nothing for it so a
        // downstream consumer cannot accidentally parse a partial document.
        text: if interpolation_free {
            literal
        } else {
            String::new()
        },
        interpolation_free,
        // An explicit `gql`/`graphql` tag: a parse failure is an honest miss
        // (counted in coverage's `unparsed_documents`).
        tagged: true,
        enclosing_fqn: enclosing_fqn.to_string(),
        span: span_of(node),
    })
}

/// The keyword/punctuation an untagged template-literal constant must start with
/// (after trimming) to be a GraphQL-document *candidate*. A cheap prefilter that
/// keeps css/sql/html/path strings out of the link pipeline without parsing them:
/// only a string opening with an operation/fragment keyword or a bare selection
/// set `{` is forwarded. False negatives (an exotic document that does not start
/// this way) are acceptable — the link is parse-gated regardless, so the cost is
/// a missed candidate, never an invented edge.
const GQL_UNTAGGED_PREFIXES: [&str; 5] = ["query", "mutation", "subscription", "fragment", "{"];

/// Whether `text` (a template constant's literal body) looks like a GraphQL
/// document cheaply enough to forward as an untagged candidate. Trims leading
/// whitespace and matches against [`GQL_UNTAGGED_PREFIXES`]. A keyword prefix must
/// be followed by a non-identifier char (or end) so `queryClient`/`mutationFn`
/// substrings do not slip through.
fn looks_like_untagged_gql(text: &str) -> bool {
    let trimmed = text.trim_start();
    for prefix in GQL_UNTAGGED_PREFIXES {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            // `{` is a self-delimiting selection set; the keywords must end on a
            // word boundary so `queryString` is not read as `query …`.
            if prefix == "{" || rest.chars().next().is_none_or(|c| !is_ident_char(c)) {
                return true;
            }
        }
    }
    false
}

/// Whether `c` can appear in a JS/GraphQL identifier (for the keyword-boundary
/// check in [`looks_like_untagged_gql`]).
fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Extract an *untagged* GraphQL-document candidate from a `variable_declarator`
/// whose value is a bare `template_string` (no tag) with **no** substitutions —
/// the dominant AppSync/Amplify constant style
/// (`` const GET_X = `query GetX { getX { id } }` ``). Gated by the cheap
/// [`looks_like_untagged_gql`] prefilter so css/sql/html constants never enter the
/// pipeline; a template *with* `${…}` substitutions is not emitted at all (its
/// text is unreliable and an untagged interpolated string is far more likely a
/// non-GraphQL computed literal). The candidate is parse-gated at link time, so
/// `tagged: false` — a parse failure is silently dropped, never counted.
fn extract_untagged_gql_document(
    declarator: Node,
    bytes: &[u8],
    enclosing_fqn: &str,
) -> Option<GqlDocument> {
    let value = declarator.child_by_field_name("value")?;
    if value.kind() != "template_string" {
        return None;
    }
    // Join the literal fragments; bail on any substitution (untagged interpolated
    // templates are not emitted — text is unreliable, R1/R5).
    let mut cursor = value.walk();
    let mut literal = String::new();
    for child in value.children(&mut cursor) {
        match child.kind() {
            "string_fragment" | "escape_sequence" => literal.push_str(text(child, bytes)),
            "template_substitution" => return None,
            _ => {} // backticks / delimiters
        }
    }
    if !looks_like_untagged_gql(&literal) {
        return None; // css/sql/html/path constant — keep it out of the pipeline.
    }
    Some(GqlDocument {
        text: literal,
        // No substitutions reached here, so the text is always reliable.
        interpolation_free: true,
        // A *candidate*, not an explicit GraphQL surface: parse-gated at link
        // time, and a parse failure is silently skipped (never counted).
        tagged: false,
        enclosing_fqn: enclosing_fqn.to_string(),
        span: span_of(declarator),
    })
}

/// The literal outer keys an Apollo-style resolver map uses. Extraction is
/// conservative to exactly these three (spec §3); a `schema {}`-renamed root is a
/// deferred, surfaced miss — never guessed.
const RESOLVER_ROOT_KEYS: [&str; 3] = ["Query", "Mutation", "Subscription"];

/// Extract Apollo-style resolver-map entries from an `object` literal.
///
/// Recognised shape: an outer property whose key is *literally* `Query`,
/// `Mutation`, or `Subscription` and whose value is an `object` literal. Each
/// inner property of that object whose value is a function/arrow/identifier
/// becomes one [`ResolverEntry`]:
/// - `getUser` (shorthand) → `handler_name = Some("getUser")`,
/// - `getUser: getUser` (identifier value) → `handler_name = Some("getUser")`,
/// - `getUser: () => …` / `getUser: function …` (inline) → `handler_name = None`.
///
/// An inner value that is **not** a function/arrow/identifier (a nested object, a
/// scalar like `{ Query: { timeout: 30 } }`) yields **no** entry — the
/// false-positive guard that keeps a plain config object from being misread as a
/// resolver map (design R1/R5; spec §3). Only the immediate object is inspected;
/// nested objects are walked separately by [`walk`].
fn extract_resolver_entries(node: Node, bytes: &[u8], enclosing_fqn: &str, out: &mut AnalyzedFile) {
    let mut cursor = node.walk();
    for pair in node.children(&mut cursor) {
        if pair.kind() != "pair" {
            continue;
        }
        let Some(key) = pair.child_by_field_name("key") else {
            continue;
        };
        // The outer key must be a literal identifier/string in the root-key set.
        let op_type = match property_key_name(key, bytes) {
            Some(name) if RESOLVER_ROOT_KEYS.contains(&name.as_str()) => name,
            _ => continue,
        };
        let Some(value) = pair.child_by_field_name("value") else {
            continue;
        };
        if value.kind() != "object" {
            continue; // a root key whose value is not an object literal → skip
        }
        collect_resolver_fields(value, bytes, &op_type, enclosing_fqn, out);
    }
}

/// For one root-type object (`{ field: handler, … }`), push a [`ResolverEntry`]
/// per inner property whose value is a function/arrow/identifier.
fn collect_resolver_fields(
    obj: Node,
    bytes: &[u8],
    op_type: &str,
    enclosing_fqn: &str,
    out: &mut AnalyzedFile,
) {
    let mut cursor = obj.walk();
    for member in obj.children(&mut cursor) {
        match member.kind() {
            // `{ getUser }` shorthand → field == handler name (an identifier).
            "shorthand_property_identifier" => {
                let field = text(member, bytes).to_string();
                out.resolver_entries.push(ResolverEntry {
                    op_type: op_type.to_string(),
                    field: field.clone(),
                    handler_name: Some(field),
                    enclosing_fqn: enclosing_fqn.to_string(),
                    span: span_of(member),
                });
            }
            // `{ field: <value> }` → emit only when <value> is a function form.
            "pair" => {
                let Some(key) = member.child_by_field_name("key") else {
                    continue;
                };
                let Some(field) = property_key_name(key, bytes) else {
                    continue;
                };
                let Some(value) = member.child_by_field_name("value") else {
                    continue;
                };
                let handler_name = match value.kind() {
                    // Inline function/arrow handler → no name.
                    "arrow_function" | "function_expression" | "function" => None,
                    // An identifier value names the handler (`getUser: getUser`).
                    "identifier" => Some(text(value, bytes).to_string()),
                    // Anything else (nested object, scalar, call, …) is NOT a
                    // resolver field → skip (the false-positive guard).
                    _ => continue,
                };
                out.resolver_entries.push(ResolverEntry {
                    op_type: op_type.to_string(),
                    field,
                    handler_name,
                    enclosing_fqn: enclosing_fqn.to_string(),
                    span: span_of(member),
                });
            }
            _ => {}
        }
    }
}

/// The textual name of a property key node: a bare `property_identifier`/
/// `identifier`, or the contents of a `string` key. `None` for a computed key.
fn property_key_name(key: Node, bytes: &[u8]) -> Option<String> {
    match key.kind() {
        "property_identifier" | "identifier" => Some(text(key, bytes).to_string()),
        "string" => specifier_of(key, bytes),
        _ => None,
    }
}

/// Classify a URL argument node into a [`UrlShape`].
///
/// - `string` literal → `Literal(<contents>)`.
/// - `template_string` → `Template(<path with each `${…}` replaced by `{}`>)`.
/// - anything else (identifier, `binary_expression` concat, call, …) → `Dynamic`.
fn url_shape(node: Node, bytes: &[u8]) -> UrlShape {
    match node.kind() {
        "string" => match specifier_of(node, bytes) {
            Some(s) => UrlShape::Literal(s),
            None => UrlShape::Dynamic,
        },
        "template_string" => UrlShape::Template(template_to_pattern(node, bytes)),
        _ => UrlShape::Dynamic,
    }
}

/// Reconstruct a `template_string` into a path pattern by replacing every
/// `${…}` interpolation with `{}` and keeping the literal text between them.
/// `` `/users/${id}/posts` `` → `"/users/{}/posts"`.
fn template_to_pattern(node: Node, bytes: &[u8]) -> String {
    let mut out = String::new();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            // The literal chunks of the template carry their text directly.
            "string_fragment" => out.push_str(text(child, bytes)),
            // An interpolation `${…}` collapses to the canonical placeholder.
            "template_substitution" => out.push_str("{}"),
            // Backticks and escape sequences: ignore the delimiters; copy an
            // escape's text so e.g. a literal `\n` is preserved verbatim.
            "escape_sequence" => out.push_str(text(child, bytes)),
            _ => {}
        }
    }
    out
}

/// The string-literal value of property `key` on an object-literal node, e.g.
/// `{ method: "POST" }` with `key = "method"` → `Some("POST")`. Returns `None`
/// when the node is not an object, the key is absent, or its value is not a plain
/// string literal (so a computed method is reported as unknown, not assumed).
fn object_string_prop<'a>(node: Node<'a>, bytes: &'a [u8], key: &str) -> Option<String> {
    if node.kind() != "object" {
        return None;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() != "pair" {
            continue;
        }
        let k = child.child_by_field_name("key")?;
        let k_name = match k.kind() {
            "property_identifier" | "identifier" => text(k, bytes).to_string(),
            "string" => specifier_of(k, bytes).unwrap_or_default(),
            _ => continue,
        };
        if k_name != key {
            continue;
        }
        let v = child.child_by_field_name("value")?;
        if v.kind() == "string" {
            return specifier_of(v, bytes);
        }
        return None;
    }
    None
}

/// The [`UrlShape`] of an object-literal's `url` property (for `axios(config)`).
/// `{ url: "/users", method: "GET" }` → `Some(Literal("/users"))`. Returns `None`
/// when there is no `url` property (not an HTTP call we can attribute a path to).
fn object_url_prop(node: Node, bytes: &[u8]) -> Option<UrlShape> {
    if node.kind() != "object" {
        return None;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() != "pair" {
            continue;
        }
        let Some(k) = child.child_by_field_name("key") else {
            continue;
        };
        let k_name = match k.kind() {
            "property_identifier" | "identifier" => text(k, bytes).to_string(),
            "string" => specifier_of(k, bytes).unwrap_or_default(),
            _ => continue,
        };
        if k_name != "url" {
            continue;
        }
        if let Some(v) = child.child_by_field_name("value") {
            return Some(url_shape(v, bytes));
        }
    }
    None
}

/// First direct child of a given kind.
fn find_child<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
    let mut cursor = node.walk();
    let found = node.children(&mut cursor).find(|c| c.kind() == kind);
    found
}

/// If `node` is a `require("...")` call, capture it as an import (specifier
/// only, no imported names — the brief keys `require` by its specifier).
fn extract_require(node: Node, bytes: &[u8]) -> Option<ImportRef> {
    let func = node.child_by_field_name("function")?;
    if func.kind() != "identifier" || text(func, bytes) != "require" {
        return None;
    }
    let args = node.child_by_field_name("arguments")?;
    let specifier = first_string_arg(args, bytes)?;
    Some(ImportRef {
        specifier,
        imported_names: Vec::new(),
        span: span_of(node),
        name_spans: Vec::new(),
    })
}

/// The first string literal argument inside an `arguments` node.
fn first_string_arg(args: Node, bytes: &[u8]) -> Option<String> {
    let mut cursor = args.walk();
    for child in args.children(&mut cursor) {
        if child.kind() == "string" {
            return specifier_of(child, bytes);
        }
    }
    None
}
