//! `rename`: graph-aware, confidence-tagged multi-file rename (dry-run default).
//!
//! The find-and-replace killer the steering's Never-Do demands. Given a loaded
//! [`Graph`], a repo root, an old name, and a new name, [`rename`]:
//!
//! 1. **Resolves** the old name to exactly one **code-plane** node
//!    (Function/Method/Class/Interface). Several matches → a `Candidates`
//!    outcome (the caller pins one with `uid`). A non-code target (a contract
//!    field / infra resource) → a clear "code symbols only" error.
//! 2. Collects the **graph-implicated files**: the target's definition file plus
//!    every file owning a node connected to the target by a `Calls`/`Imports`
//!    edge (either direction). A same-named identifier in a file the graph does
//!    **not** implicate is *never* touched — the mandatory adversarial guarantee.
//! 3. In each implicated file, re-parses with the file's own tree-sitter grammar
//!    (the same grammar `index_repo` used) and collects every **identifier
//!    token** exactly equal to the old name → a confidence-tagged [`Edit`] per
//!    occurrence. The confidence is the implicating edge's confidence; the
//!    definition site is a fact → `DEF_SITE_CONFIDENCE` (0.95).
//! 4. **Guards**: dry-run is the DEFAULT ([`RenameOptions::apply`] is `false`);
//!    a repo-wide existing symbol already named `new_name` is a collision →
//!    refused with the list unless `force`; `apply` writes each file atomically
//!    (temp file + rename) and recommends a reindex.
//!
//! **Honest over-match bound (documented).** Identifier-token matching is purely
//! lexical *within* an implicated file: a same-named **local** variable in an
//! implicated file would also be collected (and tagged with that file's edge
//! confidence, so the band policy flags a `< 0.9` edit for review). It never
//! reaches a non-implicated file. Precise per-occurrence resolution
//! (Roslyn/pyright/SCIP) tightens this later; today the guarantee is *scope*
//! (only graph-implicated files) plus *confidence tagging*, not per-token
//! semantic precision.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;
use strata_core::{Direction, EdgeKind, Graph, NodeKind, Uid};

use crate::code_language_of;

/// The confidence attached to an edit at the symbol's **definition site**. The
/// def site is a fact (the graph node's own file), so it sits at the top of the
/// Extracted band (0.95) — distinct from a caller-site edit, which inherits the
/// implicating call edge's confidence.
pub const DEF_SITE_CONFIDENCE: f32 = 0.95;

/// Options controlling a [`rename`].
#[derive(Debug, Clone, Default)]
pub struct RenameOptions {
    /// Write the edits to disk. **Default `false`** — a dry run that only
    /// computes and returns the edit set. The CLI requires `--apply` and the MCP
    /// tool an explicit `apply: true` to flip this.
    pub apply: bool,
    /// When the old name resolves to several code nodes, pin the one whose uid
    /// equals this. `None` with multiple matches yields a `Candidates` outcome.
    pub uid: Option<String>,
    /// Proceed even when a repo-wide symbol is already named `new_name` (the
    /// collision guard). Default `false` → a collision refuses with the list.
    pub force: bool,
}

/// One planned edit: an identifier-token occurrence to rewrite, confidence-tagged.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Edit {
    /// Repo-relative file the edit is in.
    pub file: String,
    /// 1-based line of the identifier token.
    pub line: u32,
    /// 0-based column (UTF-8 byte offset within the line) of the token start.
    pub col: u32,
    /// The text being replaced (always the old name).
    pub old: String,
    /// The replacement text (the new name).
    pub new: String,
    /// Confidence of this edit: the implicating edge's confidence, or
    /// [`DEF_SITE_CONFIDENCE`] for an edit in the definition file. The band
    /// policy applies — a caller reviews `< 0.9` edits.
    pub confidence: f32,
}

/// One candidate when the old name resolves to several code nodes.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Candidate {
    pub uid: String,
    pub name: String,
    pub kind: String,
    pub path: String,
}

/// The outcome of a [`rename`] call.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum RenameOutcome {
    /// The old name resolved to several code nodes; the caller must pin one with
    /// [`RenameOptions::uid`]. Carries every candidate.
    Candidates {
        symbol: String,
        candidates: Vec<Candidate>,
    },
    /// A planned (dry-run) or applied rename. `applied` is `true` only when the
    /// edits were written to disk.
    Plan {
        /// The resolved target node's uid.
        target_uid: String,
        old: String,
        new: String,
        /// Whether the edits were written (`apply: true` and no collision).
        applied: bool,
        /// Every implicated file (repo-relative), the scope guarantee made
        /// explicit: edits only ever live in these files.
        implicated_files: Vec<String>,
        /// The confidence-tagged edits, sorted (file, line, col).
        edits: Vec<Edit>,
        /// On `apply`, the human reminder that the served graph / on-disk index
        /// should be refreshed (hooks normally cover it).
        reindex_recommended: bool,
    },
}

/// An error from [`rename`].
#[derive(Debug, thiserror::Error)]
pub enum RenameError {
    /// The old name matched nothing in the graph.
    #[error("symbol not found: {0} (try `strata query {0}` to search)")]
    NotFound(String),
    /// The old name matched a non-code node (a contract field / infra resource).
    #[error("rename supports code symbols (Function/Method/Class/Interface); `{0}` is a {1} — contract/infra rename is queued")]
    NotCodeSymbol(String, String),
    /// A `uid` was supplied to pin a candidate but matched none of them.
    #[error("uid `{0}` did not match any candidate for `{1}`")]
    UidNotFound(String, String),
    /// A repo-wide symbol is already named `new_name` and `force` was not set.
    #[error("rename would collide: {count} existing symbol(s) already named `{new_name}` — re-run with force to proceed anyway:\n{list}")]
    Collision {
        new_name: String,
        count: usize,
        list: String,
    },
    /// Reading or writing an implicated file failed (only on `apply`).
    #[error("io error at {path}: {detail}")]
    Io { path: String, detail: String },
}

/// The edge kinds that imply a textual reference to the target's name in another
/// file: a `Calls` edge (the caller writes the callee's identifier) and an
/// `Imports` edge (the importer writes the imported name). These are the
/// code-plane reference edges; contract/infra edges (`Produces`/`Runs`/…) do not
/// imply an identifier token and are deliberately excluded.
const REFERENCE_EDGE_KINDS: [EdgeKind; 2] = [EdgeKind::Calls, EdgeKind::Imports];

/// The code-plane node kinds `rename` accepts as a target.
fn is_code_target_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Function | NodeKind::Method | NodeKind::Class | NodeKind::Interface
    )
}

/// Run a graph-aware rename of `old_name` → `new_name` over the repo at
/// `repo_root`, honouring `opts` (dry-run by default).
///
/// See the module docs for the full contract. The returned [`RenameOutcome`] is
/// either a `Candidates` list (ambiguous target), or a `Plan` (the edit set,
/// `applied` iff written).
pub fn rename(
    graph: &Graph,
    repo_root: &Path,
    old_name: &str,
    new_name: &str,
    opts: &RenameOptions,
) -> Result<RenameOutcome, RenameError> {
    // ── 1. Resolve the target to exactly one code-plane node. ──
    let target = resolve_code_target(graph, old_name, opts.uid.as_deref())?;
    let target = match target {
        TargetResolution::One(node) => node,
        TargetResolution::Many(candidates) => {
            return Ok(RenameOutcome::Candidates {
                symbol: old_name.to_string(),
                candidates,
            });
        }
    };

    // ── 2. Collision guard: a repo-wide symbol already named `new_name`. ──
    if !opts.force {
        let collisions = collisions_for(graph, new_name);
        if !collisions.is_empty() {
            let list = collisions
                .iter()
                .map(|n| format!("  - {} ({})", n.name, n.path))
                .collect::<Vec<_>>()
                .join("\n");
            return Err(RenameError::Collision {
                new_name: new_name.to_string(),
                count: collisions.len(),
                list,
            });
        }
    }

    // ── 3. Implicated files: def file (def-site confidence) + edge-connected
    // files (the implicating edge's confidence). A file reached by several edges
    // keeps the MAX confidence (most trustworthy implication). ──
    let implicated = implicated_files(graph, &target);

    // ── 4. Collect identifier-token edits in each implicated file. ──
    let mut edits: Vec<Edit> = Vec::new();
    for (file, confidence) in &implicated {
        let content = read_repo_file(repo_root, file)?;
        for (line, col) in identifier_tokens(file, &content, old_name) {
            edits.push(Edit {
                file: file.clone(),
                line,
                col,
                old: old_name.to_string(),
                new: new_name.to_string(),
                confidence: *confidence,
            });
        }
    }
    edits.sort_by(|a, b| (&a.file, a.line, a.col).cmp(&(&b.file, b.line, b.col)));

    // ── 5. Apply (atomic per file) when requested. ──
    let applied = if opts.apply && !edits.is_empty() {
        apply_edits(repo_root, &edits)?;
        true
    } else {
        false
    };

    let implicated_files: Vec<String> = implicated.into_keys().collect();
    Ok(RenameOutcome::Plan {
        target_uid: target.uid.0.clone(),
        old: old_name.to_string(),
        new: new_name.to_string(),
        applied,
        implicated_files,
        edits,
        reindex_recommended: applied,
    })
}

/// A minimal node view the target resolver returns (so it does not borrow the
/// graph past resolution). Only the `uid` (graph anchor for edge walks) and
/// `path` (the definition file) are needed downstream.
#[derive(Debug, Clone)]
struct TargetNode {
    uid: Uid,
    path: String,
}

#[derive(Debug)]
enum TargetResolution {
    One(TargetNode),
    Many(Vec<Candidate>),
}

/// Resolve `old_name` to exactly one code-plane node, applying the `uid` pin.
///
/// Resolution mirrors `resolve_symbol`'s fqn-then-name tiers, but restricted to
/// code-plane kinds. A name that resolves only to non-code nodes is a clear
/// `NotCodeSymbol` error (so renaming a GraphQL field says "contract rename
/// queued", not "not found").
fn resolve_code_target(
    graph: &Graph,
    old_name: &str,
    uid: Option<&str>,
) -> Result<TargetResolution, RenameError> {
    // All nodes matching by fqn first, then by name (the resolve_symbol tiers).
    let mut matches: Vec<&strata_core::Node> =
        graph.nodes().filter(|n| n.fqn == old_name).collect();
    if matches.is_empty() {
        matches = graph.nodes().filter(|n| n.name == old_name).collect();
    }
    if matches.is_empty() {
        return Err(RenameError::NotFound(old_name.to_string()));
    }

    // Partition into code vs non-code.
    let code: Vec<&strata_core::Node> = matches
        .iter()
        .copied()
        .filter(|n| is_code_target_kind(n.kind))
        .collect();
    if code.is_empty() {
        // Everything that matched is non-code → the queued-feature error, naming
        // the first match's kind.
        let kind = kind_name(matches[0].kind);
        return Err(RenameError::NotCodeSymbol(old_name.to_string(), kind));
    }

    // A uid pin selects exactly one of the code candidates.
    if let Some(uid) = uid {
        let picked = code
            .iter()
            .find(|n| n.uid.0 == uid)
            .ok_or_else(|| RenameError::UidNotFound(uid.to_string(), old_name.to_string()))?;
        return Ok(TargetResolution::One(TargetNode {
            uid: picked.uid.clone(),
            path: picked.path.clone(),
        }));
    }

    if code.len() == 1 {
        let n = code[0];
        Ok(TargetResolution::One(TargetNode {
            uid: n.uid.clone(),
            path: n.path.clone(),
        }))
    } else {
        let mut candidates: Vec<Candidate> = code
            .iter()
            .map(|n| Candidate {
                uid: n.uid.0.clone(),
                name: n.name.clone(),
                kind: kind_name(n.kind),
                path: n.path.clone(),
            })
            .collect();
        candidates.sort_by(|a, b| a.uid.cmp(&b.uid));
        Ok(TargetResolution::Many(candidates))
    }
}

/// Every node in the graph whose `name` equals `new_name` (the collision set).
/// Name (not fqn) is the right granularity: a rename that introduces a clashing
/// *name* anywhere is what the guard protects against.
fn collisions_for<'a>(graph: &'a Graph, new_name: &str) -> Vec<&'a strata_core::Node> {
    let mut out: Vec<&strata_core::Node> = graph.nodes().filter(|n| n.name == new_name).collect();
    out.sort_by(|a, b| a.uid.cmp(&b.uid));
    out
}

/// The set of implicated files: the target's definition file (tagged
/// [`DEF_SITE_CONFIDENCE`]) plus every file owning a node connected to the target
/// by a [`REFERENCE_EDGE_KINDS`] edge in either direction (tagged that edge's
/// confidence). A file reached multiple ways keeps the **max** confidence.
/// Deterministic (BTreeMap key order).
fn implicated_files(graph: &Graph, target: &TargetNode) -> BTreeMap<String, f32> {
    let mut files: BTreeMap<String, f32> = BTreeMap::new();

    // The definition file is always implicated, at the def-site confidence.
    bump(&mut files, target.path.clone(), DEF_SITE_CONFIDENCE);

    // Edge-connected files, both directions, only the reference edge kinds.
    for dir in [Direction::Outgoing, Direction::Incoming] {
        for (edge, node) in graph.neighbors(&target.uid, dir, &REFERENCE_EDGE_KINDS) {
            if node.path.is_empty() {
                continue;
            }
            bump(&mut files, node.path.clone(), edge.confidence.value());
        }
    }
    files
}

/// Insert `path → confidence`, keeping the larger confidence on a repeat.
fn bump(files: &mut BTreeMap<String, f32>, path: String, confidence: f32) {
    files
        .entry(path)
        .and_modify(|c| {
            if confidence > *c {
                *c = confidence;
            }
        })
        .or_insert(confidence);
}

/// Read a repo-relative file's text, mapping IO failure to [`RenameError::Io`].
fn read_repo_file(repo_root: &Path, file: &str) -> Result<String, RenameError> {
    std::fs::read_to_string(repo_root.join(file)).map_err(|e| RenameError::Io {
        path: file.to_string(),
        detail: e.to_string(),
    })
}

/// Collect every identifier-token occurrence of `name` in `content`, parsing
/// with the grammar `file`'s extension routes to (the same grammar `index_repo`
/// used). Returns `(line, col)` pairs (1-based line, 0-based byte column).
///
/// An "identifier token" is any **leaf** node whose kind ends in `identifier`
/// (`identifier`, `property_identifier`, `type_identifier`,
/// `shorthand_property_identifier`, …) whose source text equals `name` exactly.
/// This is grammar-driven, so a `name` inside a string literal, a comment, or a
/// number is never matched (those are not identifier nodes) — the precision the
/// "no naive find-and-replace" rule requires, while staying recall-biased within
/// the file. A non-code file (no grammar) yields nothing.
fn identifier_tokens(file: &str, content: &str, name: &str) -> Vec<(u32, u32)> {
    let Some(language) = grammar_for(file) else {
        return Vec::new();
    };
    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&language).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(content, None) else {
        return Vec::new();
    };

    let bytes = content.as_bytes();
    let mut out = Vec::new();
    // Iterative DFS over the tree; collect identifier-family leaves equal to name.
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        let child_count = node.child_count();
        if child_count == 0 {
            // A leaf. Match on the identifier-family kind and exact text.
            if node.kind().ends_with("identifier") {
                if let Some(text) = bytes.get(node.byte_range()) {
                    if text == name.as_bytes() {
                        let pos = node.start_position();
                        out.push((pos.row as u32 + 1, pos.column as u32));
                    }
                }
            }
            continue;
        }
        for i in 0..child_count {
            if let Some(child) = node.child(i) {
                stack.push(child);
            }
        }
    }
    // Deterministic order regardless of DFS traversal order.
    out.sort_unstable();
    out
}

/// The tree-sitter grammar for `file`'s extension, matching the language
/// `code_language_of` reports (the single source of truth). `None` for a
/// non-code file. `.tsx` uses the TSX grammar; `.jsx`/`.js`/`.mjs`/`.cjs` use the
/// JavaScript grammar; other `ts` extensions use TypeScript.
fn grammar_for(file: &str) -> Option<tree_sitter::Language> {
    match code_language_of(file)? {
        "ts" => {
            let ext = file.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
            let lang: tree_sitter::Language = match ext.as_str() {
                "tsx" => tree_sitter_typescript::LANGUAGE_TSX.into(),
                "js" | "jsx" | "mjs" | "cjs" => tree_sitter_javascript::LANGUAGE.into(),
                _ => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            };
            Some(lang)
        }
        "py" => Some(tree_sitter_python::LANGUAGE.into()),
        "cs" => Some(tree_sitter_c_sharp::LANGUAGE.into()),
        "rust" => Some(tree_sitter_rust::LANGUAGE.into()),
        _ => None,
    }
}

/// Apply `edits` to disk, one file at a time, **atomically** (write a temp file
/// then rename over the original). Within a file, edits are applied
/// right-to-left (descending line/col) so earlier offsets stay valid as later
/// ones are rewritten. Every edit in `edits` for a file must share the old/new
/// length-delta handling — here `old`→`new` is a simple span replacement per
/// occurrence.
fn apply_edits(repo_root: &Path, edits: &[Edit]) -> Result<(), RenameError> {
    // Group edits by file.
    let mut by_file: BTreeMap<&str, Vec<&Edit>> = BTreeMap::new();
    for e in edits {
        by_file.entry(&e.file).or_default().push(e);
    }

    for (file, mut file_edits) in by_file {
        let abs = repo_root.join(file);
        let content = read_repo_file(repo_root, file)?;
        // Line-indexed byte offsets so (line, col) → an absolute byte position.
        let line_starts = line_start_offsets(&content);
        // Apply right-to-left so earlier positions are not shifted.
        file_edits.sort_by_key(|e| std::cmp::Reverse((e.line, e.col)));

        let mut bytes = content.into_bytes();
        for e in file_edits {
            let Some(&line_start) = line_starts.get((e.line as usize).saturating_sub(1)) else {
                continue;
            };
            let start = line_start + e.col as usize;
            let end = start + e.old.len();
            if end > bytes.len() || &bytes[start..end] != e.old.as_bytes() {
                // Defensive: the file changed under us / a position is stale.
                // Skip rather than corrupt; the dry-run is the source of truth.
                continue;
            }
            bytes.splice(start..end, e.new.bytes());
        }

        write_atomic(&abs, &bytes).map_err(|err| RenameError::Io {
            path: file.to_string(),
            detail: err.to_string(),
        })?;
    }
    Ok(())
}

/// Byte offset of the start of each line (0-based line index → byte offset).
fn line_start_offsets(content: &str) -> Vec<usize> {
    let mut starts = vec![0usize];
    for (i, b) in content.bytes().enumerate() {
        if b == b'\n' {
            starts.push(i + 1);
        }
    }
    starts
}

/// Write `bytes` to `path` atomically: write a sibling temp file, then rename it
/// over `path` (an atomic replace on the same filesystem).
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "rename".to_string());
    let tmp: PathBuf = dir.join(format!(".{file_name}.strata-rename.tmp"));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

/// The serde unit-variant name of a node kind (e.g. `"Function"`).
fn kind_name(kind: NodeKind) -> String {
    serde_json::to_value(kind)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| format!("{kind:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use strata_core::{Confidence, Edge, Node, Provenance, Span};

    fn node(uid: &str, name: &str, kind: NodeKind, path: &str) -> Node {
        Node {
            uid: Uid(uid.into()),
            kind,
            name: name.into(),
            fqn: name.into(),
            path: path.into(),
            span: Span::default(),
            provenance: Provenance::Extracted,
            confidence: Confidence::new(1.0),
        }
    }

    fn edge(src: &str, dst: &str, kind: EdgeKind, conf: f32) -> Edge {
        Edge {
            src: Uid(src.into()),
            dst: Uid(dst.into()),
            kind,
            provenance: Provenance::Inferred,
            confidence: Confidence::new(conf),
        }
    }

    // ── identifier_tokens: grammar-driven, excludes strings/comments ──

    #[test]
    fn identifier_tokens_ts_skips_strings_and_comments() {
        let src = "export function foo() { return 1; }\n\
                   export function bar() { return foo(); }\n\
                   // foo in a comment\n\
                   const s = \"foo in a string\";\n";
        let hits = identifier_tokens("src/a.ts", src, "foo");
        // The def (line 1) and the call (line 2) — NOT the comment or the string.
        assert_eq!(
            hits,
            vec![(1, 16), (2, 31)],
            "only the identifier def + call sites, not the comment/string; got {hits:?}"
        );
    }

    #[test]
    fn identifier_tokens_py_def_and_call() {
        let src = "def foo():\n    return 1\n\n\ndef bar():\n    return foo()\n# foo comment\n";
        let hits = identifier_tokens("svc/a.py", src, "foo");
        assert_eq!(hits, vec![(1, 4), (6, 11)], "py def + call; got {hits:?}");
    }

    #[test]
    fn identifier_tokens_cs_method_decl_and_call() {
        let src = "namespace App\n{\n    class Svc\n    {\n        public int Foo() { return 1; }\n        public int Bar() { return Foo(); }\n    }\n}\n";
        let hits = identifier_tokens("Svc.cs", src, "Foo");
        assert_eq!(
            hits,
            vec![(5, 19), (6, 34)],
            "cs method decl + call; got {hits:?}"
        );
    }

    #[test]
    fn identifier_tokens_non_code_file_is_empty() {
        assert!(identifier_tokens("README.md", "# foo\nfoo foo\n", "foo").is_empty());
    }

    // ── resolve_code_target ──

    #[test]
    fn resolve_rejects_non_code_target() {
        let mut g = Graph::new();
        g.add_node(node(
            "gql|app|s.graphql|Query.getX|()",
            "Query.getX",
            NodeKind::GraphqlField,
            "s.graphql",
        ));
        let err = resolve_code_target(&g, "Query.getX", None).unwrap_err();
        match err {
            RenameError::NotCodeSymbol(name, kind) => {
                assert_eq!(name, "Query.getX");
                assert_eq!(kind, "GraphqlField");
            }
            other => panic!("expected NotCodeSymbol, got {other:?}"),
        }
    }

    #[test]
    fn resolve_returns_candidates_when_ambiguous() {
        let mut g = Graph::new();
        g.add_node(node("ts|a|a.ts|foo|()", "foo", NodeKind::Function, "a.ts"));
        g.add_node(node("ts|a|b.ts|foo|()", "foo", NodeKind::Function, "b.ts"));
        match resolve_code_target(&g, "foo", None).unwrap() {
            TargetResolution::Many(c) => assert_eq!(c.len(), 2),
            TargetResolution::One(_) => panic!("expected ambiguous candidates"),
        }
    }

    #[test]
    fn resolve_uid_pins_one_candidate() {
        let mut g = Graph::new();
        g.add_node(node("ts|a|a.ts|foo|()", "foo", NodeKind::Function, "a.ts"));
        g.add_node(node("ts|a|b.ts|foo|()", "foo", NodeKind::Function, "b.ts"));
        match resolve_code_target(&g, "foo", Some("ts|a|b.ts|foo|()")).unwrap() {
            TargetResolution::One(n) => assert_eq!(n.path, "b.ts"),
            TargetResolution::Many(_) => panic!("uid must pin one"),
        }
    }

    #[test]
    fn resolve_not_found_is_an_error() {
        let g = Graph::new();
        assert!(matches!(
            resolve_code_target(&g, "nope", None),
            Err(RenameError::NotFound(_))
        ));
    }

    // ── implicated_files: def file + reference-edge files, NOT others ──

    #[test]
    fn implicated_files_are_def_plus_reference_edges_only() {
        let mut g = Graph::new();
        // target `foo` in a.ts; `bar` in b.ts CALLS it; `baz` in c.ts is unrelated;
        // `qux` in d.ts is connected only by a non-reference (Produces) edge.
        g.add_node(node("foo", "foo", NodeKind::Function, "a.ts"));
        g.add_node(node("bar", "bar", NodeKind::Function, "b.ts"));
        g.add_node(node("baz", "baz", NodeKind::Function, "c.ts"));
        g.add_node(node("qux", "qux", NodeKind::Function, "d.ts"));
        g.add_edge(edge("bar", "foo", EdgeKind::Calls, 0.8)); // b.ts calls foo
        g.add_edge(edge("foo", "qux", EdgeKind::Produces, 0.9)); // NOT a reference edge

        let target = TargetNode {
            uid: Uid("foo".into()),
            path: "a.ts".into(),
        };
        let files = implicated_files(&g, &target);
        // a.ts (def, 0.95) and b.ts (caller, 0.8) — never c.ts or d.ts.
        assert_eq!(files.get("a.ts"), Some(&DEF_SITE_CONFIDENCE));
        assert_eq!(files.get("b.ts"), Some(&0.8));
        assert!(!files.contains_key("c.ts"), "unrelated file not implicated");
        assert!(
            !files.contains_key("d.ts"),
            "a Produces-only edge does not implicate a file (no identifier token)"
        );
    }

    // ── collisions_for ──

    #[test]
    fn collisions_lists_existing_same_name_nodes() {
        let mut g = Graph::new();
        g.add_node(node(
            "ts|a|a.ts|taken|()",
            "taken",
            NodeKind::Function,
            "a.ts",
        ));
        let hits = collisions_for(&g, "taken");
        assert_eq!(hits.len(), 1);
        assert!(collisions_for(&g, "free").is_empty());
    }

    // ── line_start_offsets ──

    #[test]
    fn line_start_offsets_indexes_each_line() {
        // "ab\ncd\n" → line 0 at 0, line 1 at 3, (line 2 at 6).
        assert_eq!(line_start_offsets("ab\ncd\n"), vec![0, 3, 6]);
    }
}
