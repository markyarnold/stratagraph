//! strata-lang-py: the Python language adapter for StrataGraph.
//!
//! Two pure capabilities, both free of filesystem/IO, mirroring the
//! `strata-lang-ts` shape:
//!
//! 1. **Extraction** — [`analyze`] parses a Python source string with
//!    Tree-sitter and returns the symbols, imports, intra-file calls, data-plane
//!    signals (SQL/ORM), and contract-plane signals (routes, …) it found
//!    ([`strata_core::AnalyzedFile`]). Exposed both as a free function and via the
//!    [`PyAnalyzer`] implementation of [`strata_core::LanguageAnalyzer`].
//! 2. **Linking** — [`assemble_python`] adds a Python file set's nodes and the
//!    band-disciplined intra-repo call/import/inheritance edges to a [`Graph`],
//!    resolving *within Python's own world* (no cross-language linking this
//!    slice). All heuristic confidences are doc-commented constants capped to
//!    their provenance band (design §4.1) — see [`link`].
//!
//! **Contract-plane extraction (the honesty story).** Python web frameworks are
//! recognised so the shared contract linker can build producer/consumer edges:
//! producer **routes** (`routes` — Flask `@app.route`/`@app.get`, FastAPI
//! `@router.get`, Django `path()`/`re_path()` URLconf), REST consumer **calls**
//! (`http_calls` — `requests`/`httpx`), GraphQL consumer **documents**
//! (`gql_documents` — `gql("…")`), and GraphQL producer **resolvers**
//! (`resolver_entries` — Graphene, Strawberry, Ariadne). Extraction stays
//! conservative throughout — a missed framework idiom is acceptable degradation, an
//! invented edge is not (R1/R5). A Python file participates fully in the code graph,
//! the data plane, the infra `Runs` bridge, and the contract plane.

mod analyze;
mod link;

pub use analyze::analyze;
pub use link::{assemble_python, PyLinkCoverage};

use strata_core::{AnalyzedFile, LanguageAnalyzer};

/// The Python [`LanguageAnalyzer`].
///
/// Stateless; there is a single Python grammar (no dialect selection like the
/// TS/JS `.ts`/`.tsx` split), so every supported extension parses identically.
#[derive(Debug, Default, Clone, Copy)]
pub struct PyAnalyzer;

impl LanguageAnalyzer for PyAnalyzer {
    fn extensions(&self) -> &'static [&'static str] {
        // `.pyi` stub files share the grammar; included so a typed stub is at
        // least walked, never crashed on. `.py` is the dominant case.
        &["py", "pyi"]
    }

    fn analyze(&self, path: &str, source: &str) -> AnalyzedFile {
        analyze(path, source)
    }
}

#[cfg(test)]
mod wiring_smoke {
    #[test]
    fn parses_python() {
        let mut parser = tree_sitter::Parser::new();
        let lang: tree_sitter::Language = tree_sitter_python::LANGUAGE.into();
        parser.set_language(&lang).unwrap();
        let tree = parser.parse("def f():\n    pass\n", None).unwrap();
        let sexp = tree.root_node().to_sexp();
        assert!(sexp.contains("function_definition"), "sexp was: {sexp}");
    }

    #[test]
    fn analyzer_trait_extracts_a_function() {
        use strata_core::{LanguageAnalyzer, NodeKind};
        let analyzer = super::PyAnalyzer;
        assert!(analyzer.extensions().contains(&"py"));
        let file = analyzer.analyze("src/a.py", "def foo():\n    pass\n");
        assert_eq!(file.symbols.len(), 1);
        assert_eq!(file.symbols[0].name, "foo");
        assert_eq!(file.symbols[0].kind, NodeKind::Function);
    }
}
