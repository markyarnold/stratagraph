//! strata-lang-cs: the C# language adapter for StrataGraph.
//!
//! Two pure capabilities, both free of filesystem/IO, mirroring the
//! `strata-lang-py` / `strata-lang-ts` shape:
//!
//! 1. **Extraction** — [`analyze`] parses a C# source string with Tree-sitter and
//!    returns the symbols, usings, and intra-file calls it found
//!    ([`strata_core::AnalyzedFile`]). Exposed both as a free function and via the
//!    [`CsAnalyzer`] implementation of [`strata_core::LanguageAnalyzer`].
//! 2. **Linking** — [`assemble_csharp`] adds a C# file set's nodes and the
//!    band-disciplined intra-repo call/using edges to a [`Graph`], resolving
//!    *within C#'s own world* (no cross-language linking this slice). All
//!    heuristic confidences are doc-commented constants capped to their provenance
//!    band (design §4.1) — see [`link`].
//!
//! **This is Tree-sitter extraction, NOT Roslyn.** Compiler-grade C# precision
//! (full overload resolution, generic instantiation, `partial`-type merging,
//! cross-assembly symbol resolution) is **Track A3 / Roslyn**, deliberately out of
//! scope here. This crate is the honest-provenance Tree-sitter plane: every link
//! is a heuristic capped below a RESOLVED fact, and a link we cannot make is
//! surfaced (counted), never invented.
//!
//! **Namespace fqn convention (the ONE chosen, documented, and tested).** A
//! symbol's fqn is **namespace-qualified and type-nested with `.`**: in
//! `namespace App.Services { class Worker { void Run() {} } }` the class fqn is
//! `App.Services.Worker` and the method fqn is `App.Services.Worker.Run`. The
//! namespace (file-scoped or block) is a dotted prefix on every type/member fqn;
//! a method's `container_fqn` is its enclosing type's (namespace-qualified) fqn.
//! This mirrors the Python plane's dotted-container convention and matches C#'s
//! own fully-qualified-name syntax. It is pinned by
//! `tests/extraction.rs::namespace_qualifies_type_and_member_fqns`.
//!
//! **What is deliberately NOT extracted this slice (the honesty story).** C# fills
//! the code-plane vecs of `AnalyzedFile` — `symbols`/`imports`/`calls`. The
//! contract-plane vecs (`routes`, `http_calls`, `gql_documents`,
//! `resolver_entries`) stay **empty**: ASP.NET routing attributes and
//! `HttpClient`/`HttpRequestMessage` consumer calls are a later enhancement, not
//! this slice. Overloads **collapse to one node by name** (the flat-fqn precedent
//! from Python; arity-aware splitting is an A3/Roslyn refinement). Reflection
//! (`GetMethod`, `dynamic`, delegate indirection) is **never guessed**. See the
//! crate-level honesty notes in [`analyze`] and [`link`].

mod analyze;
mod link;

pub use analyze::analyze;
pub use link::{assemble_csharp, CsLinkCoverage};

use strata_core::{AnalyzedFile, LanguageAnalyzer};

/// The C# [`LanguageAnalyzer`].
///
/// Stateless; there is a single C# grammar (no dialect selection), so every
/// supported extension parses identically. `.csx` (C# script) shares the grammar
/// and is included so a script file is at least walked, never crashed on.
#[derive(Debug, Default, Clone, Copy)]
pub struct CsAnalyzer;

impl LanguageAnalyzer for CsAnalyzer {
    fn extensions(&self) -> &'static [&'static str] {
        &["cs", "csx"]
    }

    fn analyze(&self, path: &str, source: &str) -> AnalyzedFile {
        analyze(path, source)
    }
}

#[cfg(test)]
mod wiring_smoke {
    #[test]
    fn parses_csharp() {
        // The de-risk, pinned as a test: the grammar must load into core 0.25 and
        // parse a class+method file. A regression (grammar/core bump that breaks
        // the ABI) fails here loudly rather than silently emptying every C# file.
        let mut parser = tree_sitter::Parser::new();
        let lang: tree_sitter::Language = tree_sitter_c_sharp::LANGUAGE.into();
        parser
            .set_language(&lang)
            .expect("tree-sitter-c-sharp must load into core 0.25 (de-risk)");
        let tree = parser
            .parse("class C { void M() {} }", None)
            .expect("parse must succeed");
        let sexp = tree.root_node().to_sexp();
        assert!(
            sexp.contains("class_declaration") && sexp.contains("method_declaration"),
            "sexp was: {sexp}"
        );
    }

    #[test]
    fn analyzer_trait_extracts_a_method() {
        use strata_core::{LanguageAnalyzer, NodeKind};
        let analyzer = super::CsAnalyzer;
        assert!(analyzer.extensions().contains(&"cs"));
        let file = analyzer.analyze("src/A.cs", "class A { void Foo() {} }");
        // The class and its method are both extracted.
        assert!(file
            .symbols
            .iter()
            .any(|s| s.name == "A" && s.kind == NodeKind::Class));
        assert!(file
            .symbols
            .iter()
            .any(|s| s.name == "Foo" && s.kind == NodeKind::Method));
    }
}
