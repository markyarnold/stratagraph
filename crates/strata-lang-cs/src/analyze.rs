//! Pure per-file extraction of symbols, usings, and intra-file calls.
//!
//! Operates on a C# source string via Tree-sitter; performs no IO. Like the
//! TS/Python adapters, extraction is a manual recursive walk over the parse tree
//! (no Tree-sitter queries), which keeps the direct dependency set minimal and
//! gives precise control over the namespace/container/enclosing-scope tracking a
//! call site needs.
//!
//! Only the code-plane fields of [`AnalyzedFile`] are filled
//! (`symbols`/`imports`/`calls`); the contract-plane fields stay empty this slice
//! (see the crate docs for the honesty story).
//!
//! **Honesty invariants enforced here, at extraction:**
//! - **Overloads collapse by name.** Two methods `Run()` and `Run(int)` on the
//!   same type produce two `RawSymbol`s with the *same* fqn
//!   (`T.Run`). Downstream linking treats them as one logical target — the flat-fqn
//!   precedent from Python. Arity-aware splitting is a Roslyn (A3) refinement.
//! - **Reflection / dynamic targets are never invented.** `t.GetMethod("Run")` is
//!   recorded as an honest member call to `GetMethod` with receiver `t`; the string
//!   `"Run"` is NEVER promoted to a call of a method named `Run`. A `dynamic`
//!   receiver is just an unknown receiver. The extractor records what is written.
//! - **Computed callees yield no static callee.** An invocation whose `function`
//!   is itself an invocation/element-access/parenthesized lambda (e.g.
//!   `factory()()`, `handlers[k]()`) is a dynamic target — no callee name invented.

use strata_core::{AnalyzedFile, CallRef, ImportRef, NodeKind, RawSymbol, Span, SqlCandidate};
use tree_sitter::{Node, Parser};

/// Analyze a single C# source file.
///
/// Pure: no filesystem access. Returns whatever can be extracted; on a parse
/// failure (grammar load) returns an empty `AnalyzedFile` rather than panicking.
pub fn analyze(_path: &str, source: &str) -> AnalyzedFile {
    let mut parser = Parser::new();
    let lang: tree_sitter::Language = tree_sitter_c_sharp::LANGUAGE.into();
    if parser.set_language(&lang).is_err() {
        return AnalyzedFile::default();
    }
    let Some(tree) = parser.parse(source, None) else {
        return AnalyzedFile::default();
    };

    let bytes = source.as_bytes();
    let mut out = AnalyzedFile::default();
    let ctx = Ctx {
        namespace: "",
        container: None,
        enclosing_fqn: "",
    };
    // The root's children are walked as a sibling sequence so a file-scoped
    // `namespace N;` switches the namespace for everything that follows it.
    walk_siblings(tree.root_node(), bytes, &ctx, &mut out);
    out
}

/// Walk context: the dotted namespace prefix in force, the enclosing *type* fqn
/// for member symbols (`Some` only directly inside a type body), and the nearest
/// enclosing method/local-function fqn for call attribution (`""` at namespace or
/// type top level — a call there attributes to the file's Module node).
struct Ctx<'a> {
    namespace: &'a str,
    container: Option<&'a str>,
    enclosing_fqn: &'a str,
}

/// Convert a Tree-sitter node's range into a core `Span` (1-based lines, 0-based
/// columns — matching the workspace convention).
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

/// Join a dotted prefix and a leaf with `.`; an empty prefix yields the leaf.
/// The single fqn-composition primitive — namespace + type + member all use it,
/// which is what makes the namespace convention uniform.
fn dot(prefix: &str, leaf: &str) -> String {
    if prefix.is_empty() {
        leaf.to_string()
    } else {
        format!("{prefix}.{leaf}")
    }
}

/// The fully-qualified name of a type/member: `namespace` then `container` then
/// `name`, each joined with `.` when non-empty. In `namespace App { class C { void
/// M() } }`, `M`'s fqn is `App.C.M` (the chosen convention).
fn fqn_of(ctx: &Ctx, name: &str) -> String {
    let prefix = match ctx.container {
        Some(c) if !c.is_empty() => c.to_string(),
        _ => ctx.namespace.to_string(),
    };
    dot(&prefix, name)
}

/// Recursive walk dispatching on node kind.
fn walk(node: Node, bytes: &[u8], ctx: &Ctx, out: &mut AnalyzedFile) {
    match node.kind() {
        // ── Usings (incl. `using X = Y` aliases and `global using`). ──
        "using_directive" => {
            if let Some(import) = extract_using(node, bytes) {
                out.imports.push(import);
            }
            return;
        }
        // ── Block namespace (`namespace N { … }`). Its declarations nest in a
        // `body: (declaration_list …)`, so re-walking its children with the
        // extended namespace is correct. Nested namespaces compose
        // (`namespace A { namespace B {…} }` → `A.B`). The file-scoped form
        // (`namespace N;`) is NOT handled here — its declarations are *siblings*,
        // so it is handled in `walk_siblings` instead.
        "namespace_declaration" => {
            let ns_name = node
                .child_by_field_name("name")
                .map(|n| text(n, bytes))
                .unwrap_or("");
            let new_ns = dot(ctx.namespace, ns_name);
            let inner = Ctx {
                namespace: &new_ns,
                container: None,
                enclosing_fqn: "",
            };
            walk_siblings(node, bytes, &inner, out);
            return;
        }
        // A file-scoped namespace reached here directly (not as a leading sibling)
        // declares the namespace for the rest of its container; `walk_siblings`
        // owns that switch, so as a standalone node it contributes no symbol.
        "file_scoped_namespace_declaration" => {
            return;
        }
        // ── Type declarations: class / interface / struct / record. ──
        // record (and `record struct`) extract as a `Class`-kind node — flagged in
        // the crate docs (a record is a reference/value type with members; for the
        // code graph it behaves as a class). The struct keyword variant parses as
        // either `record_declaration` (this grammar) or `record_struct_declaration`
        // (handled defensively).
        "class_declaration"
        | "interface_declaration"
        | "struct_declaration"
        | "record_declaration"
        | "record_struct_declaration" => {
            extract_type(node, bytes, ctx, out);
            return;
        }
        // ── Members of a type. ──
        "method_declaration" => {
            extract_method(node, bytes, ctx, out);
            return;
        }
        "constructor_declaration" => {
            // A constructor is recorded as a Method named after the type (its
            // `name` field IS the type name). Body calls attribute to it.
            extract_callable(node, bytes, ctx, out, NodeKind::Method);
            return;
        }
        // A local function (`void local() {}` inside a method body) → Function,
        // no container (it is not a type member), mirroring Python's nested-def
        // rule. Body calls attribute to the local function.
        "local_function_statement" => {
            extract_local_function(node, bytes, ctx, out);
            return;
        }
        // ── Call sites. ──
        "invocation_expression" => {
            if let Some(call) = extract_call(node, bytes, ctx.enclosing_fqn) {
                out.calls.push(call);
            }
            // Still descend (arguments may contain nested calls / lambdas).
            walk_children(node, bytes, ctx, out);
            return;
        }
        // ── SQL string literals → data-plane `SqlCandidate` (Slice 16, D3, M2). ──
        // The three *constant* string forms (regular `"…"`, verbatim `@"…"`, and the
        // raw `"""…"""`). An `interpolated_string_expression` (`$"…"`) is a DISTINCT
        // node kind and is deliberately NOT matched here — interpolation is dynamic
        // SQL, honestly dropped (we never guess a table from a `$"… {x} …"`, R1/R5).
        // Recorded additively; strings are leaves, so nothing to descend into.
        "string_literal" | "verbatim_string_literal" | "raw_string_literal" => {
            if let Some(cand) = extract_sql_candidate(node, bytes, ctx.enclosing_fqn) {
                out.sql_candidates.push(cand);
            }
            return;
        }
        _ => {}
    }

    walk_children(node, bytes, ctx, out);
}

/// Extract a [`SqlCandidate`] from a C# constant string literal node when its
/// inner text passes the cheap SQL-keyword prefilter
/// ([`looks_like_sql`](strata_core::looks_like_sql)).
///
/// Each form exposes its content differently: a regular `string_literal` has a
/// `string_literal_content` child, a `raw_string_literal` has a `raw_string_content`
/// child, and a `verbatim_string_literal` (`@"…"`) is a single leaf whose `@"`
/// prefix and trailing `"` are stripped. Only `$"…"` interpolated strings are
/// excluded — and those never reach here (a different node kind, not in the match
/// arm above), so dynamic SQL is honestly unlinked.
fn extract_sql_candidate(node: Node, bytes: &[u8], enclosing_fqn: &str) -> Option<SqlCandidate> {
    let mut literal = String::new();
    let mut cursor = node.walk();
    let mut saw_content_child = false;
    for child in node.children(&mut cursor) {
        match child.kind() {
            "string_literal_content" | "raw_string_content" => {
                literal.push_str(text(child, bytes));
                saw_content_child = true;
            }
            _ => {}
        }
    }
    // `verbatim_string_literal` is a leaf with no content child: strip the `@"`
    // prefix and trailing `"` from its raw text. (A regular/raw literal always has a
    // content child, so this fallback only fires for the verbatim form.)
    if !saw_content_child {
        let raw = text(node, bytes);
        literal = raw
            .strip_prefix("@\"")
            .or_else(|| raw.strip_prefix('"'))
            .unwrap_or(raw)
            .strip_suffix('"')
            .unwrap_or(raw)
            .to_string();
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

/// Walk every child of `node` with the given context, **honouring file-scoped
/// namespaces**: when a `file_scoped_namespace_declaration` appears among the
/// children, the namespace it names (composed onto the current one) applies to
/// every *subsequent* sibling. This is the C# rule that a `namespace N;` at the
/// top of a file governs the rest of the file, where the declarations are
/// siblings rather than nested in a body.
fn walk_siblings(node: Node, bytes: &[u8], ctx: &Ctx, out: &mut AnalyzedFile) {
    // `current_ns` owns the (possibly extended) namespace string so it can outlive
    // each loop iteration's borrow.
    let mut current_ns: String = ctx.namespace.to_string();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "file_scoped_namespace_declaration" {
            let ns_name = child
                .child_by_field_name("name")
                .map(|n| text(n, bytes))
                .unwrap_or("");
            current_ns = dot(ctx.namespace, ns_name);
            // The declaration itself contributes no symbol; its effect is the
            // namespace switch for the following siblings.
            continue;
        }
        let child_ctx = Ctx {
            namespace: &current_ns,
            container: ctx.container,
            enclosing_fqn: ctx.enclosing_fqn,
        };
        walk(child, bytes, &child_ctx, out);
    }
}

/// Walk every child of `node` with the given context (no namespace switching).
/// Used inside expression/statement bodies where a file-scoped namespace cannot
/// legally appear.
fn walk_children(node: Node, bytes: &[u8], ctx: &Ctx, out: &mut AnalyzedFile) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk(child, bytes, ctx, out);
    }
}

/// Extract a type declaration (class/interface/struct/record) and recurse into
/// its body with the type as the new container, so members get
/// namespace-qualified `Type.member` fqns.
fn extract_type(node: Node, bytes: &[u8], ctx: &Ctx, out: &mut AnalyzedFile) {
    let Some(name_node) = node.child_by_field_name("name") else {
        // A nameless/partial type fragment — descend without inventing a symbol.
        walk_children(node, bytes, ctx, out);
        return;
    };
    let name = text(name_node, bytes).to_string();
    let fqn = fqn_of(ctx, &name);

    // interface → Interface; everything else (class/struct/record/record struct)
    // → Class. The struct/record-as-Class choice is documented in the crate docs:
    // these are all "a type with members" for the code graph.
    let kind = if node.kind() == "interface_declaration" {
        NodeKind::Interface
    } else {
        NodeKind::Class
    };

    out.symbols.push(RawSymbol {
        kind,
        name,
        fqn: fqn.clone(),
        container_fqn: None,
        span: span_of(node),
    });

    // Bases (`: Base, IFace` → a `base_list`) are parsed for the type header but,
    // like the TS/Python analyzers' inheritance, are NOT emitted into the frozen
    // `AnalyzedFile` model — no `Extends`/`Implements` edge is produced this slice
    // (deferred; see crate docs). Members are extracted with this type as their
    // container. The enclosing fqn resets — a field initializer call would
    // attribute to the type's Module, not to a phantom method.
    let inner = Ctx {
        namespace: ctx.namespace,
        container: Some(&fqn),
        enclosing_fqn: "",
    };
    walk_children(node, bytes, &inner, out);
}

/// Extract a `method_declaration` as a `Method` (when inside a type) or a
/// `Function` (top-level/local — rare for a bare method, but handled).
fn extract_method(node: Node, bytes: &[u8], ctx: &Ctx, out: &mut AnalyzedFile) {
    let kind = match ctx.container {
        Some(c) if !c.is_empty() => NodeKind::Method,
        _ => NodeKind::Function,
    };
    extract_callable(node, bytes, ctx, out, kind);
}

/// Extract a method/constructor: push the symbol, then walk its body with the
/// callable's fqn as the enclosing scope (so its calls attribute to it).
fn extract_callable(node: Node, bytes: &[u8], ctx: &Ctx, out: &mut AnalyzedFile, kind: NodeKind) {
    let Some(name_node) = node.child_by_field_name("name") else {
        walk_children(node, bytes, ctx, out);
        return;
    };
    let name = text(name_node, bytes).to_string();
    let fqn = fqn_of(ctx, &name);

    // `container_fqn` is the enclosing type fqn for a Method; None for a Function.
    // Overloads collapse: two methods of the same name on one type push the SAME
    // fqn here — that is the deliberate name-level collapse (documented).
    let container_fqn = if kind == NodeKind::Method {
        ctx.container.map(str::to_string)
    } else {
        None
    };

    out.symbols.push(RawSymbol {
        kind,
        name,
        fqn: fqn.clone(),
        container_fqn,
        span: span_of(node),
    });

    // Body calls attribute to this callable; the container is unchanged (a method
    // body can declare a local function whose own container is None).
    let inner = Ctx {
        namespace: ctx.namespace,
        container: ctx.container,
        enclosing_fqn: &fqn,
    };
    if let Some(body) = node.child_by_field_name("body") {
        walk_children(body, bytes, &inner, out);
    } else {
        // Expression-bodied member (`=> expr`) or abstract decl: walk all children
        // except re-deriving the body, so an expression-body call still lands.
        walk_non_name_children(node, bytes, &inner, out);
    }
}

/// A local function inside a method body → a `Function` with no container, its
/// body attributing calls to the local function's own (namespace-qualified) fqn.
fn extract_local_function(node: Node, bytes: &[u8], ctx: &Ctx, out: &mut AnalyzedFile) {
    let Some(name_node) = node.child_by_field_name("name") else {
        walk_children(node, bytes, ctx, out);
        return;
    };
    let name = text(name_node, bytes).to_string();
    // A local function is named within the namespace (not the type) — it is a
    // closure, not a member. fqn is namespace.name (or bare name at no namespace).
    let fqn = dot(ctx.namespace, &name);
    out.symbols.push(RawSymbol {
        kind: NodeKind::Function,
        name,
        fqn: fqn.clone(),
        container_fqn: None,
        span: span_of(node),
    });
    let inner = Ctx {
        namespace: ctx.namespace,
        container: None,
        enclosing_fqn: &fqn,
    };
    if let Some(body) = node.child_by_field_name("body") {
        walk_children(body, bytes, &inner, out);
    } else {
        walk_non_name_children(node, bytes, &inner, out);
    }
}

/// Walk all children except the `name` field child (used for expression-bodied
/// callables so we descend into the expression body without re-emitting the name
/// as a call).
fn walk_non_name_children(node: Node, bytes: &[u8], ctx: &Ctx, out: &mut AnalyzedFile) {
    let name_id = node.child_by_field_name("name").map(|n| n.id());
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if Some(child.id()) == name_id {
            continue;
        }
        walk(child, bytes, ctx, out);
    }
}

/// Extract a `using_directive` into an [`ImportRef`].
///
/// Three shapes:
/// - `using System;` → specifier `"System"`, bound name `"System"`.
/// - `using Alias = System.Text.StringBuilder;` → specifier
///   `"System.Text.StringBuilder"`, bound name `"Alias"` (the alias). The alias
///   identifier is the `name:` field; the target is the namespace/type child.
/// - `global using System.Linq;` → like a plain using (a `global` child marks it),
///   specifier `"System.Linq"`. Recorded identically — `global` widens scope to
///   the whole assembly, which for our single-file extraction is the same record.
///
/// A `using static System.Math;` (a `static` child) is recorded as a plain using
/// of the type namespace — its members are imported, which we surface as the
/// dependency without inventing per-member bindings.
fn extract_using(node: Node, bytes: &[u8]) -> Option<ImportRef> {
    // The alias name, if any (`using X = Y` has a `name:` field).
    let alias = node.child_by_field_name("name");

    // The target namespace/type is the qualified_name / identifier child that is
    // NOT the alias name and NOT a keyword (`using`, `global`, `static`, `=`, `;`).
    let mut target: Option<Node> = None;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if Some(child.id()) == alias.map(|a| a.id()) {
            continue;
        }
        if matches!(
            child.kind(),
            "qualified_name" | "identifier" | "alias_qualified_name"
        ) {
            target = Some(child);
        }
    }
    let target = target?;
    let specifier = text(target, bytes).to_string();

    // Bound name: the alias when present, else the specifier's *last segment* is
    // what a bare `using` makes available, but we record the full specifier as the
    // bound name too so a fully-qualified `Namespace.Type.Method()` style call can
    // match. We bind the alias (rebinding case) or the specifier itself.
    let (bound, bound_span) = match alias {
        Some(a) => (text(a, bytes).to_string(), span_of(a)),
        None => (specifier.clone(), span_of(target)),
    };

    Some(ImportRef {
        specifier,
        imported_names: vec![bound],
        span: span_of(node),
        name_spans: vec![bound_span],
    })
}

/// Extract a call site from an `invocation_expression`.
///
/// `function:` is the callee expression. Two attributable shapes:
/// - **`identifier`** (`Helper()`, `local()`) → a bare call, no receiver.
/// - **`member_access_expression`** (`this.Process()`, `obj.Do()`, `Type.M()`,
///   `Build().Trim()`) → callee is the `name:` leaf; the receiver is the full
///   immediate-`expression:` text (`this`, `obj`, `Type`, `Build()` — the honest
///   receiver chain, recorded verbatim even when it is itself a call).
///
/// Returns `None` for any other callee shape (a nested `invocation_expression`
/// used directly as `factory()()`, an `element_access_expression` `map[k]()`, a
/// parenthesized lambda) — a computed/dynamic target we never attribute to a
/// static symbol. This is also where reflection stays honest: `t.GetMethod("Run")`
/// is a member call to `GetMethod`; the `"Run"` string is an *argument*, never a
/// callee.
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
        "member_access_expression" => {
            let name = function.child_by_field_name("name")?;
            let callee = text(name, bytes).to_string();
            let receiver = function
                .child_by_field_name("expression")
                .map(|o| text(o, bytes).to_string());
            Some(CallRef {
                callee_name: callee,
                receiver,
                enclosing_fqn: enclosing_fqn.to_string(),
                span: span_of(node),
                callee_span: span_of(name),
                // C# `.` is overloaded: `Type.Method()` (static) and `obj.Method()`
                // (instance) are the same `member_access_expression` — distinguishing
                // a type qualifier from a value receiver needs receiver-type
                // inference. Always `false` (a field receiver); separating static
                // calls is deferred to receiver inference (A3).
                receiver_is_path: false,
            })
        }
        // Any other callee shape (a computed/dynamic target) — not attributed.
        _ => None,
    }
}
