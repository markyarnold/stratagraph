//! strata-core: domain model, in-memory graph, and traversal engine.
//! Pure Rust, no parsing or IO dependencies. Correctness lives here.

pub mod analyze;
pub mod graph;
pub mod ids;
pub mod model;
pub mod traverse;

pub use analyze::{
    looks_like_sql, AnalyzedFile, CallRef, GqlDocument, HttpCall, ImportRef, LanguageAnalyzer,
    OrmFramework, OrmModelHint, RawSymbol, ResolverEntry, RouteDecl, SqlCandidate, UrlShape,
    ANALYZER_SCHEMA_VERSION, ROUTE_METHOD_ANY,
};
pub use graph::{Direction, Graph};
pub use ids::Uid;
pub use model::{Confidence, Edge, EdgeKind, Node, NodeKind, Provenance, Span};
pub use traverse::query;
pub use traverse::{context, ContextResult};
pub use traverse::{explain, Explanation, PathHop};
pub use traverse::{
    impact, will_break_label, AffectedNode, ImpactOptions, ImpactResult, MemberDependent,
    DEFAULT_WILL_BREAK_CONFIDENCE,
};

/// The engine build id (short git hash, `-dirty` suffixed when the tree had
/// local changes, `unknown` outside a git checkout) — stamped at compile time
/// by `build.rs`. Surfaced by `strata --version`, the index summary, and the
/// desktop app so version skew between a running binary and the repo is
/// visible, never spooky.
pub const ENGINE_ID: &str = env!("STRATA_ENGINE_ID");

#[cfg(test)]
mod smoke {
    #[test]
    fn workspace_builds() {
        assert_eq!(2 + 2, 4);
    }
}
