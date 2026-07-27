//! Pure per-file extraction of symbols, `use` imports, and intra-file calls.
//!
//! Operates on a Rust source string via Tree-sitter; performs no IO. Like the
//! TS/Python/C# adapters, extraction is a manual recursive walk over the parse
//! tree (no Tree-sitter queries), which keeps the direct dependency set minimal
//! and gives precise control over the module/container/enclosing-scope tracking a
//! call site needs.
//!
//! Only the code-plane fields of [`AnalyzedFile`] are filled
//! (`symbols`/`imports`/`calls`, plus `sql_candidates` for the data plane); the
//! contract-plane fields stay empty this slice (see the crate docs for the honesty
//! story).
//!
//! **Honesty invariants enforced here, at extraction:**
//! - **Macros are NOT calls.** A `foo!(…)` is a `macro_invocation` node, distinct
//!   from a `call_expression`. The walk matches `call_expression` only, so
//!   `println!`, `vec!`, `assert!`, a user `my_macro!()` — none produce a `CallRef`.
//!   The macro's expansion is NEVER guessed. `macro_rules!` definitions
//!   (`macro_definition`) are not symbols this slice. This is the load-bearing
//!   honesty pin: the call graph reflects statically-written calls, not what a
//!   macro might expand to.
//! - **Generics are stripped from the impl self-type.** `impl<T> Container<T> { … }`
//!   attributes its methods to `Container` (the base type identifier), not
//!   `Container<T>` — so a call to a method resolves by the type's plain name. The
//!   same as the C# plane stripping type parameters.
//! - **Computed/dynamic callees yield no static callee.** An invocation whose
//!   `function` is itself a call/index/paren expression (`factory()()`,
//!   `handlers[k]()`) is a dynamic target — no callee name invented.

use strata_core::{AnalyzedFile, CallRef, ImportRef, NodeKind, RawSymbol, Span, SqlCandidate};
use tree_sitter::{Node, Parser};

/// Analyze a single Rust source file.
///
/// Pure: no filesystem access. Returns whatever can be extracted; on a parse
/// failure (grammar load) returns an empty `AnalyzedFile` rather than panicking.
pub fn analyze(_path: &str, source: &str) -> AnalyzedFile {
    let mut parser = Parser::new();
    let lang: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
    if parser.set_language(&lang).is_err() {
        return AnalyzedFile::default();
    }
    let Some(tree) = parser.parse(source, None) else {
        return AnalyzedFile::default();
    };

    let bytes = source.as_bytes();
    let mut out = AnalyzedFile::default();
    let ctx = Ctx {
        module: "",
        container: None,
        enclosing_fqn: "",
    };
    walk_children(tree.root_node(), bytes, &ctx, &mut out);
    out
}

/// Walk context: the `::`-joined module-path prefix in force, the enclosing
/// *type/trait* fqn for member symbols (`Some` only directly inside a type or
/// `impl`/`trait` body), and the nearest enclosing fn fqn for call attribution
/// (`""` at module or type top level — a call there attributes to the file's
/// Module node).
struct Ctx<'a> {
    module: &'a str,
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

/// Whether a `line_comment`/`block_comment` node is a DOC comment (`///`,
/// `//!`, `/** */`, `/*! */`) rather than a plain `//`/`/* */` comment.
/// tree-sitter-rust surfaces this structurally: a doc comment's node has an
/// `outer`/`inner` field (the marker) plus a `doc` field (its content); a plain
/// comment has neither. Checking the field beats a text-prefix scan — it is the
/// grammar's own distinction (immune to `////`/`/***`-style banner-comment
/// edge cases) and works identically for both node kinds.
fn is_doc_comment(node: Node) -> bool {
    matches!(node.kind(), "line_comment" | "block_comment")
        && (node.child_by_field_name("outer").is_some()
            || node.child_by_field_name("inner").is_some())
}

/// A comment node's own [`Span`], correcting a tree-sitter-rust quirk: a DOC
/// LINE comment's (`///`/`//!`) node range includes its own trailing newline
/// (so the external scanner can merge consecutive `///` lines), which makes the
/// raw `end_position()` misreport one row low with column 0 — a plain `//`
/// comment or a `/** */` block does not do this. Detected structurally (the
/// node's own text ends with `\n`) rather than by kind, and corrected using the
/// node's own start row: a line comment runs to end-of-line by construction, so
/// stripping the swallowed newline can never leave a multi-line remainder.
fn comment_span(node: Node, bytes: &[u8]) -> Span {
    let start = node.start_position();
    let raw = text(node, bytes);
    match raw.strip_suffix('\n') {
        Some(content) => Span {
            start_line: start.row as u32 + 1,
            start_col: start.column as u32,
            end_line: start.row as u32 + 1,
            end_col: start.column as u32 + content.len() as u32,
        },
        None => span_of(node),
    }
}

/// Walk `decl`'s `prev_sibling` chain collecting a contiguous, gap-free run of
/// doc comments immediately above it — no blank line between the run and
/// `decl`, and none between two comments within the run (each checked via
/// [`comment_span`]'s corrected end row against the next node's start row). A
/// non-doc-comment sibling, or a row gap, stops the walk right there — the run
/// never crosses either. Returns the SPAN of the whole run (topmost comment's
/// start through the nearest comment's end) — never its text (bodies-from-disk,
/// design decision 4/§8).
fn doc_span_of(decl: Node, bytes: &[u8]) -> Option<Span> {
    let mut expected_row = decl.start_position().row as u32;
    let mut nearest: Option<Node> = None;
    let mut topmost: Option<Node> = None;
    let mut cursor = decl.prev_sibling();
    while let Some(node) = cursor {
        if !is_doc_comment(node) {
            break;
        }
        let span = comment_span(node, bytes);
        if span.end_line != expected_row {
            break;
        }
        if nearest.is_none() {
            nearest = Some(node);
        }
        topmost = Some(node);
        expected_row = node.start_position().row as u32;
        cursor = node.prev_sibling();
    }
    let nearest = nearest?;
    let topmost = topmost.unwrap_or(nearest);
    let nearest_span = comment_span(nearest, bytes);
    let topmost_start = topmost.start_position();
    Some(Span {
        start_line: topmost_start.row as u32 + 1,
        start_col: topmost_start.column as u32,
        end_line: nearest_span.end_line,
        end_col: nearest_span.end_col,
    })
}

/// Join a `::`-path prefix and a leaf with `::`; an empty prefix yields the leaf.
/// The single fqn-composition primitive — module + type + member all use it, which
/// is what makes the `::`-path convention uniform.
fn join(prefix: &str, leaf: &str) -> String {
    if prefix.is_empty() {
        leaf.to_string()
    } else {
        format!("{prefix}::{leaf}")
    }
}

/// The fully-qualified name of a type/member: `module` then `container` then
/// `name`, each joined with `::` when non-empty. In `mod m { struct C; impl C { fn
/// f(&self) {} } }`, `f`'s fqn is `m::C::f` (the chosen convention).
fn fqn_of(ctx: &Ctx, name: &str) -> String {
    let prefix = match ctx.container {
        Some(c) if !c.is_empty() => c.to_string(),
        _ => ctx.module.to_string(),
    };
    join(&prefix, name)
}

/// Strip generic arguments from a self-type node, returning the base type name.
/// `impl Container { … }` → the `type_identifier` `Container`; `impl Container<T>`
/// → the `generic_type`'s inner `type:` `type_identifier` `Container`; a
/// `scoped_type_identifier` (`crate::Foo`) → its full text. Returns `None` when no
/// nameable type can be derived (e.g. an `impl Trait for (A, B)` tuple type — a
/// rare shape we honestly skip rather than invent a container for).
fn impl_self_type_name(type_node: Node, bytes: &[u8]) -> Option<String> {
    match type_node.kind() {
        "type_identifier" => Some(text(type_node, bytes).to_string()),
        // `Container<T>` — the base type is the `type:` child (a type_identifier or
        // scoped_type_identifier). Generics are stripped so methods attribute to the
        // plain type name.
        "generic_type" => type_node
            .child_by_field_name("type")
            .map(|n| text(n, bytes).to_string()),
        // `crate::module::Foo` / `Foo::Bar` — record the full scoped path text. Its
        // last segment is what call resolution keys on, but the full path keeps the
        // fqn unambiguous within the file.
        "scoped_type_identifier" => Some(text(type_node, bytes).to_string()),
        // Any other self-type (tuple, reference, array, …) — no nameable container.
        _ => None,
    }
}

/// Recursive walk dispatching on node kind.
fn walk(node: Node, bytes: &[u8], ctx: &Ctx, out: &mut AnalyzedFile) {
    match node.kind() {
        // ── `use` declarations (incl. `as` aliases, `{…}` groups, glob `*`,
        // `self`/`super`/`crate` prefixes). ──
        "use_declaration" => {
            extract_use(node, bytes, out);
            return;
        }
        // ── Inline module (`mod name { … }`). Its items nest in a
        // `body: (declaration_list …)`, re-walked with the extended module path.
        // Nested modules compose (`mod a { mod b {…} }` → `a::b`). A file module
        // (`mod name;`) has NO body — it declares a sibling file, contributes no
        // symbol here (the indexer indexes that file separately). ──
        "mod_item" => {
            let mod_name = node
                .child_by_field_name("name")
                .map(|n| text(n, bytes))
                .unwrap_or("");
            if let Some(body) = node.child_by_field_name("body") {
                let new_mod = join(ctx.module, mod_name);
                let inner = Ctx {
                    module: &new_mod,
                    container: None,
                    enclosing_fqn: "",
                };
                walk_children(body, bytes, &inner, out);
            }
            return;
        }
        // ── Type declarations: struct / enum / union → Class; trait → Interface. ──
        "struct_item" | "enum_item" | "union_item" => {
            extract_type(node, bytes, ctx, out, NodeKind::Class);
            return;
        }
        "trait_item" => {
            extract_type(node, bytes, ctx, out, NodeKind::Interface);
            return;
        }
        // ── `impl Type { … }` / `impl Trait for Type { … }`: the `fn`s inside are
        // Methods of the self-`Type` (generics stripped). ──
        "impl_item" => {
            extract_impl(node, bytes, ctx, out);
            return;
        }
        // ── A free `fn` (or a `fn` reached at type/module top level). Inside an
        // `impl`/`trait` body, `extract_impl`/`extract_type` set `container`, so a
        // `function_item` there becomes a Method; otherwise a Function. ──
        "function_item" => {
            extract_function(node, bytes, ctx, out);
            return;
        }
        // ── A trait method *signature* (`fn sig(&self) -> T;`, no body) → a Method
        // of the enclosing trait. ──
        "function_signature_item" => {
            extract_function(node, bytes, ctx, out);
            return;
        }
        // ── Call sites. ──
        "call_expression" => {
            if let Some(call) = extract_call(node, bytes, ctx.enclosing_fqn) {
                out.calls.push(call);
            }
            // Still descend (arguments may contain nested calls / closures).
            walk_children(node, bytes, ctx, out);
            return;
        }
        // ── Macro invocations are NOT calls. `foo!(…)` is a `macro_invocation`
        // (distinct from `call_expression`), so it is deliberately NOT matched as a
        // call — we never invent a call to `foo`, and never guess its expansion.
        // We descend into the node, but tree-sitter represents a macro's arguments
        // as an opaque `token_tree` (not parsed sub-expressions), so a real call
        // written as a macro argument (e.g. `dbg!(f())`) is NOT captured — a
        // deliberate, safe miss (never a fabricated call), not a guarantee of
        // capture. Honest under-approximation per §4.1. ──
        "macro_invocation" => {
            walk_children(node, bytes, ctx, out);
            return;
        }
        // ── `macro_rules!` definitions are not symbols this slice. Descending could
        // misread the macro body's token tree as code, so we stop here. ──
        "macro_definition" => {
            return;
        }
        // ── SQL string literals → data-plane `SqlCandidate` (Slice 16, D3, M2). ──
        // A plain `"…"` / raw `r"…"` / `r#"…"#` string literal. A macro that builds
        // dynamic SQL (`format!("… {}", x)`) is honestly dropped — the literal here
        // is only the constant fragment, and `looks_like_sql` gates it. Strings are
        // leaves, so nothing to descend into.
        "string_literal" | "raw_string_literal" => {
            if let Some(cand) = extract_sql_candidate(node, bytes, ctx.enclosing_fqn) {
                out.sql_candidates.push(cand);
            }
            return;
        }
        _ => {}
    }

    walk_children(node, bytes, ctx, out);
}

/// Walk every child of `node` with the given context.
fn walk_children(node: Node, bytes: &[u8], ctx: &Ctx, out: &mut AnalyzedFile) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk(child, bytes, ctx, out);
    }
}

/// Extract a type declaration (struct/enum/union → Class, trait → Interface) and
/// recurse into its body with the type/trait as the new container, so members get
/// module-qualified `Type::member` fqns.
///
/// For a `trait`, the body's `function_signature_item`s (and any default
/// `function_item`s) become Methods of the trait. For a struct/enum/union the body
/// holds only fields/variants (no fns), so the recursion finds no members — the
/// type's *methods* live in separate `impl` blocks, handled by [`extract_impl`].
fn extract_type(node: Node, bytes: &[u8], ctx: &Ctx, out: &mut AnalyzedFile, kind: NodeKind) {
    let Some(name_node) = node.child_by_field_name("name") else {
        walk_children(node, bytes, ctx, out);
        return;
    };
    let name = text(name_node, bytes).to_string();
    let fqn = fqn_of(ctx, &name);

    out.symbols.push(RawSymbol {
        kind,
        name,
        fqn: fqn.clone(),
        container_fqn: None,
        span: span_of(node),
        doc_span: doc_span_of(node, bytes),
    });

    // Members (trait method sigs/defaults) attribute to this type/trait. The
    // enclosing fqn resets — a default method's body sets its own enclosing fqn.
    let inner = Ctx {
        module: ctx.module,
        container: Some(&fqn),
        enclosing_fqn: "",
    };
    if let Some(body) = node.child_by_field_name("body") {
        walk_children(body, bytes, &inner, out);
    }
}

/// Extract an `impl Type { … }` / `impl Trait for Type { … }` block: its `fn`s are
/// Methods of the self-`Type` (the impl's `type:` field, generics stripped). The
/// `trait:` field, when present (`impl Trait for Type`), is NOT used as the
/// container — the methods belong to the concrete `Type` (their receiver) — but it
/// is parsed for the header. An inherent `impl Type` has no `trait:` field.
fn extract_impl(node: Node, bytes: &[u8], ctx: &Ctx, out: &mut AnalyzedFile) {
    let self_type = node
        .child_by_field_name("type")
        .and_then(|t| impl_self_type_name(t, bytes));
    let Some(self_type) = self_type else {
        // No nameable self type (e.g. `impl Trait for (A, B)`) — descend without a
        // container so a nested fn still attributes to the module, never a phantom.
        walk_children(node, bytes, ctx, out);
        return;
    };
    let type_fqn = fqn_of(ctx, &self_type);
    let inner = Ctx {
        module: ctx.module,
        container: Some(&type_fqn),
        enclosing_fqn: "",
    };
    if let Some(body) = node.child_by_field_name("body") {
        walk_children(body, bytes, &inner, out);
    } else {
        walk_children(node, bytes, &inner, out);
    }
}

/// Extract a `fn` (`function_item` or trait `function_signature_item`) as a
/// `Method` (when inside a type/impl/trait container) or a `Function`
/// (module/crate top level). Associated fns without `self` and methods with
/// `self`/`&self`/`&mut self` are both Methods when inside a container — no
/// special-casing beyond the container check. Body calls (if any) attribute to the
/// fn's fqn.
fn extract_function(node: Node, bytes: &[u8], ctx: &Ctx, out: &mut AnalyzedFile) {
    let Some(name_node) = node.child_by_field_name("name") else {
        walk_children(node, bytes, ctx, out);
        return;
    };
    let name = text(name_node, bytes).to_string();
    let fqn = fqn_of(ctx, &name);

    let (kind, container_fqn) = match ctx.container {
        Some(c) if !c.is_empty() => (NodeKind::Method, Some(c.to_string())),
        _ => (NodeKind::Function, None),
    };

    out.symbols.push(RawSymbol {
        kind,
        name,
        fqn: fqn.clone(),
        container_fqn,
        span: span_of(node),
        doc_span: doc_span_of(node, bytes),
    });

    // Body calls attribute to this fn. The container is unchanged (a fn body can
    // declare a closure/nested item, but a nested fn's container is None). A trait
    // signature item has no body — nothing to descend into.
    let inner = Ctx {
        module: ctx.module,
        container: ctx.container,
        enclosing_fqn: &fqn,
    };
    if let Some(body) = node.child_by_field_name("body") {
        walk_children(body, bytes, &inner, out);
    }
}

/// Extract a `use_declaration` into one [`ImportRef`] per bound name.
///
/// The `argument:` field is the path tree. Shapes handled:
/// - `use a::b::c;` → specifier `"a::b::c"`, bound name `c` (the last segment).
/// - `use a::b as e;` → specifier `"a::b"`, bound name `e` (the alias).
/// - `use a::b::{c, d as e};` → one import per list item: `(a::b::c, c)` and
///   `(a::b::d, e)`.
/// - `use a::b::*;` → specifier `"a::b"`, bound name `"*"` (a glob, recorded
///   honestly as importing everything from the path — no per-name binding invented).
/// - `self`/`super`/`crate` prefixes are kept verbatim in the specifier path.
///
/// Each bound name becomes its own `ImportRef` (specifier + single imported name),
/// mirroring how the indexer's import edges key on a (specifier, name) pair.
fn extract_use(node: Node, bytes: &[u8], out: &mut AnalyzedFile) {
    let Some(arg) = node.child_by_field_name("argument") else {
        return;
    };
    let span = span_of(node);
    collect_use_paths(arg, bytes, "", span, out);
}

/// Recursively flatten a `use` path tree into `(specifier, bound_name)` imports.
/// `prefix` is the `::`-joined path accumulated from enclosing `scoped_use_list`s.
fn collect_use_paths(node: Node, bytes: &[u8], prefix: &str, span: Span, out: &mut AnalyzedFile) {
    match node.kind() {
        // A leaf path `a::b::c` (or `crate::Foo`, `super::thing`). Specifier is the
        // full joined path; bound name is its last `::` segment.
        "scoped_identifier" | "identifier" | "crate" | "super" | "self" => {
            let path = join(prefix, text(node, bytes));
            let bound = last_segment(&path).to_string();
            push_import(out, path, bound, span);
        }
        // `a::b::*` glob — import everything from the (joined) base path. The
        // wildcard's base is the `scoped_identifier`/`identifier` child; the bound
        // name is recorded as `"*"` (honest: a glob binds no specific name).
        "use_wildcard" => {
            // The base path is the child that is NOT the `*` token.
            let mut base = String::new();
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if matches!(
                    child.kind(),
                    "scoped_identifier" | "identifier" | "crate" | "super"
                ) {
                    base = join(prefix, text(child, bytes));
                }
            }
            push_import(out, base, "*".to_string(), span);
        }
        // `a::b::{c, d as e}` — the `path:` field is the shared prefix; the `list:`
        // field's items each extend it.
        "scoped_use_list" => {
            let path = node
                .child_by_field_name("path")
                .map(|p| join(prefix, text(p, bytes)))
                .unwrap_or_else(|| prefix.to_string());
            if let Some(list) = node.child_by_field_name("list") {
                let mut cursor = list.walk();
                for item in list.children(&mut cursor) {
                    collect_use_paths(item, bytes, &path, span, out);
                }
            }
        }
        // A bare `{c, d}` list with no shared scoped prefix (rare top-level form).
        "use_list" => {
            let mut cursor = node.walk();
            for item in node.children(&mut cursor) {
                collect_use_paths(item, bytes, prefix, span, out);
            }
        }
        // `c as alias` (inside a list or at top level). The `path:` field is the
        // imported path; `alias:` is the local name.
        "use_as_clause" => {
            let path = node
                .child_by_field_name("path")
                .map(|p| join(prefix, text(p, bytes)))
                .unwrap_or_else(|| prefix.to_string());
            let alias = node
                .child_by_field_name("alias")
                .map(|a| text(a, bytes).to_string())
                .unwrap_or_else(|| last_segment(&path).to_string());
            push_import(out, path, alias, span);
        }
        _ => {}
    }
}

/// Push one `ImportRef` (specifier + a single bound name).
fn push_import(out: &mut AnalyzedFile, specifier: String, bound: String, span: Span) {
    if specifier.is_empty() {
        return;
    }
    out.imports.push(ImportRef {
        specifier,
        imported_names: vec![bound],
        span,
        name_spans: vec![span],
    });
}

/// The last `::`-separated segment of a path (`a::b::C` → `C`, `C` → `C`).
fn last_segment(path: &str) -> &str {
    match path.rfind("::") {
        Some(idx) => &path[idx + 2..],
        None => path,
    }
}

/// Extract a [`SqlCandidate`] from a Rust string literal node when its inner text
/// passes the cheap SQL-keyword prefilter
/// ([`looks_like_sql`](strata_core::looks_like_sql)).
///
/// A regular `string_literal` exposes its body as a `string_content` child; a
/// `raw_string_literal` (`r"…"` / `r#"…"#`) exposes a `string_content` child too.
/// We concatenate the content child(ren) so the inner text is delimiter-free.
fn extract_sql_candidate(node: Node, bytes: &[u8], enclosing_fqn: &str) -> Option<SqlCandidate> {
    let mut literal = String::new();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "string_content" {
            literal.push_str(text(child, bytes));
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

/// Extract a call site from a `call_expression`.
///
/// `function:` is the callee expression. Three attributable shapes:
/// - **`identifier`** (`helper()`) → a bare call, no receiver.
/// - **`scoped_identifier`** (`a::b::c()`, `MyStruct::assoc()`, `Vec::<u32>::new()`,
///   `<Foo as Bar>::baz()`) → callee is the `name:` leaf; the receiver is the full
///   `path:` text (`a::b`, `MyStruct`, the honest qualifier).
/// - **`field_expression`** (`self.m()`, `obj.m()`, `a.b().c()`) → callee is the
///   `field:` leaf; the receiver is the full `value:` text (`self`, `obj`,
///   `a.b()` — the honest receiver chain, recorded verbatim even when it is itself
///   a call).
///
/// Returns `None` for any other callee shape (a nested `call_expression` used
/// directly as `factory()()`, an `index_expression` `map[k]()`, a parenthesized
/// closure) — a computed/dynamic target we never attribute to a static symbol.
/// **Macros never reach here**: a `foo!(…)` is a `macro_invocation`, a different
/// node kind that the walk does not treat as a call.
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
        "scoped_identifier" => {
            let name = function.child_by_field_name("name")?;
            let callee = text(name, bytes).to_string();
            let receiver = function
                .child_by_field_name("path")
                .map(|p| text(p, bytes).to_string());
            Some(CallRef {
                callee_name: callee,
                receiver,
                enclosing_fqn: enclosing_fqn.to_string(),
                span: span_of(node),
                callee_span: span_of(name),
                // `Type::method`, `mod::func`, `Self::m`, `a::b::c` — the `::` path is
                // a type/module qualifier, NOT a value. This is the load-bearing
                // discriminator: it lets the linker resolve `Type::method()` to the
                // exact method on the named type instead of fanning out to every
                // same-named method.
                receiver_is_path: true,
            })
        }
        "field_expression" => {
            let field = function.child_by_field_name("field")?;
            let callee = text(field, bytes).to_string();
            let receiver = function
                .child_by_field_name("value")
                .map(|v| text(v, bytes).to_string());
            Some(CallRef {
                callee_name: callee,
                receiver,
                enclosing_fqn: enclosing_fqn.to_string(),
                span: span_of(node),
                callee_span: span_of(field),
                // `self.m()`, `obj.m()`, `a.b().c()` — the `.` receiver is a *value*
                // (a variable/expr/`self`), not a type. An instance call on an
                // unknown-type receiver, which the linker keeps as an Ambiguous
                // fan-out (resolving the receiver's type needs inference = A3).
                receiver_is_path: false,
            })
        }
        // Any other callee shape (a computed/dynamic target) — not attributed.
        _ => None,
    }
}
