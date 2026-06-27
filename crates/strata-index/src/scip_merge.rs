//! Precise (SCIP) resolution overlay for the heuristic graph builder.
//!
//! The accuracy-critical bridge between two coordinate systems:
//!   * Tree-sitter spans (from `strata-lang-ts`) carry **byte** columns;
//!   * SCIP [`Position`]s carry **UTF-16 code-unit** columns.
//!
//! [`utf16_offset`] converts the former to the latter at each call/import site
//! so a position can be looked up in a [`ScipResolver`]. [`SiteResolver`] then
//! maps the resolved SCIP target back onto a graph node (or the external
//! `Package` node) — by `(def_file, def_line, symbol-name)`, with an
//! overload-tolerant `(def_file, symbol-name)` fallback for when SCIP's
//! definition line (an overload signature) differs from the extractor's (the
//! implementation).

use std::collections::{BTreeMap, HashMap};

use strata_core::{AnalyzedFile, Uid};
use strata_scip::{Position, ResolvedTarget, ScipResolver};

use crate::build::{uid_module, uid_package};

/// Count the UTF-16 code units in `line_text` that precede byte column
/// `byte_col`.
///
/// Tree-sitter reports a 0-based **byte** column on a line; SCIP/LSP want a
/// 0-based **UTF-16 code-unit** column. For ASCII the two coincide; they
/// diverge on any line containing a character outside the Basic Latin block
/// (e.g. `é` is 2 UTF-8 bytes but 1 UTF-16 unit; an emoji is 4 bytes but 2
/// UTF-16 units).
///
/// Robustness: a `byte_col` past the end of the line is clamped to the line
/// length, and one that falls *inside* a multi-byte character is rounded down to
/// the nearest char boundary — so a slightly-off column never panics and never
/// over-counts.
pub fn utf16_offset(line_text: &str, byte_col: usize) -> u32 {
    let clamped = byte_col.min(line_text.len());
    // Round down to a char boundary so slicing is always valid.
    let boundary = (0..=clamped)
        .rev()
        .find(|&i| line_text.is_char_boundary(i))
        .unwrap_or(0);
    let mut units: u32 = 0;
    for ch in line_text[..boundary].chars() {
        units += ch.len_utf16() as u32;
    }
    units
}

/// Build a SCIP [`Position`] for a 0-based `line` and a 0-based Tree-sitter
/// **byte** column, converting the column to UTF-16 code units using the
/// source line's text.
pub fn scip_position(line_text: &str, line0: u32, byte_col: usize) -> Position {
    Position::new(line0, utf16_offset(line_text, byte_col))
}

/// What a SCIP target maps to in our graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MappedTarget {
    /// A first-party node (a symbol or, for a namespace import, a module).
    Node(Uid),
    /// An external symbol with no first-party definition: the `Package` node.
    External(Uid),
}

impl MappedTarget {
    /// The target node's `Uid`, without materializing anything in the graph.
    ///
    /// For a first-party `Node` this is the existing node's uid; for an
    /// `External` it is the `Package` node's uid (the builder adds that node
    /// idempotently, but the *identity* is already determined here). The
    /// differential harness uses this to record the SCIP target a builder run
    /// would point at, guaranteeing the two agree (drift test).
    pub fn uid(&self) -> &Uid {
        match self {
            MappedTarget::Node(uid) | MappedTarget::External(uid) => uid,
        }
    }
}

/// Extract the trailing symbol name from a SCIP moniker.
///
/// scip-typescript / scip-python monikers look like
/// `scip-typescript npm <pkg> <ver> src/`b.ts`/foo().` — the part after the
/// backtick-quoted file segment is a chain of descriptors. We want the *last*
/// named descriptor:
///   * `…/foo().`            → `foo`   (function)
///   * `…/Widget#save().`    → `save`  (method on a type)
///   * `…/Widget#`           → `Widget` (type)
///   * ``…/`café`().``       → `café`  (backtick-quoted identifier)
///   * `…/`b.ts`/`           → `None`  (a module — no trailing name)
///
/// **rust-analyzer impl-method shape (Track C1).** rust-analyzer encodes an
/// inherent impl method as `…/impl#[Type]method().` and a trait-impl method as
/// `…/impl#[Type][Trait]method().`. The descriptor before the `(` is then
/// `impl#[Type]method` / `impl#[Type][Trait]method`, whose segment after the `#`
/// starts with `[` — not an identifier. The extractor strips the leading
/// `[Type]`/`[Type][Trait]` bracket groups and takes the trailing identifier
/// (`method`). TypeScript and Python monikers never contain `[` in a descriptor
/// segment, so this path leaves them untouched (the cases above are unchanged).
///
/// Returns `None` for a module (or any shape without a trailing named
/// descriptor), in which case the caller maps to the module node.
pub fn symbol_name_from_moniker(moniker: &str) -> Option<String> {
    // Drop a trailing descriptor terminator '.' (e.g. the `.` in `foo().`).
    let trimmed = moniker.trim_end_matches('.');
    if trimmed.is_empty() {
        return None;
    }
    // Method/function: the name sits immediately before the last '('.
    if let Some(paren) = trimmed.rfind('(') {
        // Guard against the parameter form `foo().(param)` where the char before
        // '(' is '.' — that has no name we want to match a symbol on.
        let before = &trimmed[..paren];
        if !before.is_empty() && !before.ends_with('.') {
            return last_descriptor_ident(before);
        }
        return None;
    }
    // Type: the name sits immediately before the last '#'.
    if let Some(hash) = trimmed.rfind('#') {
        return last_descriptor_ident(&trimmed[..hash]);
    }
    // Anything else (a module path ending in '/', a meta descriptor, …): no name.
    None
}

/// The identifier of the last descriptor in `s`, where descriptors are separated
/// by `/` or `#`. The identifier may be backtick-quoted (then the quotes are
/// stripped). Returns `None` if it does not look like an identifier.
fn last_descriptor_ident(s: &str) -> Option<String> {
    let s = s.trim_end();
    // Backtick-quoted identifier: take the text between the final pair of `.
    if s.ends_with('`') {
        let inner_end = s.len() - 1;
        let open = s[..inner_end].rfind('`')?;
        let name = &s[open + 1..inner_end];
        return (!name.is_empty()).then(|| name.to_string());
    }
    // Plain identifier: text after the last '/', '#', or ' '.
    let start = s.rfind(['/', '#', ' ']).map(|i| i + 1).unwrap_or(0);
    let mut name = &s[start..];
    // rust-analyzer impl-method shape: the segment after the `#` is one or more
    // `[Type]`/`[Trait]` bracket groups followed by the method name —
    // `[Rectangle]area`, `[Rectangle][Shape]describe`. When the segment STARTS
    // with `[`, the real identifier is everything after the final `]`; strip the
    // bracket groups. (A scip-typescript/python descriptor never starts a segment
    // with `[`, so this leaves every TS/Python moniker untouched.)
    if name.starts_with('[') {
        if let Some(close) = name.rfind(']') {
            name = &name[close + 1..];
        }
    }
    if name.is_empty() || !name.chars().next().is_some_and(is_ident_start) {
        return None;
    }
    Some(name.to_string())
}

fn is_ident_start(c: char) -> bool {
    c == '_' || c == '$' || c.is_alphabetic()
}

/// Resolves call/import sites against a parsed SCIP index and maps the resulting
/// target onto a graph node. Holds the lookup tables needed to do that purely
/// (no IO): a `(file, def_line_1based, name) → Uid` index of first-party symbol
/// nodes and a `file → module Uid` index.
pub struct SiteResolver<'a> {
    scip: &'a ScipResolver,
    sources: &'a BTreeMap<String, String>,
    nodes_by_loc: HashMap<(String, u32, String), Uid>,
    /// Overload fallback for `nodes_by_loc`: `(file, simple-name)` → the symbol
    /// node, recorded only when that name is **unique** in the file (`None`
    /// marks an ambiguous name the fallback refuses to guess between). Built and
    /// consumed by `match_named_def`.
    by_file_name: HashMap<(String, String), Option<Uid>>,
    module_by_file: HashMap<String, Uid>,
}

impl<'a> SiteResolver<'a> {
    /// Build the lookup tables from the analyzed file set.
    pub fn new(
        scip: &'a ScipResolver,
        sources: &'a BTreeMap<String, String>,
        repo_name: &str,
        analyzed: &BTreeMap<String, AnalyzedFile>,
    ) -> SiteResolver<'a> {
        let SymbolIndices {
            nodes_by_loc,
            by_file_name,
            module_by_file,
        } = build_symbol_indices(repo_name, analyzed);
        SiteResolver {
            scip,
            sources,
            nodes_by_loc,
            by_file_name,
            module_by_file,
        }
    }

    /// Resolve a site at `(file, line0, byte_col)` (0-based line, Tree-sitter
    /// byte column of the callee/imported-name identifier) and map it onto a
    /// graph target. Returns `None` when SCIP does not cover the position or the
    /// target cannot be mapped — the caller then falls back to the heuristic.
    pub fn resolve_site(&self, file: &str, line0: u32, byte_col: usize) -> Option<MappedTarget> {
        let line_text = nth_line(self.sources.get(file)?, line0)?;
        let pos = scip_position(line_text, line0, byte_col);
        let target = self.scip.resolve_at(file, pos)?;
        self.map_target(&target)
    }

    /// Map a [`ResolvedTarget`] onto a graph node (or external `Package`).
    fn map_target(&self, target: &ResolvedTarget) -> Option<MappedTarget> {
        if target.is_external {
            // No first-party definition: target the external Package node, keyed
            // by the package name carried in the moniker.
            let pkg = package_from_moniker(&target.moniker)?;
            return Some(MappedTarget::External(uid_package(&pkg)));
        }
        let def_file = target.def_file.as_deref()?;
        let def_pos = target.def_position?;
        let def_line_1based = def_pos.line + 1;

        match symbol_name_from_moniker(&target.moniker) {
            // A named symbol: match (file, def line, name) onto a symbol node,
            // tolerant of overloads (SCIP's signature line vs our impl line).
            Some(name) => match_named_def(
                &self.nodes_by_loc,
                &self.by_file_name,
                def_file,
                def_line_1based,
                &name,
            )
            .map(MappedTarget::Node),
            // No trailing name → a module (e.g. a `* as NS` namespace import).
            None => self
                .module_by_file
                .get(def_file)
                .cloned()
                .map(MappedTarget::Node),
        }
    }
}

/// The symbol lookup tables a [`SiteResolver`] resolves SCIP targets against.
/// Split out from [`SiteResolver::new`] so the index construction — in
/// particular the overload `by_file_name` ambiguity collapse — is unit-tested
/// without needing a parsed SCIP index.
struct SymbolIndices {
    /// Exact `(file, 1-based decl line, simple-name)` → symbol node.
    nodes_by_loc: HashMap<(String, u32, String), Uid>,
    /// Overload fallback `(file, simple-name)` → node, only when unique in the
    /// file (`None` = the name is ambiguous; never guessed between).
    by_file_name: HashMap<(String, String), Option<Uid>>,
    /// `file` → module node.
    module_by_file: HashMap<String, Uid>,
}

/// Build the [`SymbolIndices`] from the analyzed file set (pure; no IO).
fn build_symbol_indices(
    repo_name: &str,
    analyzed: &BTreeMap<String, AnalyzedFile>,
) -> SymbolIndices {
    let mut nodes_by_loc: HashMap<(String, u32, String), Uid> = HashMap::new();
    let mut by_file_name: HashMap<(String, String), Option<Uid>> = HashMap::new();
    let mut module_by_file: HashMap<String, Uid> = HashMap::new();
    for (path, file) in analyzed {
        module_by_file.insert(path.clone(), uid_module(repo_name, path));
        for sym in &file.symbols {
            let uid = Uid::new("ts", repo_name, path, &sym.fqn, "");
            // Exact key: the declaration's 1-based start line + simple name (SCIP
            // def positions are 0-based, reconciled in `map_target`). First
            // symbol at a given (file, line, name) wins.
            nodes_by_loc
                .entry((path.clone(), sym.span.start_line, sym.name.clone()))
                .or_insert_with(|| uid.clone());
            // Name-in-file index for the overload fallback: record the uid the
            // first time we see (file, name); collapse to `None` (ambiguous) if a
            // DISTINCT uid shares the same (file, name), so genuine same-name
            // shadows are never merged on a guess.
            by_file_name
                .entry((path.clone(), sym.name.clone()))
                .and_modify(|slot| {
                    if slot.as_ref() != Some(&uid) {
                        *slot = None;
                    }
                })
                .or_insert_with(|| Some(uid.clone()));
        }
    }
    SymbolIndices {
        nodes_by_loc,
        by_file_name,
        module_by_file,
    }
}

/// Map a SCIP definition at `(def_file, def_line_1based, name)` onto a symbol
/// node, tolerant of TypeScript **overloads**.
///
/// The exact `(file, line, name)` key is tried first: it is always correct and
/// keeps genuinely-distinct same-named symbols (shadows on different lines)
/// apart. It misses only when SCIP's recorded definition line differs from the
/// line the extractor recorded for the *same* symbol — the canonical case being
/// an overloaded function, where SCIP points at the first **signature** line
/// while our extractor (which matches only `function_declaration`) records the
/// **implementation** line. We then fall back to a UNIQUE `(file, name)` match —
/// the same symbol whichever overload line SCIP chose. The fallback **declines**
/// (returns `None`) when the name is not unique in the file, so two distinct
/// same-named definitions are never merged on a guess.
fn match_named_def(
    nodes_by_loc: &HashMap<(String, u32, String), Uid>,
    by_file_name: &HashMap<(String, String), Option<Uid>>,
    def_file: &str,
    def_line_1based: u32,
    name: &str,
) -> Option<Uid> {
    if let Some(uid) = nodes_by_loc.get(&(def_file.to_string(), def_line_1based, name.to_string()))
    {
        return Some(uid.clone());
    }
    // Exact miss → only a UNIQUE (file, name) is safe to bind (overload tolerance).
    by_file_name
        .get(&(def_file.to_string(), name.to_string()))
        .cloned()
        .flatten()
}

/// The package name carried by an external scip-typescript moniker, i.e. the
/// third whitespace-delimited token in `scip-typescript npm <pkg> <ver> …`.
fn package_from_moniker(moniker: &str) -> Option<String> {
    let mut parts = moniker.split_whitespace();
    let _scheme = parts.next()?; // "scip-typescript"
    let _manager = parts.next()?; // "npm"
    let pkg = parts.next()?; // the package name
    (!pkg.is_empty()).then(|| pkg.to_string())
}

/// The `line0`-th (0-based) line of `text`, without the trailing newline.
fn nth_line(text: &str, line0: u32) -> Option<&str> {
    text.split('\n').nth(line0 as usize).map(|l| {
        // `split('\n')` already excludes '\n'; strip a trailing '\r' too.
        l.strip_suffix('\r').unwrap_or(l)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test 7: ASCII lines map byte columns 1:1 to UTF-16 offsets.
    #[test]
    fn utf16_offset_ascii_is_identity() {
        let line = "export function run() { bar(); }";
        assert_eq!(utf16_offset(line, 0), 0);
        assert_eq!(utf16_offset(line, 24), 24); // `bar` starts at byte 24
        assert_eq!(utf16_offset(line, line.len()), line.len() as u32);
    }

    // Test 7 (cont.): non-ASCII lines diverge — each multi-byte char counts as
    // its UTF-16 unit width, not its byte width.
    #[test]
    fn utf16_offset_non_ascii_counts_code_units() {
        // `é` is U+00E9: 2 UTF-8 bytes, 1 UTF-16 unit.
        // Line: `café(); nf();`  bytes: c(0)a(1)f(2)é(3,4)((5))(6);(7) (8)n(9)f(10)
        let line = "café(); nf();";
        // Before `é` (byte 3): "caf" = 3 units.
        assert_eq!(utf16_offset(line, 3), 3);
        // After `é` (byte 5, the `(`): "café" = 4 units (NOT 5 bytes).
        assert_eq!(utf16_offset(line, 5), 4);
        // `nf` starts at byte 9 → "café(); " = 4 + 4 = 8 units (byte 9 - 1 for é).
        assert_eq!(utf16_offset(line, 9), 8);
    }

    // The exact divergence the fixture's `uni.ts` relies on: a callee after two
    // accented identifiers. Byte col 49 of `nf` maps to UTF-16 col 47.
    #[test]
    fn utf16_offset_matches_fixture_uni_line() {
        let line = "export function uniCaller() { café(); naïve(); nf(); }";
        // café callee at byte 30 → 30 (only ASCII before it).
        assert_eq!(utf16_offset(line, 30), 30);
        // naïve callee at byte 39 → 38 (one é before it).
        assert_eq!(utf16_offset(line, 39), 38);
        // nf callee at byte 49 → 47 (two accented chars before it).
        assert_eq!(utf16_offset(line, 49), 47);
    }

    // An astral-plane character (emoji) is 4 UTF-8 bytes and 2 UTF-16 units.
    #[test]
    fn utf16_offset_handles_surrogate_pairs() {
        let line = "x = \"😀\"; y();";
        // Before the emoji (byte 5, the opening quote is at 4, emoji at 5..9).
        // "x = \"" = 5 ASCII bytes/units.
        assert_eq!(utf16_offset(line, 5), 5);
        // After the emoji (byte 9): 5 + 2 surrogate units = 7.
        assert_eq!(utf16_offset(line, 9), 7);
    }

    // Robustness: past-end clamps; inside-a-char rounds down (no panic).
    #[test]
    fn utf16_offset_is_robust_to_bad_columns() {
        let line = "café";
        assert_eq!(
            utf16_offset(line, 1000),
            4,
            "past end clamps to line length"
        );
        // Byte 4 is INSIDE `é` (bytes 3..5); round down to byte 3 → "caf" = 3.
        assert_eq!(utf16_offset(line, 4), 3, "inside a char rounds down");
    }

    #[test]
    fn scip_position_carries_line_and_converted_col() {
        let pos = scip_position("café(); nf();", 7, 9);
        assert_eq!(pos.line, 7);
        assert_eq!(pos.character, 8);
    }

    // ── Rust impl-method moniker extraction (the C1 shim) ─────────────────────
    //
    // rust-analyzer encodes an impl method as `…/impl#[Type]method().` and a
    // trait-impl method as `…/impl#[Type][Trait]method().`. The descriptor before
    // the `(` is then `impl#[Type]method` / `impl#[Type][Trait]method`, whose last
    // segment (after the `#`) STARTS WITH `[` — not an identifier — so the
    // extractor must strip the `impl#`-prefixed bracket segments and take the
    // trailing identifier. TS/Python monikers never contain `impl#[`, so they are
    // untouched.

    #[test]
    fn rust_impl_method_moniker_yields_method_name() {
        // Inherent impl methods.
        assert_eq!(
            symbol_name_from_moniker(
                "rust-analyzer cargo shapes 0.0.0 models/impl#[Rectangle]new()."
            ),
            Some("new".to_string())
        );
        assert_eq!(
            symbol_name_from_moniker(
                "rust-analyzer cargo shapes 0.0.0 models/impl#[Rectangle]area()."
            ),
            Some("area".to_string())
        );
        assert_eq!(
            symbol_name_from_moniker(
                "rust-analyzer cargo shapes 0.0.0 models/impl#[Rectangle]scale()."
            ),
            Some("scale".to_string())
        );
    }

    #[test]
    fn rust_trait_impl_method_moniker_yields_method_name() {
        // `impl Trait for Type` method: two bracket segments before the name.
        assert_eq!(
            symbol_name_from_moniker(
                "rust-analyzer cargo shapes 0.0.0 models/impl#[Rectangle][Shape]describe()."
            ),
            Some("describe".to_string())
        );
        assert_eq!(
            symbol_name_from_moniker(
                "rust-analyzer cargo shapes 0.0.0 models/impl#[Circle][Shape]describe()."
            ),
            Some("describe".to_string())
        );
    }

    #[test]
    fn rust_non_impl_monikers_unchanged() {
        // A free fn, a type, and a module still extract exactly as before — the
        // shim only fires on the `impl#[…]` shape.
        assert_eq!(
            symbol_name_from_moniker("rust-analyzer cargo shapes 0.0.0 compute/seed()."),
            Some("seed".to_string())
        );
        assert_eq!(
            symbol_name_from_moniker("rust-analyzer cargo shapes 0.0.0 models/Rectangle#"),
            Some("Rectangle".to_string())
        );
        assert_eq!(
            symbol_name_from_moniker("rust-analyzer cargo shapes 0.0.0 models/"),
            None
        );
    }

    #[test]
    fn ts_and_py_monikers_unchanged_by_rust_shim() {
        // The pre-existing TS/Python (scip-typescript-grammar) cases MUST still
        // extract identically — the Rust shim must not perturb them.
        // TS function / method / type.
        assert_eq!(
            symbol_name_from_moniker("scip-typescript npm pkg 1.0.0 src/`b.ts`/foo()."),
            Some("foo".to_string())
        );
        assert_eq!(
            symbol_name_from_moniker("scip-typescript npm pkg 1.0.0 src/`b.ts`/Widget#save()."),
            Some("save".to_string())
        );
        assert_eq!(
            symbol_name_from_moniker("scip-typescript npm pkg 1.0.0 src/`b.ts`/Widget#"),
            Some("Widget".to_string())
        );
        // A backtick-quoted identifier.
        assert_eq!(
            symbol_name_from_moniker("scip-typescript npm pkg 1.0.0 src/`b.ts`/`café`()."),
            Some("café".to_string())
        );
        // Python method / function / class (scip-python uses the same grammar).
        assert_eq!(
            symbol_name_from_moniker("scip-python python shop 0.0.0 `shop.models`/Cart#total()."),
            Some("total".to_string())
        );
        assert_eq!(
            symbol_name_from_moniker("scip-python python shop 0.0.0 `shop.models`/sum_prices()."),
            Some("sum_prices".to_string())
        );
        assert_eq!(
            symbol_name_from_moniker("scip-python python shop 0.0.0 `shop.models`/Cart#"),
            Some("Cart".to_string())
        );
        // A module path → no trailing name.
        assert_eq!(
            symbol_name_from_moniker("scip-python python shop 0.0.0 `shop.models`/__init__:"),
            None
        );
    }

    // ── Overload-tolerant definition matching (match_named_def) ───────────────

    fn t_uid(file: &str, fqn: &str) -> Uid {
        Uid::new("ts", "t", file, fqn, "")
    }

    // Exact (file, line, name) always wins — the precise, always-correct path.
    #[test]
    fn match_named_def_prefers_exact_line() {
        let parse = t_uid("src/lib.ts", "parse");
        let mut by_loc: HashMap<(String, u32, String), Uid> = HashMap::new();
        by_loc.insert(("src/lib.ts".into(), 20, "parse".into()), parse.clone());
        let mut by_name: HashMap<(String, String), Option<Uid>> = HashMap::new();
        by_name.insert(("src/lib.ts".into(), "parse".into()), Some(parse.clone()));

        assert_eq!(
            match_named_def(&by_loc, &by_name, "src/lib.ts", 20, "parse"),
            Some(parse)
        );
    }

    // Overload: SCIP's def line (the signature, 18) differs from the extractor's
    // (the impl, 20). The exact key misses; the UNIQUE (file, name) fallback
    // recovers the impl node — this is the bug being fixed.
    #[test]
    fn match_named_def_overload_falls_back_to_unique_name() {
        let parse = t_uid("src/lib.ts", "parse");
        let mut by_loc: HashMap<(String, u32, String), Uid> = HashMap::new();
        by_loc.insert(("src/lib.ts".into(), 20, "parse".into()), parse.clone());
        let mut by_name: HashMap<(String, String), Option<Uid>> = HashMap::new();
        by_name.insert(("src/lib.ts".into(), "parse".into()), Some(parse.clone()));

        assert_eq!(
            match_named_def(&by_loc, &by_name, "src/lib.ts", 18, "parse"),
            Some(parse),
            "an overload signature line must resolve to the unique impl node"
        );
    }

    // Over-merge guard: two DISTINCT same-named symbols in one file (e.g.
    // `Rect.area` and `Disc.area`, both simple-name `area`) make the (file, name)
    // key ambiguous. Exact lines still resolve precisely, but a NON-matching line
    // must DECLINE rather than guess between them — never a confident-but-wrong
    // merge.
    #[test]
    fn match_named_def_declines_ambiguous_name() {
        let rect_area = t_uid("src/shapes.ts", "Rect.area");
        let disc_area = t_uid("src/shapes.ts", "Disc.area");
        let mut by_loc: HashMap<(String, u32, String), Uid> = HashMap::new();
        by_loc.insert(
            ("src/shapes.ts".into(), 7, "area".into()),
            rect_area.clone(),
        );
        by_loc.insert(
            ("src/shapes.ts".into(), 16, "area".into()),
            disc_area.clone(),
        );
        let mut by_name: HashMap<(String, String), Option<Uid>> = HashMap::new();
        by_name.insert(("src/shapes.ts".into(), "area".into()), None);

        assert_eq!(
            match_named_def(&by_loc, &by_name, "src/shapes.ts", 7, "area"),
            Some(rect_area)
        );
        assert_eq!(
            match_named_def(&by_loc, &by_name, "src/shapes.ts", 16, "area"),
            Some(disc_area)
        );
        assert_eq!(
            match_named_def(&by_loc, &by_name, "src/shapes.ts", 99, "area"),
            None,
            "an ambiguous name must never be merged on an exact miss"
        );
    }

    // A name absent from the file resolves to nothing (no spurious match).
    #[test]
    fn match_named_def_absent_name_is_none() {
        let by_loc: HashMap<(String, u32, String), Uid> = HashMap::new();
        let by_name: HashMap<(String, String), Option<Uid>> = HashMap::new();
        assert_eq!(
            match_named_def(&by_loc, &by_name, "src/lib.ts", 18, "ghost"),
            None
        );
    }

    // The index BUILD collapses an ambiguous (file, name) to `None` but keeps a
    // unique name resolvable — proven against the REAL extractor output: an
    // overloaded `parse` (one impl symbol, unique name) and `area` shared by two
    // classes (ambiguous).
    #[test]
    fn build_symbol_indices_collapses_ambiguous_keeps_unique() {
        let src = "\
export function parse(x: string): number;
export function parse(x: number): string;
export function parse(x: string | number): number | string { return 0; }
export class Rect { area(): number { return 1; } }
export class Disc { area(): number { return 2; } }
";
        let mut analyzed = BTreeMap::new();
        analyzed.insert(
            "src/lib.ts".to_string(),
            strata_lang_ts::analyze("src/lib.ts", src),
        );
        let idx = build_symbol_indices("t", &analyzed);

        // `parse` is a single unique symbol in the file → fallback-resolvable.
        assert_eq!(
            idx.by_file_name
                .get(&("src/lib.ts".to_string(), "parse".to_string())),
            Some(&Some(t_uid("src/lib.ts", "parse"))),
            "the overloaded parse must be a single unique symbol"
        );
        // `area` is shared by Rect and Disc → ambiguous → never merged on a guess.
        assert_eq!(
            idx.by_file_name
                .get(&("src/lib.ts".to_string(), "area".to_string())),
            Some(&None::<Uid>),
            "a name shared by two classes must collapse to ambiguous"
        );
    }
}
