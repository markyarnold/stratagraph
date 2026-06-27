//! strata-lang-ts: TypeScript/JavaScript language adapter for StrataGraph.
//!
//! Two pure capabilities, both free of filesystem/IO:
//!
//! 1. **Extraction** — [`analyze`] parses a source string with Tree-sitter and
//!    returns the symbols, imports, and intra-file calls it found
//!    ([`strata_core::AnalyzedFile`]). Exposed both as a free function and via
//!    the [`TsAnalyzer`] implementation of [`strata_core::LanguageAnalyzer`].
//! 2. **Module resolution** — [`resolve`] implements the TS/JS module
//!    resolution algorithm over a [`ModuleFs`] trait, so it is unit-testable
//!    without touching a real filesystem. The indexer (milestone 4) supplies a
//!    real `ModuleFs` and builds [`ResolveOptions`] from tsconfig.

mod analyze;
mod resolve;

pub use analyze::analyze;
pub use resolve::{resolve, ModuleFs, ResolveOptions, ResolveResult};

use strata_core::{AnalyzedFile, LanguageAnalyzer};

/// The TypeScript/JavaScript [`LanguageAnalyzer`].
///
/// Stateless; grammar selection happens per call based on the file extension.
#[derive(Debug, Default, Clone, Copy)]
pub struct TsAnalyzer;

impl LanguageAnalyzer for TsAnalyzer {
    fn extensions(&self) -> &'static [&'static str] {
        &["ts", "tsx", "js", "jsx", "mjs", "cjs"]
    }

    fn analyze(&self, path: &str, source: &str) -> AnalyzedFile {
        analyze(path, source)
    }
}

#[cfg(test)]
mod wiring_smoke {
    #[test]
    fn parses_typescript() {
        let mut parser = tree_sitter::Parser::new();
        let lang: tree_sitter::Language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
        parser.set_language(&lang).unwrap();
        let tree = parser.parse("function f(){}", None).unwrap();
        let sexp = tree.root_node().to_sexp();
        assert!(sexp.contains("function_declaration"), "sexp was: {sexp}");
    }

    #[test]
    fn parses_javascript() {
        let mut parser = tree_sitter::Parser::new();
        let lang: tree_sitter::Language = tree_sitter_javascript::LANGUAGE.into();
        parser.set_language(&lang).unwrap();
        let tree = parser.parse("function f(){}", None).unwrap();
        let sexp = tree.root_node().to_sexp();
        assert!(sexp.contains("function_declaration"), "sexp was: {sexp}");
    }

    #[test]
    fn analyzer_trait_extracts_a_function() {
        use strata_core::{LanguageAnalyzer, NodeKind};
        let analyzer = super::TsAnalyzer;
        assert!(analyzer.extensions().contains(&"ts"));
        let file = analyzer.analyze("src/a.ts", "function foo() {}");
        assert_eq!(file.symbols.len(), 1);
        assert_eq!(file.symbols[0].name, "foo");
        assert_eq!(file.symbols[0].kind, NodeKind::Function);
    }
}
