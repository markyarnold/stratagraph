//! Plain data types that make up the `ScipResolver` query surface.
//!
//! These deal only in SCIP concepts (files, positions, symbol monikers) and
//! carry no dependency on `strata-core` — milestone 2 bridges them onto graph
//! nodes.

/// A 0-based source position.
///
/// `character` is a **UTF-16 code-unit** offset from the start of the line,
/// matching the encoding `scip-typescript` emits. This was determined
/// empirically from the non-ASCII fixture (spec A3): in `café`, the four-unit
/// identifier produces an end character of `start + 4`, i.e. one unit for `é`
/// (UTF-16), not two (which UTF-8 bytes would give). The resolver compares
/// caller positions against occurrence ranges in these same units, so a caller
/// that supplies UTF-16 offsets (as an LSP/Tree-sitter UTF-16 column does)
/// aligns exactly. See the `unicode` alignment test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    /// 0-based line number.
    pub line: u32,
    /// 0-based UTF-16 code-unit offset from the start of the line.
    pub character: u32,
}

impl Position {
    /// Construct a position from a line and a UTF-16 code-unit character offset.
    pub fn new(line: u32, character: u32) -> Self {
        Self { line, character }
    }
}

/// The result of resolving a source position to the symbol it references.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedTarget {
    /// The SCIP symbol string (a globally-stable moniker).
    pub moniker: String,
    /// Relative path of the definition, if a first-party definition exists.
    ///
    /// `None` when the symbol has no definition occurrence in the indexed
    /// project (e.g. an external `node_modules`/lib symbol).
    pub def_file: Option<String>,
    /// Start of the definition occurrence, if one is known.
    pub def_position: Option<Position>,
    /// `true` when the symbol has no first-party definition occurrence.
    pub is_external: bool,
}

/// Coarse diagnostics about a parsed index (feeds coverage metrics later).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScipStats {
    /// Number of documents (files) in the index.
    pub documents: usize,
    /// Total number of occurrences across all documents.
    pub occurrences: usize,
    /// Number of distinct symbols with at least one definition occurrence.
    pub definitions: usize,
}
