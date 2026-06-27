//! strata-lang-rust: the Rust language adapter for StrataGraph.
//!
//! Two pure capabilities, both free of filesystem/IO, mirroring the
//! `strata-lang-cs` / `strata-lang-py` / `strata-lang-ts` shape:
//!
//! 1. **Extraction** — [`analyze`] parses a Rust source string with Tree-sitter
//!    and returns the symbols, `use` imports, and intra-file calls it found
//!    ([`strata_core::AnalyzedFile`]). Exposed both as a free function and via the
//!    [`RustAnalyzer`] implementation of [`strata_core::LanguageAnalyzer`].
//! 2. **Linking** — [`assemble_rust`] adds a Rust file set's nodes and the
//!    band-disciplined intra-repo call/use edges to a [`Graph`](strata_core::Graph),
//!    resolving *within Rust's own world* (no cross-language linking this slice).
//!    All heuristic confidences are doc-commented constants capped to their
//!    provenance band (design §4.1) — see [`link`].
//!
//! **This is Tree-sitter extraction, NOT rust-analyzer / SCIP.** Compiler-grade
//! Rust precision (full trait resolution, monomorphisation, macro expansion,
//! cross-crate symbol resolution) is a later, compiler-precision slice
//! (the Rust analogue of C#'s Track A3), deliberately out of scope here. This
//! crate is the honest-provenance Tree-sitter plane: every link is a heuristic
//! capped below a RESOLVED fact, and a link we cannot make is surfaced (counted),
//! never invented.
//!
//! **The fqn convention (the ONE chosen, documented, and tested).** A symbol's fqn
//! is **module-path-qualified and type-nested with `::`** — Rust's own path
//! syntax: in `mod outer { struct MyStruct; impl MyStruct { fn method(&self) {} }
//! }` the struct fqn is `outer::MyStruct` and the method fqn is
//! `outer::MyStruct::method`. The (inline) module path is a `::`-joined prefix on
//! every item fqn, and nested modules compose (`mod a { mod b { … } }` → `a::b`).
//! A free `fn` at module top level is `module::fn` (or bare `fn` at crate root). An
//! `impl Type`/`impl Trait for Type` attributes its `fn`s to `Type` (the impl's
//! self type, **generics stripped** — `impl<T> Container<T>` → methods of
//! `Container`), so same-named methods across different `impl`s of different types
//! stay distinct via the type in the fqn. A `trait`'s method signatures are
//! Methods of the *trait*. Pinned by `tests/extraction.rs`.
//!
//! **What is deliberately NOT extracted this slice (the honesty story).** Rust
//! fills the code-plane vecs of `AnalyzedFile` — `symbols`/`imports`/`calls` (plus
//! `sql_candidates` for the data plane). The contract-plane vecs (`routes`,
//! `http_calls`, `gql_documents`, `resolver_entries`) stay **empty**: actix/axum
//! route extraction and `reqwest`/`hyper` consumer calls are a later enhancement,
//! not this slice.
//!
//! - **Macros are NEVER faked into calls.** A `foo!(…)` macro invocation is a
//!   *different AST node* (`macro_invocation`) from a function call
//!   (`call_expression`); the extractor does **not** record it as a call to `foo`,
//!   and it never guesses the macro's expansion. `println!`, `vec!`, `assert!` etc.
//!   contribute no call edge. `macro_rules!` definitions are not symbols this slice.
//!   This is the Rust analogue of the C# plane's reflection-never-invented rule:
//!   the call graph reflects what is statically written, not what a macro might
//!   expand to. See the crate-level honesty notes in [`analyze`] and [`link`].
//! - **`struct`/`enum`/`union` extract as `Class`-kind** (they are "a type with
//!   members / variants / fields" for the code graph); a `trait` is its own
//!   `Interface` kind (its method signatures are its Methods). Documented and
//!   tested, mirroring how the C# plane maps `struct`/`record` → Class and
//!   `interface` → Interface.
//! - **Trait dispatch is never resolved to a concrete impl.** A `t.method()` on a
//!   trait-object/generic receiver, or a bare call with several same-named
//!   candidates across impls, fans out at the AMBIGUOUS band — never a confident
//!   single pick (resolving the concrete impl needs the type system).

mod analyze;
mod link;

pub use analyze::analyze;
pub use link::{assemble_rust, RustLinkCoverage};

use strata_core::{AnalyzedFile, LanguageAnalyzer};

/// The Rust [`LanguageAnalyzer`].
///
/// Stateless; there is a single Rust grammar (no dialect selection), so every
/// supported extension parses identically.
#[derive(Debug, Default, Clone, Copy)]
pub struct RustAnalyzer;

impl LanguageAnalyzer for RustAnalyzer {
    fn extensions(&self) -> &'static [&'static str] {
        &["rs"]
    }

    fn analyze(&self, path: &str, source: &str) -> AnalyzedFile {
        analyze(path, source)
    }
}

#[cfg(test)]
mod wiring_smoke {
    #[test]
    fn parses_rust() {
        // The de-risk, pinned as a test: the grammar must load into core 0.25 and
        // parse a fn+struct+impl file. A regression (grammar/core bump that breaks
        // the ABI) fails here loudly rather than silently emptying every Rust file.
        let mut parser = tree_sitter::Parser::new();
        let lang: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
        parser
            .set_language(&lang)
            .expect("tree-sitter-rust must load into core 0.25 (de-risk)");
        let tree = parser
            .parse("struct S; impl S { fn m(&self) {} }", None)
            .expect("parse must succeed");
        let root = tree.root_node();
        assert!(!root.has_error(), "clean parse expected");
        let sexp = root.to_sexp();
        assert!(
            sexp.contains("struct_item")
                && sexp.contains("impl_item")
                && sexp.contains("function_item"),
            "sexp was: {sexp}"
        );
    }

    #[test]
    fn analyzer_trait_extracts_a_method() {
        use strata_core::{LanguageAnalyzer, NodeKind};
        let analyzer = super::RustAnalyzer;
        assert!(analyzer.extensions().contains(&"rs"));
        let file = analyzer.analyze("src/a.rs", "struct A; impl A { fn foo(&self) {} }");
        // The struct (as Class) and its method are both extracted.
        assert!(file
            .symbols
            .iter()
            .any(|s| s.name == "A" && s.kind == NodeKind::Class));
        assert!(file
            .symbols
            .iter()
            .any(|s| s.name == "foo" && s.kind == NodeKind::Method));
    }
}
