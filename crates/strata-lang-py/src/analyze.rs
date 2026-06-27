//! Pure per-file extraction of symbols, imports, and intra-file calls.
//!
//! Operates on a Python source string via Tree-sitter; performs no IO. Like the
//! TS adapter, extraction is a manual cursor/recursive walk over the parse tree
//! (no Tree-sitter queries), which keeps the direct dependency set minimal and
//! gives precise control over enclosing-scope tracking for call sites.
//!
//! Fills the code-plane fields of [`AnalyzedFile`] (`symbols`/`imports`/`calls`),
//! the data-plane signals (`sql_candidates`/`orm_models`), and the contract-plane
//! signals from Python web frameworks:
//! - producer **routes** (`routes`) — Flask/FastAPI decorators, Django URLconf;
//! - REST consumer **calls** (`http_calls`) — `requests`/`httpx`;
//! - GraphQL consumer **documents** (`gql_documents`) — `gql("…")`;
//! - GraphQL producer **resolvers** (`resolver_entries`) — Graphene, Strawberry,
//!   Ariadne.
//!
//! The rule holds throughout: a missed signal is acceptable degradation, an
//! invented one is not (R1/R5).

use strata_core::{
    AnalyzedFile, CallRef, GqlDocument, HttpCall, ImportRef, NodeKind, OrmFramework, OrmModelHint,
    RawSymbol, ResolverEntry, RouteDecl, Span, SqlCandidate, UrlShape, ROUTE_METHOD_ANY,
};
use tree_sitter::{Node, Parser};

/// HTTP verb decorators recognised as web-framework route registrations on an
/// object receiver: Flask 2 `@app.get(...)`, FastAPI `@app.post(...)` /
/// `@router.get(...)`. Conservative false-positive guard: only these exact verbs
/// (`@app.middleware`, `@functools.wraps` are not routes). `@app.route(...)` (Flask
/// 1, with a `methods=` kwarg) is handled separately.
const ROUTE_VERBS: [&str; 7] = ["get", "post", "put", "delete", "patch", "head", "options"];

/// Django URLconf route-registration functions: `path("…", view)`, `re_path(r"…",
/// view)`, and the legacy `url(r"…", view)`. Each maps a URL pattern to a view that
/// dispatches HTTP methods internally, so the resulting route is method-less
/// ([`ROUTE_METHOD_ANY`]).
const DJANGO_ROUTE_FNS: [&str; 3] = ["path", "re_path", "url"];

/// Receiver identifiers that denote an outgoing-HTTP *client*: a call
/// `requests.get(...)` / `httpx.post(...)` is a consumer request. Deliberately
/// minimal — only the unambiguous module globals. A bare `client.get(...)` /
/// `session.get(...)` is NOT matched: Python `.get()` is ubiquitous (`dict.get`,
/// `os.environ.get`, a cache), so requiring a known client receiver avoids a flood
/// of false consumer links (the conservative cost is a missed call, never an
/// invented one — mirrors the TS adapter's `axios`-only rule).
const HTTP_CLIENT_OBJECTS: [&str; 2] = ["requests", "httpx"];

/// HTTP verbs a `requests.<verb>(url)` / `httpx.<verb>(url)` call may use. The
/// generic `requests.request(method, url)` form is handled separately (its method
/// is the first string argument).
const HTTP_CLIENT_VERBS: [&str; 7] = ["get", "post", "put", "delete", "patch", "head", "options"];

/// The three GraphQL root operation types. A Python GraphQL *server* resolver is
/// attributed to one of these. Used to gate resolver-host detection — a Graphene
/// or Strawberry class named exactly `Query`/`Mutation`/`Subscription` (the
/// conventional root names; a differently-named root is an honest miss, never
/// guessed).
const GRAPHQL_OP_TYPES: [&str; 3] = ["Query", "Mutation", "Subscription"];

/// Analyze a single Python source file.
///
/// Pure: no filesystem access. Returns whatever can be extracted; on a parse
/// failure (grammar load) returns an empty `AnalyzedFile` rather than panicking.
pub fn analyze(_path: &str, source: &str) -> AnalyzedFile {
    let mut parser = Parser::new();
    let lang: tree_sitter::Language = tree_sitter_python::LANGUAGE.into();
    if parser.set_language(&lang).is_err() {
        return AnalyzedFile::default();
    }
    let Some(tree) = parser.parse(source, None) else {
        return AnalyzedFile::default();
    };

    let bytes = source.as_bytes();
    let mut out = AnalyzedFile::default();
    walk(tree.root_node(), bytes, None, "", &mut out);
    out
}

/// Convert a Tree-sitter node's range into a core `Span` (1-based lines,
/// 0-based columns — matching the workspace convention).
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

/// Recursive walk. `container` is the enclosing class fqn for member symbols
/// (`Some` only directly inside a `class_definition` body); `enclosing_fqn` is
/// the nearest enclosing function/method fqn for call sites (empty string at
/// module top level).
fn walk(
    node: Node,
    bytes: &[u8],
    container: Option<&str>,
    enclosing_fqn: &str,
    out: &mut AnalyzedFile,
) {
    match node.kind() {
        // `decorated_definition` wraps a `decorator`+ and a `definition:` child
        // (the real function/class). Decorators are metadata; recurse into the
        // wrapped definition with the SAME context (so a decorated method is
        // still a member of its class), and skip re-walking the decorator
        // expressions as ordinary code — a decorator like `@app.route('/x')` is
        // not a call site of the function body.
        "decorated_definition" => {
            if let Some(def) = node.child_by_field_name("definition") {
                // Decorators carry contract signals we extract before recursing
                // (they are otherwise metadata, not call sites of the body):
                // - a decorated FUNCTION may be a web-framework route handler
                //   (Flask `@app.route`/`@app.get`, FastAPI `@router.post`) or an
                //   Ariadne GraphQL resolver (`@query.field("x")`);
                // - a decorated CLASS may be a Strawberry GraphQL type
                //   (`@strawberry.type class Query`) whose `@strawberry.field`
                //   methods are resolvers.
                match def.kind() {
                    "function_definition" => {
                        extract_decorator_routes(node, def, container, bytes, out);
                        extract_ariadne_resolver(node, def, container, bytes, out);
                    }
                    "class_definition" => {
                        extract_strawberry_resolvers(node, def, bytes, out);
                    }
                    _ => {}
                }
                walk(def, bytes, container, enclosing_fqn, out);
            }
            return;
        }
        "function_definition" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = text(name_node, bytes).to_string();
                // A `function_definition` whose container is a class body is a
                // Method; otherwise (module level OR nested in another function)
                // it is a Function. `async def` is the same node kind — no
                // special-casing.
                let (kind, fqn) = match container {
                    Some(class_fqn) if !class_fqn.is_empty() => {
                        (NodeKind::Method, make_fqn(container, &name))
                    }
                    _ => (NodeKind::Function, name.clone()),
                };
                out.symbols.push(RawSymbol {
                    kind,
                    name,
                    fqn: fqn.clone(),
                    container_fqn: if kind == NodeKind::Method {
                        container.map(str::to_string)
                    } else {
                        None
                    },
                    span: span_of(node),
                });
                // Descend into the body. The enclosing scope becomes this
                // function's fqn; the container resets to None so a NESTED def is
                // a plain Function (a closure is not a class member) and calls in
                // the body attribute to this function.
                if let Some(body) = node.child_by_field_name("body") {
                    walk_children(body, bytes, None, &fqn, out);
                }
                return;
            }
        }
        "class_definition" => {
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
                // ORM model hint (Slice 25, D3, M2b): an explicit table name declared
                // in the class body — SQLAlchemy `__tablename__ = "…"` or Django nested
                // `class Meta: db_table = "…"`. Captured additively before walking the
                // body; an explicit literal only (no convention, no dynamic — R1/R5).
                if let Some(hint) = extract_orm_model_hint(node, bytes, &fqn) {
                    out.orm_models.push(hint);
                }
                // Graphene GraphQL resolver host: an undecorated `class Query(
                // graphene.ObjectType)` whose `resolve_<field>` methods are
                // producers. (Strawberry's decorated class is handled in the
                // `decorated_definition` arm above.)
                extract_graphene_resolvers(node, &fqn, bytes, out);
                // Bases (`superclasses:`) are parsed for the class header but, like
                // the TS analyzer's inheritance, are NOT emitted into the frozen
                // `AnalyzedFile` model — no `Extends` edge is produced this slice
                // (deferred; see crate docs). Members are extracted with this class
                // as their container so methods get `Class.method` fqns.
                if let Some(body) = node.child_by_field_name("body") {
                    walk_children(body, bytes, Some(&fqn), enclosing_fqn, out);
                }
                return;
            }
        }
        "import_statement" => {
            extract_plain_imports(node, bytes, out);
            return;
        }
        "import_from_statement" => {
            if let Some(import) = extract_from_import(node, bytes) {
                out.imports.push(import);
            }
            return;
        }
        "call" => {
            if let Some(call) = extract_call(node, bytes, enclosing_fqn) {
                out.calls.push(call);
            }
            // A Django URLconf entry — `path("…", view)` / `re_path(…)` / `url(…)` —
            // is a method-less producer route (the view dispatches methods itself).
            if let Some(route) = extract_django_route(node, bytes, enclosing_fqn) {
                out.routes.push(route);
            }
            // An outgoing HTTP request — `requests.get(...)` / `httpx.post(...)` —
            // is a REST consumer call linked to the operation it hits.
            if let Some(http) = extract_http_call(node, bytes, enclosing_fqn) {
                out.http_calls.push(http);
            }
            // A `gql("…")` document is a GraphQL consumer signal.
            if let Some(doc) = extract_gql_document(node, bytes, enclosing_fqn) {
                out.gql_documents.push(doc);
            }
            // Still descend (arguments may contain nested calls / lambdas / comps).
            walk_children(node, bytes, container, enclosing_fqn, out);
            return;
        }
        // A string literal that looks like SQL → a data-plane `SqlCandidate`
        // (Slice 16, D3, M2). Recorded additively; we still fall through to
        // `walk_children` so an f-string's `interpolation` children (which can hold
        // nested calls) are walked — the candidate itself is dropped as dynamic.
        "string" => {
            if let Some(cand) = extract_sql_candidate(node, bytes, enclosing_fqn) {
                out.sql_candidates.push(cand);
            }
        }
        _ => {}
    }

    walk_children(node, bytes, container, enclosing_fqn, out);
}

/// Extract a [`SqlCandidate`] from a Python `string` node when its content passes
/// the cheap SQL-keyword prefilter ([`looks_like_sql`](strata_core::looks_like_sql)).
///
/// The grammar splits a string into `string_start` / `string_content` / `string_end`,
/// and an **f-string** carries `interpolation` (`{…}`) children. Any `interpolation`
/// child means this is NOT a single constant literal → dynamic SQL, honestly dropped
/// (we never guess a table from an interpolated query, R1/R5). The inner text is the
/// concatenation of the `string_content` children (covers `"…"`, `'…'`, and triple-
/// quoted `'''…'''` / `"""…"""`), so the quote/prefix tokens are excluded.
fn extract_sql_candidate(node: Node, bytes: &[u8], enclosing_fqn: &str) -> Option<SqlCandidate> {
    let mut literal = String::new();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "string_content" => literal.push_str(text(child, bytes)),
            // An f-string interpolation makes this NOT a single literal → drop.
            "interpolation" => return None,
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

/// Extract an [`OrmModelHint`] from a Python `class_definition` when the class body
/// declares an **explicit** table name (Slice 25, D3, M2b):
///
/// - **SQLAlchemy** — a class-body `__tablename__ = "<table>"` assignment.
/// - **Django** — a nested `class Meta:` whose body has `db_table = "<table>"`.
///
/// Only an explicit **string literal** name yields a hint: a dynamic right-hand side
/// (`__tablename__ = PREFIX + "x"` is a `binary_operator`, not a `string`) is
/// rejected, and a model with no such assignment yields `None` (convention-derived
/// inference is deferred — never invented, R1/R5). `class_fqn` is the model class's
/// fqn (for Django, the OUTER class, not `Meta`). SQLAlchemy is checked first; if a
/// class somehow has both, the explicit `__tablename__` wins (its own table name).
fn extract_orm_model_hint(node: Node, bytes: &[u8], class_fqn: &str) -> Option<OrmModelHint> {
    let body = node.child_by_field_name("body")?;
    let mut cursor = body.walk();
    // First pass: a direct `__tablename__ = "<str>"` in the class body (SQLAlchemy).
    for stmt in body.children(&mut cursor) {
        if let Some(table) = class_body_string_assignment(stmt, bytes, "__tablename__") {
            return Some(OrmModelHint {
                model_fqn: class_fqn.to_string(),
                table_name: table,
                framework: OrmFramework::SqlAlchemy,
                span: span_of(node),
            });
        }
    }
    // Second pass: a nested `class Meta:` whose body has `db_table = "<str>"` (Django).
    let mut cursor = body.walk();
    for stmt in body.children(&mut cursor) {
        if stmt.kind() != "class_definition" {
            continue;
        }
        let meta_name = stmt
            .child_by_field_name("name")
            .map(|n| text(n, bytes))
            .unwrap_or("");
        if meta_name != "Meta" {
            continue;
        }
        let Some(meta_body) = stmt.child_by_field_name("body") else {
            continue;
        };
        let mut meta_cursor = meta_body.walk();
        for meta_stmt in meta_body.children(&mut meta_cursor) {
            if let Some(table) = class_body_string_assignment(meta_stmt, bytes, "db_table") {
                return Some(OrmModelHint {
                    model_fqn: class_fqn.to_string(),
                    table_name: table,
                    framework: OrmFramework::Django,
                    span: span_of(node),
                });
            }
        }
    }
    None
}

/// If `stmt` is an `expression_statement` holding `<target> = "<str>"` where the
/// left target is exactly the identifier `target` and the right is a single string
/// literal, return the literal's inner (unquoted) text. Returns `None` for any other
/// shape — a different target, a non-string right (a `binary_operator`, a call, a
/// name), or a non-assignment — so a dynamic/computed table name is never captured.
fn class_body_string_assignment(stmt: Node, bytes: &[u8], target: &str) -> Option<String> {
    if stmt.kind() != "expression_statement" {
        return None;
    }
    let assignment = stmt.named_child(0)?;
    if assignment.kind() != "assignment" {
        return None;
    }
    let left = assignment.child_by_field_name("left")?;
    if left.kind() != "identifier" || text(left, bytes) != target {
        return None;
    }
    let right = assignment.child_by_field_name("right")?;
    // Only a single string literal is an explicit name; anything else is dynamic.
    if right.kind() != "string" {
        return None;
    }
    string_inner_text(right, bytes)
}

/// The inner (unquoted) text of a Python `string` node: the concatenation of its
/// `string_content` children (covers `"…"`, `'…'`, and triple-quoted forms). Returns
/// `None` if the string carries an `interpolation` child (an f-string is not a
/// single constant literal — never a table name we invent).
fn string_inner_text(node: Node, bytes: &[u8]) -> Option<String> {
    let mut literal = String::new();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "string_content" => literal.push_str(text(child, bytes)),
            "interpolation" => return None,
            _ => {}
        }
    }
    Some(literal)
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

/// Extract `import x` / `import x.y as z` (an `import_statement` may carry
/// several comma-separated clauses). The specifier is the *dotted module path as
/// written* (`os`, `os.path`); the bound name is the alias when present, else the
/// full dotted path text (matching how a bare `import os.path` binds `os`… but we
/// record the dotted specifier for resolution and the written binding for calls —
/// the alias case is the one that rebinds).
fn extract_plain_imports(node: Node, bytes: &[u8], out: &mut AnalyzedFile) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            // `import os` → specifier and bound name both the dotted path.
            "dotted_name" => {
                let spec = text(child, bytes).to_string();
                out.imports.push(ImportRef {
                    specifier: spec.clone(),
                    imported_names: vec![spec],
                    span: span_of(node),
                    name_spans: vec![span_of(child)],
                });
            }
            // `import os.path as p` → specifier "os.path", bound name "p".
            "aliased_import" => {
                let Some(name_node) = child.child_by_field_name("name") else {
                    continue;
                };
                let spec = text(name_node, bytes).to_string();
                let alias = child.child_by_field_name("alias");
                let (bound, bound_span) = match alias {
                    Some(a) => (text(a, bytes).to_string(), span_of(a)),
                    None => (spec.clone(), span_of(name_node)),
                };
                out.imports.push(ImportRef {
                    specifier: spec,
                    imported_names: vec![bound],
                    span: span_of(node),
                    name_spans: vec![bound_span],
                });
            }
            _ => {}
        }
    }
}

/// Extract `from <module> import a, b as c` / `from .rel import x` /
/// `from ..pkg import y` / `from mod import *`.
///
/// The specifier is the module text *as written*, including the leading dots for
/// a relative import (`.rel`, `..pkg.sub`) — the indexer resolves the relative
/// path later. A `*` (`wildcard_import`) binds **no** names: a star import is a
/// dynamic surface we never invent a binding for, though the import itself is
/// recorded so the dependency is visible.
fn extract_from_import(node: Node, bytes: &[u8]) -> Option<ImportRef> {
    let module = node.child_by_field_name("module_name")?;
    let specifier = module_specifier(module, bytes);

    let mut names = Vec::new();
    let mut name_spans = Vec::new();
    // The imported names are the `name:`-field children (each a `dotted_name` or
    // `aliased_import`); a `wildcard_import` child means `*` (bind nothing).
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        // Skip the module_name child itself (also reachable via the field).
        if child.id() == module.id() {
            continue;
        }
        match child.kind() {
            "dotted_name" => {
                names.push(text(child, bytes).to_string());
                name_spans.push(span_of(child));
            }
            "aliased_import" => {
                // `b as c` → bind the alias `c`; its span is the alias identifier.
                if let Some(alias) = child.child_by_field_name("alias") {
                    names.push(text(alias, bytes).to_string());
                    name_spans.push(span_of(alias));
                } else if let Some(name) = child.child_by_field_name("name") {
                    names.push(text(name, bytes).to_string());
                    name_spans.push(span_of(name));
                }
            }
            // `from mod import *` — record nothing (never invent a binding).
            "wildcard_import" => {}
            _ => {}
        }
    }

    Some(ImportRef {
        specifier,
        imported_names: names,
        span: span_of(node),
        name_spans,
    })
}

/// The specifier text for a from-import's `module_name`: a `relative_import`
/// (leading dots + optional dotted path → `.rel`, `..pkg.sub`, or bare `.`) or a
/// plain `dotted_name` (`mod`, `pkg.sub`). Taken verbatim from source so the
/// leading dots are preserved for relative-path resolution by the indexer.
fn module_specifier(module: Node, bytes: &[u8]) -> String {
    // `utf8_text` over the whole node is exactly the written form, dots included.
    text(module, bytes).to_string()
}

/// Extract a call site: bare `f()`, attribute `obj.m()` / `self.m()` /
/// `pkg.mod.func()`, and nested attribute chains.
///
/// Returns `None` for a *dynamic* call whose callee is not a plain name or
/// attribute — e.g. `getattr(o, 'x')()` (the function is itself a `call`), a
/// subscripted call `table[k]()`, or a lambda invocation. We never invent a
/// static callee for a computed target.
///
/// `span` covers the whole call; `callee_span` pinpoints the callee identifier
/// (the `attribute:` leaf for member calls), mirroring the TS adapter so precise
/// resolution can target the exact occurrence.
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
        "attribute" => {
            // `attribute object: <recv> attribute: <name>`. The callee is the
            // trailing `attribute` leaf; the receiver is the full immediate-object
            // text (`obj`, `self`, `pkg.mod`) — the honest receiver chain.
            let attr = function.child_by_field_name("attribute")?;
            let callee = text(attr, bytes).to_string();
            let receiver = function
                .child_by_field_name("object")
                .map(|o| text(o, bytes).to_string());
            Some(CallRef {
                callee_name: callee,
                receiver,
                enclosing_fqn: enclosing_fqn.to_string(),
                span: span_of(node),
                callee_span: span_of(attr),
                // Python `.` is overloaded: `ClassName.method()` (a call through a
                // class) and `obj.method()` (an instance call) are the same
                // `attribute` node — only knowing whether the receiver is a type or a
                // value separates them. A capitalized receiver is not a reliable type
                // signal, so we do NOT guess. Always `false` (a field receiver);
                // separating class-qualified calls is deferred to receiver inference
                // (A3).
                receiver_is_path: false,
            })
        }
        // Any other callee shape (a nested `call` like `getattr(...)()`, a
        // `subscript` like `handlers[k]()`, a parenthesized lambda, …) is a
        // dynamic target we do not attribute to a static symbol.
        _ => None,
    }
}

/// Extract producer route(s) from a decorated function's decorators (Flask/
/// FastAPI). `decorated` is the `decorated_definition`; `func_def` its
/// `function_definition`; `container` the enclosing class fqn (for a decorated
/// method). Recognised decorator shapes, each a `@<recv>.<verb>(...)` call:
///
/// - `@app.get("/x")` / `@router.post("/x")` (verb in [`ROUTE_VERBS`]) → one route,
///   method = the verb upper-cased.
/// - `@app.route("/x", methods=["GET", "POST"])` (Flask) → one route per method; a
///   bare `@app.route("/x")` (no `methods=`) defaults to a single `GET` route.
///
/// The first positional argument must be a **string-literal** path; a dynamic or
/// f-string path is not matched (never guessed, R1/R5). The producer is attributed
/// to the decorated function (`handler_name` = its name, `enclosing_fqn` = its fqn).
fn extract_decorator_routes(
    decorated: Node,
    func_def: Node,
    container: Option<&str>,
    bytes: &[u8],
    out: &mut AnalyzedFile,
) {
    let Some(name_node) = func_def.child_by_field_name("name") else {
        return;
    };
    let name = text(name_node, bytes).to_string();
    let fqn = make_fqn(container, &name);

    let mut cursor = decorated.walk();
    for child in decorated.children(&mut cursor) {
        if child.kind() != "decorator" {
            continue;
        }
        // The decorator's expression (after `@`); a route registration is a call.
        let Some(expr) = child.named_child(0) else {
            continue;
        };
        if expr.kind() != "call" {
            continue;
        }
        let Some(func) = expr.child_by_field_name("function") else {
            continue;
        };
        if func.kind() != "attribute" {
            continue;
        }
        let Some(attr) = func.child_by_field_name("attribute") else {
            continue;
        };
        let verb = text(attr, bytes);
        let Some(args) = expr.child_by_field_name("arguments") else {
            continue;
        };
        let Some(path) = first_string_arg(args, bytes) else {
            continue; // no literal path → not a route we can match (never guessed).
        };
        if ROUTE_VERBS.contains(&verb) {
            out.routes.push(RouteDecl {
                method: verb.to_ascii_uppercase(),
                path,
                handler_name: Some(name.clone()),
                enclosing_fqn: fqn.clone(),
                span: span_of(decorated),
            });
        } else if verb == "route" {
            // Flask: one route per declared method; default GET when none given.
            let methods = methods_kwarg(args, bytes).unwrap_or_else(|| vec!["GET".to_string()]);
            for method in methods {
                let method = method.to_ascii_uppercase();
                // A literal `methods=["<ANY>"]` must never masquerade as the
                // method-less Django sentinel (which would wrongly route this
                // method-bearing declaration through the path-only producer branch
                // and emit a confident-wrong edge). Drop it: it is not a real verb.
                if method == ROUTE_METHOD_ANY {
                    continue;
                }
                out.routes.push(RouteDecl {
                    method,
                    path: path.clone(),
                    handler_name: Some(name.clone()),
                    enclosing_fqn: fqn.clone(),
                    span: span_of(decorated),
                });
            }
        }
    }
}

/// The first **positional string-literal** argument of an `argument_list` (the
/// route/URL path), unquoted. Returns `None` if the first positional argument is not
/// a plain string literal (an f-string, an identifier, or a kwarg-first call) — a
/// non-literal path is never guessed.
fn first_string_arg(args: Node, bytes: &[u8]) -> Option<String> {
    let mut cursor = args.walk();
    for child in args.children(&mut cursor) {
        if !child.is_named() {
            continue;
        }
        // The first positional arg decides: a string is the path; anything else
        // (a kwarg, an identifier) means there is no literal positional path.
        if child.kind() == "string" {
            return string_inner_text(child, bytes);
        }
        return None;
    }
    None
}

/// Collect the string-literal entries of a `methods=[...]` keyword argument in an
/// `argument_list` (Flask `@app.route("/x", methods=["GET", "POST"])`). Returns
/// `None` when there is no `methods=` kwarg (caller defaults to `GET`) or it is not
/// a list literal (a variable is not enumerable). Non-string list entries are
/// skipped (never guessed).
fn methods_kwarg(args: Node, bytes: &[u8]) -> Option<Vec<String>> {
    let mut cursor = args.walk();
    for child in args.children(&mut cursor) {
        if child.kind() != "keyword_argument" {
            continue;
        }
        let name = child.child_by_field_name("name")?;
        if text(name, bytes) != "methods" {
            continue;
        }
        let value = child.child_by_field_name("value")?;
        // Flask accepts a list or tuple of method strings. A non-literal `methods=`
        // (a variable) is not enumerable -> None (the caller defaults to GET); an
        // empty list/tuple yields no methods, so the route is dropped, never guessed.
        if value.kind() != "list" && value.kind() != "tuple" {
            return None;
        }
        let mut methods = Vec::new();
        let mut vc = value.walk();
        for entry in value.children(&mut vc) {
            if entry.kind() == "string" {
                if let Some(m) = string_inner_text(entry, bytes) {
                    methods.push(m);
                }
            }
        }
        return Some(methods);
    }
    None
}

/// Extract a method-less Django URLconf route from a `call` node:
/// `path("users/<int:pk>/", view)`, `re_path(r"^x$", view)`, or `url(r"…", view)`.
/// The callee must be a bare identifier in [`DJANGO_ROUTE_FNS`] and the first
/// positional argument a string-literal pattern; otherwise `None`. The route is
/// method-less ([`ROUTE_METHOD_ANY`]); the handler is the second argument's view
/// name when it is an `identifier` (`my_view`) or `attribute` (`views.my_view`),
/// else `None` (a `Class.as_view()` call falls back to the module at link time —
/// Django views are usually cross-file).
fn extract_django_route(node: Node, bytes: &[u8], enclosing_fqn: &str) -> Option<RouteDecl> {
    let function = node.child_by_field_name("function")?;
    if function.kind() != "identifier" {
        return None;
    }
    let fn_name = text(function, bytes);
    if !DJANGO_ROUTE_FNS.contains(&fn_name) {
        return None;
    }
    let args = node.child_by_field_name("arguments")?;
    let positionals: Vec<Node> = {
        let mut cursor = args.walk();
        args.children(&mut cursor)
            .filter(|c| c.is_named())
            .collect()
    };
    let first = positionals.first()?;
    if first.kind() != "string" {
        return None;
    }
    let raw = string_inner_text(*first, bytes)?;
    // `path("users/<int:pk>/", …)` uses converter syntax and is mount-relative
    // (no leading slash, conventional trailing slash); canonicalise it to the
    // OpenAPI path shape (leading slash, no trailing) so the method-agnostic path
    // match aligns — without this a Django producer link is a silent no-op. A
    // `re_path`/`url` pattern is a regex (`r"^…$"`), left raw: it carries regex
    // metacharacters and never matches a normalised operation path (an honest miss).
    let path = if fn_name == "path" {
        canonicalize_django_path(&raw)
    } else {
        raw
    };
    let handler_name = positionals
        .get(1)
        .and_then(|n| view_handler_name(*n, bytes));
    Some(RouteDecl {
        method: ROUTE_METHOD_ANY.to_string(),
        path,
        handler_name,
        enclosing_fqn: enclosing_fqn.to_string(),
        span: span_of(node),
    })
}

/// Canonicalise a Django `path()` converter pattern to the OpenAPI path shape: a
/// single leading slash and no trailing slash (except the bare root `/`). Django
/// patterns are written mount-relative (`users/<int:pk>/`) while an OpenAPI
/// operation path is `/users/{pk}`; without this they would never match (a silent
/// no-op). Segment-level `<int:pk>` → `{}` canonicalisation is then applied by
/// `normalize_path` at match time.
fn canonicalize_django_path(pattern: &str) -> String {
    let trimmed = pattern.trim_matches('/');
    if trimmed.is_empty() {
        "/".to_string()
    } else {
        format!("/{trimmed}")
    }
}

/// The view handler name from a Django route's view argument: a bare `identifier`
/// (`my_view`) yields its text; an `attribute` (`views.my_view`) yields the trailing
/// `.attribute` leaf. Any other shape (a `Class.as_view()` call, a lambda) yields
/// `None`, so the producer falls back to the declaring module rather than guessing.
fn view_handler_name(node: Node, bytes: &[u8]) -> Option<String> {
    match node.kind() {
        "identifier" => Some(text(node, bytes).to_string()),
        "attribute" => node
            .child_by_field_name("attribute")
            .map(|a| text(a, bytes).to_string()),
        _ => None,
    }
}

/// Extract an outgoing HTTP call (a REST consumer signal) from a `call` node:
/// `requests.<verb>(url, …)` / `httpx.<verb>(url, …)` (verb in [`HTTP_CLIENT_VERBS`]
/// ⇒ method = the verb), or the generic `requests.request("GET", url, …)` (method =
/// the first string-literal argument, else `None` — unknown, not assumed). The
/// receiver must be one of [`HTTP_CLIENT_OBJECTS`] (`requests`/`httpx`); any other
/// receiver is not matched (Python `.get()` is far too common to assume — R5).
///
/// The URL argument's shape is read by [`url_shape_of`]: a string literal →
/// `Literal`, an f-string → `Template` (interpolations canonicalised to `{}`), and
/// anything else → `Dynamic` (opaque path, never matched downstream).
fn extract_http_call(node: Node, bytes: &[u8], enclosing_fqn: &str) -> Option<HttpCall> {
    let function = node.child_by_field_name("function")?;
    if function.kind() != "attribute" {
        return None;
    }
    let object = function.child_by_field_name("object")?;
    if object.kind() != "identifier" || !HTTP_CLIENT_OBJECTS.contains(&text(object, bytes)) {
        return None;
    }
    let verb = text(function.child_by_field_name("attribute")?, bytes);
    let args = node.child_by_field_name("arguments")?;
    // Positional args only (a keyword like `timeout=…` is neither the URL nor method).
    let positionals: Vec<Node> = {
        let mut cursor = args.walk();
        args.children(&mut cursor)
            .filter(|c| c.is_named() && c.kind() != "keyword_argument")
            .collect()
    };

    if HTTP_CLIENT_VERBS.contains(&verb) {
        let url = positionals
            .first()
            .map(|n| url_shape_of(*n, bytes))
            .unwrap_or(UrlShape::Dynamic);
        Some(HttpCall {
            method: Some(verb.to_ascii_uppercase()),
            url,
            enclosing_fqn: enclosing_fqn.to_string(),
            span: span_of(node),
        })
    } else if verb == "request" {
        // `requests.request(method, url, …)`: method is the first string literal
        // (None if not a plain literal — unknown, never assumed); url is the 2nd arg.
        let method = positionals
            .first()
            .filter(|n| n.kind() == "string")
            .and_then(|n| string_inner_text(*n, bytes))
            .map(|m| m.to_ascii_uppercase());
        let url = positionals
            .get(1)
            .map(|n| url_shape_of(*n, bytes))
            .unwrap_or(UrlShape::Dynamic);
        Some(HttpCall {
            method,
            url,
            enclosing_fqn: enclosing_fqn.to_string(),
            span: span_of(node),
        })
    } else {
        None
    }
}

/// The static shape of a URL argument. A `string` literal with no f-string
/// interpolation → [`UrlShape::Literal`] (its inner text); an f-string (any
/// `interpolation` child) → [`UrlShape::Template`] with each interpolation
/// canonicalised to `{}` (so `f"/users/{id}"` → `"/users/{}"`, matching a
/// normalized operation path); anything else (a bare identifier, a concatenation,
/// a call) → [`UrlShape::Dynamic`] (opaque — never matched, R5).
fn url_shape_of(node: Node, bytes: &[u8]) -> UrlShape {
    if node.kind() != "string" {
        return UrlShape::Dynamic;
    }
    let mut acc = String::new();
    let mut has_interpolation = false;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "string_content" => acc.push_str(text(child, bytes)),
            "interpolation" => {
                has_interpolation = true;
                acc.push_str("{}");
            }
            _ => {}
        }
    }
    if has_interpolation {
        UrlShape::Template(acc)
    } else {
        UrlShape::Literal(acc)
    }
}

/// Extract a GraphQL consumer document from a `gql("…")` call (the `gql` library's
/// document tag). `tagged` is always true (the author explicitly marked it GraphQL),
/// so a parse failure at link time is an honest miss (counted, not linked). The
/// first positional argument must be a string literal; an f-string sets
/// `interpolation_free = false` (its expansion is opaque → counted, never linked,
/// like the TS adapter's interpolated tagged template). A non-string first argument
/// yields no document.
fn extract_gql_document(node: Node, bytes: &[u8], enclosing_fqn: &str) -> Option<GqlDocument> {
    let function = node.child_by_field_name("function")?;
    if function.kind() != "identifier" || text(function, bytes) != "gql" {
        return None;
    }
    let args = node.child_by_field_name("arguments")?;
    // The first positional argument (the document string); a kwarg-first or empty
    // call yields no document.
    let first = args.named_child(0)?;
    if first.kind() != "string" {
        return None;
    }
    let mut acc = String::new();
    let mut interpolation_free = true;
    let mut cursor = first.walk();
    for child in first.children(&mut cursor) {
        match child.kind() {
            "string_content" => acc.push_str(text(child, bytes)),
            "interpolation" => interpolation_free = false,
            _ => {}
        }
    }
    Some(GqlDocument {
        text: acc,
        interpolation_free,
        tagged: true,
        enclosing_fqn: enclosing_fqn.to_string(),
        span: span_of(node),
    })
}

/// Extract Graphene GraphQL producer resolvers from an (undecorated) class:
/// `class Query(graphene.ObjectType)` whose `resolve_<field>` methods implement
/// `Query.<field>`. Conservative: the class must be named exactly one of
/// [`GRAPHQL_OP_TYPES`] AND have a `*.ObjectType` base, and only `resolve_`-prefixed
/// methods are taken (a missed resolver is acceptable; a guessed one is not).
fn extract_graphene_resolvers(
    class_node: Node,
    class_name: &str,
    bytes: &[u8],
    out: &mut AnalyzedFile,
) {
    if !GRAPHQL_OP_TYPES.contains(&class_name) || !has_objecttype_base(class_node, bytes) {
        return;
    }
    let Some(body) = class_node.child_by_field_name("body") else {
        return;
    };
    let mut cursor = body.walk();
    for stmt in body.children(&mut cursor) {
        // A method may be bare or decorated; unwrap a `decorated_definition`.
        let func = match stmt.kind() {
            "function_definition" => stmt,
            "decorated_definition" => match stmt.child_by_field_name("definition") {
                Some(d) if d.kind() == "function_definition" => d,
                _ => continue,
            },
            _ => continue,
        };
        let Some(name_node) = func.child_by_field_name("name") else {
            continue;
        };
        let method_name = text(name_node, bytes);
        let Some(field) = method_name.strip_prefix("resolve_") else {
            continue;
        };
        if field.is_empty() {
            continue;
        }
        out.resolver_entries.push(ResolverEntry {
            op_type: class_name.to_string(),
            field: field.to_string(),
            handler_name: Some(method_name.to_string()),
            enclosing_fqn: make_fqn(Some(class_name), method_name),
            span: span_of(func),
        });
    }
}

/// Whether a `class_definition`'s `superclasses` include a `*.ObjectType` base
/// (`graphene.ObjectType` or a bare `ObjectType`) — the Graphene resolver-host
/// signal.
fn has_objecttype_base(class_node: Node, bytes: &[u8]) -> bool {
    let Some(supers) = class_node.child_by_field_name("superclasses") else {
        return false;
    };
    let mut cursor = supers.walk();
    let has_base = supers.children(&mut cursor).any(|base| {
        base.is_named() && {
            let t = text(base, bytes);
            t == "ObjectType" || t.ends_with(".ObjectType")
        }
    });
    has_base
}

/// Extract Strawberry GraphQL producer resolvers from a `@strawberry.type`-decorated
/// class named one of [`GRAPHQL_OP_TYPES`]: each `@strawberry.field`-decorated method
/// implements `<Class>.<method>`. `decorated` is the `decorated_definition`,
/// `class_def` its `class_definition`.
fn extract_strawberry_resolvers(
    decorated: Node,
    class_def: Node,
    bytes: &[u8],
    out: &mut AnalyzedFile,
) {
    if !has_decorator_attr(decorated, bytes, "strawberry", "type") {
        return;
    }
    let Some(name_node) = class_def.child_by_field_name("name") else {
        return;
    };
    let class_name = text(name_node, bytes);
    if !GRAPHQL_OP_TYPES.contains(&class_name) {
        return;
    }
    let Some(body) = class_def.child_by_field_name("body") else {
        return;
    };
    let mut cursor = body.walk();
    for stmt in body.children(&mut cursor) {
        if stmt.kind() != "decorated_definition"
            || !has_decorator_attr(stmt, bytes, "strawberry", "field")
        {
            continue;
        }
        let Some(def) = stmt.child_by_field_name("definition") else {
            continue;
        };
        if def.kind() != "function_definition" {
            continue;
        }
        let Some(name_node) = def.child_by_field_name("name") else {
            continue;
        };
        let method_name = text(name_node, bytes);
        out.resolver_entries.push(ResolverEntry {
            op_type: class_name.to_string(),
            field: method_name.to_string(),
            handler_name: Some(method_name.to_string()),
            enclosing_fqn: make_fqn(Some(class_name), method_name),
            span: span_of(def),
        });
    }
}

/// Whether any decorator on `decorated` is `@<obj>.<attr>` — matching both the bare
/// attribute form (`@strawberry.type`) and the call form (`@strawberry.field(...)`).
fn has_decorator_attr(decorated: Node, bytes: &[u8], obj: &str, attr: &str) -> bool {
    let mut cursor = decorated.walk();
    for child in decorated.children(&mut cursor) {
        if child.kind() != "decorator" {
            continue;
        }
        let Some(expr) = child.named_child(0) else {
            continue;
        };
        // `@a.b` → the expr is the attribute; `@a.b(...)` → it is a call whose
        // function is the attribute.
        let attr_node = match expr.kind() {
            "attribute" => expr,
            "call" => match expr.child_by_field_name("function") {
                Some(f) if f.kind() == "attribute" => f,
                _ => continue,
            },
            _ => continue,
        };
        let o = attr_node
            .child_by_field_name("object")
            .map(|n| text(n, bytes));
        let a = attr_node
            .child_by_field_name("attribute")
            .map(|n| text(n, bytes));
        if o == Some(obj) && a == Some(attr) {
            return true;
        }
    }
    false
}

/// Extract an Ariadne GraphQL producer resolver from a function decorated
/// `@query.field("name")` / `@mutation.field("name")` / `@subscription.field("name")`.
/// The decorator receiver maps to the op type ([`ariadne_op_type`]); the field name
/// is the decorator's first string-literal argument; the handler is the decorated
/// function. `decorated` is the `decorated_definition`, `func_def` its function.
fn extract_ariadne_resolver(
    decorated: Node,
    func_def: Node,
    container: Option<&str>,
    bytes: &[u8],
    out: &mut AnalyzedFile,
) {
    let Some(name_node) = func_def.child_by_field_name("name") else {
        return;
    };
    let fn_name = text(name_node, bytes);
    let fn_fqn = make_fqn(container, fn_name);

    let mut cursor = decorated.walk();
    for child in decorated.children(&mut cursor) {
        if child.kind() != "decorator" {
            continue;
        }
        let Some(expr) = child.named_child(0) else {
            continue;
        };
        if expr.kind() != "call" {
            continue;
        }
        let Some(func) = expr.child_by_field_name("function") else {
            continue;
        };
        if func.kind() != "attribute" {
            continue;
        }
        if func
            .child_by_field_name("attribute")
            .map(|n| text(n, bytes))
            != Some("field")
        {
            continue;
        }
        let Some(op_type) = func
            .child_by_field_name("object")
            .map(|n| text(n, bytes))
            .and_then(ariadne_op_type)
        else {
            continue;
        };
        let Some(args) = expr.child_by_field_name("arguments") else {
            continue;
        };
        let Some(field) = first_string_arg(args, bytes) else {
            continue;
        };
        out.resolver_entries.push(ResolverEntry {
            op_type: op_type.to_string(),
            field,
            handler_name: Some(fn_name.to_string()),
            enclosing_fqn: fn_fqn.clone(),
            span: span_of(decorated),
        });
    }
}

/// Map an Ariadne decorator receiver var (`query`/`mutation`/`subscription`, the
/// conventional `QueryType()`/`MutationType()` binding names) to its canonical
/// GraphQL op type. Any other receiver → `None` (not a recognised resolver root).
fn ariadne_op_type(receiver: &str) -> Option<&'static str> {
    match receiver {
        "query" => Some("Query"),
        "mutation" => Some("Mutation"),
        "subscription" => Some("Subscription"),
        _ => None,
    }
}
