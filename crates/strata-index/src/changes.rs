//! `detect_changes`: the mechanical pre-commit check.
//!
//! Given a loaded [`Graph`] and a repository root, [`detect_changes`] shells out
//! to the `git` binary for the changed-file set, re-derives the **changed
//! symbols per plane** (code via the existing analyzers, contract via
//! [`strata_contract`], infra via [`strata_infra`]), aggregates the blast radius
//! over the loaded graph with the existing [`impact`] traversal (never
//! reimplemented), and assigns a risk level on the steering's published rubric.
//!
//! No new dependencies: git is invoked through [`std::process::Command`]. A
//! missing `git` binary or a non-repository root is surfaced as a clear
//! [`ChangesError`] — never a silent guess at "no changes".
//!
//! Pure-ish: the only IO is the `git` subprocess (reading the working tree / the
//! index / `HEAD` blobs). The symbol-diff and risk logic are pure functions of
//! the `(old_content, new_content)` pairs and the loaded graph, and are
//! unit-tested without git.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

use serde::Serialize;
use strata_core::{impact, Graph, ImpactOptions, NodeKind};

use crate::{analyze_for_path, code_language_of};

/// Which working set `detect_changes` diffs against `HEAD`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeScope {
    /// The unstaged working tree (`git diff HEAD`). The default.
    Working,
    /// The staged index (`git diff --cached HEAD`).
    Staged,
}

impl ChangeScope {
    fn as_str(self) -> &'static str {
        match self {
            ChangeScope::Working => "working",
            ChangeScope::Staged => "staged",
        }
    }
}

/// An error from running `detect_changes`. Every variant is a clear, actionable
/// message — the tool never guesses "no changes" when it could not actually look.
#[derive(Debug, thiserror::Error)]
pub enum ChangesError {
    /// The `git` binary could not be launched (not installed / not on `PATH`).
    #[error("git is not available: {0} — detect_changes needs the git binary on PATH")]
    GitUnavailable(String),
    /// `git` ran but reported the path is not a repository (or `HEAD` is absent).
    #[error("not a git repository (or no commits yet) at {root}: {stderr}")]
    NotARepo { root: String, stderr: String },
}

/// One changed file's diff status, repo-root-relative and rename-aware.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum FileChange {
    /// A newly added file (no old blob).
    Added { path: String },
    /// A removed file (no new content).
    Deleted { path: String },
    /// A modified file (old blob ↔ new content).
    Modified { path: String },
    /// A rename (`-M`), carrying both the old and the new path.
    Renamed { old_path: String, path: String },
}

impl FileChange {
    /// The current (new) path of the change, or — for a delete — the path that
    /// went away. Used to route the file to a plane by extension.
    pub fn current_path(&self) -> &str {
        match self {
            FileChange::Added { path }
            | FileChange::Deleted { path }
            | FileChange::Modified { path }
            | FileChange::Renamed { path, .. } => path,
        }
    }
}

/// Which plane a changed symbol belongs to. Serialized into the report so the
/// caller can group per plane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Plane {
    Code,
    Contract,
    Infra,
}

/// How a symbol changed between the old and new revision of a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Added,
    Removed,
    Modified,
}

/// The **operation-level** breaking/additive classification of a CONTRACT-plane
/// change: a `Removed`/`Modified` operation key breaks its consumers; an `Added`
/// one cannot (new surface has no existing consumers). Honest bound: this is
/// operation-level only — field-level intelligence (a narrowed type, a new
/// required parameter *inside* an operation) needs request/response schema
/// extraction the contract adapters do not do yet, so an in-place field change
/// surfaces as `Modified` ⇒ `Breaking` (recall-safe, never a silent pass).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContractChange {
    Breaking,
    Additive,
}

impl ContractChange {
    /// Classify a changed symbol: contract-plane `Added` → `Additive`,
    /// contract-plane `Removed`/`Modified` → `Breaking`, other planes → `None`.
    fn classify(plane: Plane, change: ChangeKind) -> Option<ContractChange> {
        match (plane, change) {
            (Plane::Contract, ChangeKind::Added) => Some(ContractChange::Additive),
            (Plane::Contract, _) => Some(ContractChange::Breaking),
            _ => None,
        }
    }
}

/// One changed symbol — a plane-tagged, file-scoped fact, identified by its
/// fully-qualified key (fqn for code; the operation key for contract; the
/// logical id for infra).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChangedSymbol {
    pub plane: Plane,
    pub change: ChangeKind,
    /// The fqn (code), operation key (contract), or logical id (infra).
    pub key: String,
    /// The repo-relative file the symbol lives in (the new path for a rename).
    pub file: String,
    /// Operation-level breaking/additive label — contract plane only, `None`
    /// elsewhere (see [`ContractChange`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contract_change: Option<ContractChange>,
}

impl ChangedSymbol {
    /// Build a changed symbol, deriving the contract-plane breaking/additive
    /// label from `(plane, change)` — the single construction path, so the label
    /// can never drift from the change kind.
    fn new(plane: Plane, change: ChangeKind, key: String, file: String) -> ChangedSymbol {
        ChangedSymbol {
            plane,
            change,
            key,
            file,
            contract_change: ContractChange::classify(plane, change),
        }
    }
}

/// A node reached by aggregating the blast radius of all removed/modified
/// changed symbols over the loaded graph. Mirrors the `impact` tool's affected
/// shape (depth/confidence/ambiguous), deduped across changed symbols keeping
/// the min-depth / max-confidence occurrence.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AffectedNode {
    pub uid: String,
    pub name: String,
    pub kind: String,
    pub path: String,
    pub depth: usize,
    pub confidence: f32,
    pub ambiguous: bool,
    /// The §15.6 will-break verdict, re-derived from the aggregated
    /// `confidence`/`ambiguous` via [`strata_core::will_break_label`] (mirrors the
    /// impact tool's field): `true` = "will break", `false` = "may be affected".
    pub will_break: bool,
}

/// The risk level, on the steering's published rubric (see [`classify_risk`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum RiskLevel {
    Low,
    Medium,
    High,
    Critical,
}

/// The risk verdict: a level plus the human-readable reasons behind it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Risk {
    pub level: RiskLevel,
    /// Why this level — e.g. `"18 affected"`, or
    /// `"breaking change to contract surface: Query.getPolicyStats (removed)"`.
    pub reasons: Vec<String>,
}

/// The full `detect_changes` result.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChangeReport {
    /// `"working"` or `"staged"`.
    pub scope: String,
    /// Every changed file, with its diff status.
    pub files: Vec<FileChange>,
    /// Changed symbols across all planes (sorted: plane, then file, then key).
    pub symbols: Vec<ChangedSymbol>,
    /// Files whose extension belongs to no plane (md, config, …): listed, never
    /// claimed to contain symbols.
    pub other_files: Vec<String>,
    /// The aggregated blast radius over the loaded graph.
    pub affected: Vec<AffectedNode>,
    /// The risk verdict.
    pub risk: Risk,
}

/// One symbol a blast-radius target file defines: the graph node that lives in
/// the file, reduced to the fields a pre-edit reader needs (its name, kind, and
/// the fqn the agent would pass to `impact`/`context`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BlastSymbol {
    /// The node's fully-qualified name — what to pass to `impact`/`context`.
    pub fqn: String,
    /// The node's short name.
    pub name: String,
    /// The serde unit-variant kind name (e.g. `"Function"`, `"GraphqlField"`).
    pub kind: String,
}

/// The blast radius of editing one **file**: the symbols it defines across all
/// planes, the aggregated reverse blast radius of changing them, and the risk —
/// the pre-edit answer to "what depends on this file before I touch it?".
///
/// Built by [`blast_for_file`], which **reuses** `detect_changes`'s
/// [`aggregate_impact`] and [`classify_risk`] (never a parallel implementation),
/// so a file's blast equals the blast `detect_changes` would compute were every
/// symbol in the file modified. A file with **no indexed symbols** is an honest
/// empty report (`symbols` empty, `affected` empty, `Risk::Low` "no indexed
/// symbols"), never a fabricated all-clear.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BlastReport {
    /// The repo-relative file the report is for (as passed in).
    pub file: String,
    /// The symbols the file defines in the loaded graph (sorted by fqn). Empty
    /// for a new/unindexed file — flagged explicitly in [`Self::note`].
    pub symbols: Vec<BlastSymbol>,
    /// The aggregated reverse blast radius of modifying every symbol the file
    /// defines (the same dedupe/order as `detect_changes`).
    pub affected: Vec<AffectedNode>,
    /// The risk verdict on the steering rubric (identical rubric to
    /// `detect_changes`).
    pub risk: Risk,
    /// An honest one-line note when the file has **no indexed symbols** (so the
    /// empty report is never mistaken for "nothing depends on it"); `None` when
    /// the file defines at least one symbol.
    pub note: Option<String>,
}

// ── thresholds (these constants EQUAL the steering's published rubric) ──────────
//
// From the init steering `BLAST_RADIUS_TABLE` (crates/strata-cli/src/init/content.rs):
//   LOW < 5 affected; MEDIUM 5–15; HIGH > 15; CRITICAL on contract surface /
//   cross-repo. The boundaries are inclusive at 5 and 15 (the "5–15" band), so
//   <5 is LOW, 5..=15 is MEDIUM, >15 is HIGH.
/// `affected < LOW_MAX` ⇒ LOW.
pub const LOW_MAX: usize = 5;
/// `affected <= MEDIUM_MAX` (and `>= LOW_MAX`) ⇒ MEDIUM; `> MEDIUM_MAX` ⇒ HIGH.
pub const MEDIUM_MAX: usize = 15;

/// Run the pre-commit change check: git diff → changed symbols per plane →
/// aggregated blast radius over `graph` → risk.
///
/// `repo_root` is the repository working directory; `scope` selects the working
/// tree or the staged index. Returns a [`ChangesError`] (never a false "no
/// changes") when git is unavailable or the path is not a repository.
///
/// Thin wrapper over [`detect_changes_in_repo`] with `repo = None` (no scoping).
/// Call [`detect_changes_in_repo`] directly when the repo name is known (estate mode).
pub fn detect_changes(
    graph: &Graph,
    repo_root: &Path,
    scope: ChangeScope,
) -> Result<ChangeReport, ChangesError> {
    detect_changes_in_repo(graph, repo_root, scope, None)
}

/// Run the pre-commit change check, optionally scoped to a named member repo.
///
/// Identical to [`detect_changes`] when `repo` is `None`. When `repo` is
/// `Some(name)`, only graph nodes whose UID `package` field equals `name` are
/// considered when resolving changed symbols. This prevents path collisions in an
/// estate graph where two repos share a relative file path (e.g. both have
/// `src/handlers.ts`): without scoping, a diff of the producer repo would
/// spuriously match nodes from the consumer repo.
///
/// The `affected` set is still aggregated over the **whole** estate graph after
/// the initial symbol resolution — cross-repo consumers appear there regardless.
pub fn detect_changes_in_repo(
    graph: &Graph,
    repo_root: &Path,
    scope: ChangeScope,
    repo: Option<&str>,
) -> Result<ChangeReport, ChangesError> {
    let files = git_changed_files(repo_root, scope)?;

    // Partition the changed files into the three planes (+ other), deriving the
    // changed symbols for each.
    let mut symbols: Vec<ChangedSymbol> = Vec::new();
    let mut other_files: Vec<String> = Vec::new();

    for fc in &files {
        let path = fc.current_path().to_string();
        let plane = plane_for_path(&path);
        match plane {
            Some(Plane::Code) => {
                let (old, new) = old_new_content(repo_root, fc, scope);
                symbols.extend(diff_code_symbols(&path, old.as_deref(), new.as_deref()));
            }
            Some(Plane::Contract) => {
                // The JSON/YAML space is overloaded: a file may be an OpenAPI
                // spec OR a CFN/SAM template. Run BOTH diff helpers — each
                // self-gates via its adapter (`detects` / `detect_kind`), so at
                // most one claims any given file; a plain `.json` claims neither
                // and falls to `other_files` below. (GraphQL `.graphql/.gql` is
                // contract-only; the infra helper rejects it, costing nothing.)
                let (old, new) = old_new_content(repo_root, fc, scope);
                let mut claimed = diff_contract_symbols(&path, old.as_deref(), new.as_deref());
                claimed.extend(diff_infra_symbols(&path, old.as_deref(), new.as_deref()));
                if claimed.is_empty() {
                    other_files.push(path);
                } else {
                    symbols.extend(claimed);
                }
            }
            Some(Plane::Infra) => {
                let (old, new) = old_new_content(repo_root, fc, scope);
                symbols.extend(diff_infra_symbols(&path, old.as_deref(), new.as_deref()));
            }
            // A changed file in a plane-bearing extension might still be a
            // non-spec (e.g. a `.json` that is neither OpenAPI nor CFN): the diff
            // helpers return nothing, and we additionally record it as `other`
            // only when NO plane claimed it. Handled below.
            None => other_files.push(path),
        }
    }
    // Sort symbols deterministically: plane, then file, then key.
    symbols.sort_by(|a, b| (a.plane as u8, &a.file, &a.key).cmp(&(b.plane as u8, &b.file, &b.key)));
    other_files.sort();
    other_files.dedup();

    // ── Blast radius: aggregate impact over every removed/modified changed
    // symbol that exists in the loaded graph. Added symbols have no upstream.
    // Pass `repo` so the symbol resolution is scoped to the member repo (estate
    // mode); the impact traversal then fans out across the full estate graph. ──
    let affected = aggregate_impact_in_repo(graph, &symbols, repo);

    // ── Risk. ──
    let risk = classify_risk(graph, &symbols, &affected, repo);

    Ok(ChangeReport {
        scope: scope.as_str().to_string(),
        files,
        symbols,
        other_files,
        affected,
        risk,
    })
}

/// Which plane a path's extension belongs to (the spec routing): code for
/// `.ts/.tsx/.js/.jsx/.mjs/.cjs/.py/.pyi/.cs/.csx`; contract/infra for the
/// shared `.graphql/.gql/.json/.yaml/.yml/.template` candidate set (the diff
/// helper then confirms via the adapter's `detects`). Anything else → `None`
/// (other_files).
fn plane_for_path(path: &str) -> Option<Plane> {
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    if code_language_of(path).is_some() {
        return Some(Plane::Code);
    }
    match ext.as_str() {
        // GraphQL SDL / documents are contract candidates.
        "graphql" | "gql" => Some(Plane::Contract),
        // The overloaded JSON/YAML/template space is BOTH a contract (OpenAPI)
        // and an infra (CFN/SAM) candidate. It routes to the `Contract` arm,
        // which runs BOTH diff helpers (each self-gates via its adapter) and
        // demotes the file to `other_files` if neither claims it. The `Plane`
        // here is just "which dispatch arm", not the final classification.
        "json" | "yaml" | "yml" | "template" => Some(Plane::Contract),
        _ => None,
    }
}

// ── git plumbing ────────────────────────────────────────────────────────────────

/// Run `git diff --name-status -M [--cached] HEAD` at `repo_root` and parse the
/// output into [`FileChange`]s. Errors clearly when git is missing or the path
/// is not a repo.
fn git_changed_files(
    repo_root: &Path,
    scope: ChangeScope,
) -> Result<Vec<FileChange>, ChangesError> {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(repo_root)
        .arg("diff")
        .arg("--name-status")
        .arg("-M");
    if scope == ChangeScope::Staged {
        cmd.arg("--cached");
    }
    cmd.arg("HEAD");

    let output = cmd
        .output()
        .map_err(|e| ChangesError::GitUnavailable(e.to_string()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(ChangesError::NotARepo {
            root: repo_root.display().to_string(),
            stderr,
        });
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_name_status(&stdout))
}

/// Parse `git diff --name-status -M` output into [`FileChange`]s.
///
/// Each line is tab-separated: a status code then path(s). `A`/`D`/`M` carry one
/// path; `R<score>` (rename) and `C<score>` (copy) carry old + new paths.
/// Unknown statuses are treated as modifications of their last path (recall-
/// biased: better to analyze than to drop).
fn parse_name_status(stdout: &str) -> Vec<FileChange> {
    let mut out = Vec::new();
    for line in stdout.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split('\t');
        let Some(status) = parts.next() else {
            continue;
        };
        let rest: Vec<&str> = parts.collect();
        if rest.is_empty() {
            continue;
        }
        let code = status.chars().next().unwrap_or('M');
        match code {
            'A' => out.push(FileChange::Added {
                path: rest[0].to_string(),
            }),
            'D' => out.push(FileChange::Deleted {
                path: rest[0].to_string(),
            }),
            'R' | 'C' if rest.len() >= 2 => out.push(FileChange::Renamed {
                old_path: rest[0].to_string(),
                path: rest[1].to_string(),
            }),
            // 'M', 'T' (type change), or any unknown single-path status.
            _ => out.push(FileChange::Modified {
                path: rest.last().copied().unwrap_or_default().to_string(),
            }),
        }
    }
    out
}

/// The old (HEAD blob) and new (working/staged) content for a change.
///
/// - Old content is `git show HEAD:<old_path>` (the pre-rename path for a
///   rename), `None` for an added file (no old blob).
/// - New content is the working-tree file for `Working`, or the staged blob
///   (`git show :<path>`) for `Staged`; `None` for a deleted file.
fn old_new_content(
    repo_root: &Path,
    fc: &FileChange,
    scope: ChangeScope,
) -> (Option<String>, Option<String>) {
    let old = match fc {
        FileChange::Added { .. } => None,
        FileChange::Deleted { path } | FileChange::Modified { path } => {
            git_show(repo_root, &format!("HEAD:{path}"))
        }
        FileChange::Renamed { old_path, .. } => git_show(repo_root, &format!("HEAD:{old_path}")),
    };
    let new = match fc {
        FileChange::Deleted { .. } => None,
        FileChange::Added { path }
        | FileChange::Modified { path }
        | FileChange::Renamed { path, .. } => match scope {
            ChangeScope::Working => std::fs::read_to_string(repo_root.join(path)).ok(),
            ChangeScope::Staged => git_show(repo_root, &format!(":{path}")),
        },
    };
    (old, new)
}

/// `git show <rev>` at `repo_root`, returning the blob text or `None` (absent /
/// binary / error). A `None` is benign — the caller treats a missing side as an
/// empty symbol set.
fn git_show(repo_root: &Path, rev: &str) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("show")
        .arg(rev)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

// ── code-plane symbol diff ──────────────────────────────────────────────────────

/// Diff the code symbols of one file between old and new content.
///
/// Both sides are analyzed with the file's own language analyzer (the same
/// routing `index_repo` uses), then the symbol sets are compared by fqn:
/// - a fqn only in `new` → [`ChangeKind::Added`];
/// - a fqn only in `old` → [`ChangeKind::Removed`];
/// - a fqn in both whose **body slice text** differs → [`ChangeKind::Modified`]
///   (a moved-but-identical body is NOT a modification — we compare the source
///   the span covers, not the span numbers).
fn diff_code_symbols(path: &str, old: Option<&str>, new: Option<&str>) -> Vec<ChangedSymbol> {
    let old_syms = old.map(|c| code_symbol_bodies(path, c)).unwrap_or_default();
    let new_syms = new.map(|c| code_symbol_bodies(path, c)).unwrap_or_default();
    diff_symbol_maps(Plane::Code, path, &old_syms, &new_syms)
}

/// `fqn → body-slice text` for every symbol the analyzer finds in `content`.
///
/// The body slice is the source the symbol's span covers; comparing it (not the
/// raw span) means a symbol that only moved down the file is unchanged, while a
/// symbol whose body text changed is `Modified`.
fn code_symbol_bodies(path: &str, content: &str) -> BTreeMap<String, String> {
    let analyzed = analyze_for_path(path, content);
    let lines: Vec<&str> = content.lines().collect();
    let mut map = BTreeMap::new();
    for sym in &analyzed.symbols {
        let body = slice_span(&lines, sym.span.start_line, sym.span.end_line);
        // On an fqn collision within a file (overloads), keep the first; the diff
        // is still sound (both sides collapse identically).
        map.entry(sym.fqn.clone()).or_insert(body);
    }
    map
}

/// The joined source lines `[start_line, end_line]` (1-based, inclusive). A span
/// out of range clamps to the available lines.
fn slice_span(lines: &[&str], start_line: u32, end_line: u32) -> String {
    let start = (start_line.saturating_sub(1)) as usize;
    let end = (end_line as usize).min(lines.len());
    if start >= end {
        return String::new();
    }
    lines[start..end].join("\n")
}

// ── contract-plane symbol diff ──────────────────────────────────────────────────

/// Diff the contract operations of one file between old and new content.
///
/// Both sides are extracted with the matching contract adapter (GraphQL SDL or
/// OpenAPI); the operation sets are compared by `key`. A `modified` operation is
/// one whose serialized [`OperationDef`] differs (method/path/etc.). A file that
/// neither adapter `detects` yields nothing.
fn diff_contract_symbols(path: &str, old: Option<&str>, new: Option<&str>) -> Vec<ChangedSymbol> {
    let old_ops = old.map(|c| contract_op_sigs(path, c)).unwrap_or_default();
    let new_ops = new.map(|c| contract_op_sigs(path, c)).unwrap_or_default();
    diff_symbol_maps(Plane::Contract, path, &old_ops, &new_ops)
}

/// `op key → signature string` for every operation in a contract spec file. The
/// signature is `"METHOD path"` so a method/path change reads as `Modified`. A
/// non-spec file (no adapter detects it) yields an empty map.
fn contract_op_sigs(path: &str, content: &str) -> BTreeMap<String, String> {
    use strata_contract::{ContractAdapter, GraphqlAdapter, OpenApiAdapter, ProtoAdapter};

    let mut map = BTreeMap::new();
    let openapi = OpenApiAdapter;
    let graphql = GraphqlAdapter;
    let grpc = ProtoAdapter;
    let ops = if openapi.detects(path, content) {
        openapi.extract(path, content).ok()
    } else if graphql.detects(path, content) {
        graphql.extract(path, content).ok()
    } else if grpc.detects(path, content) {
        grpc.extract(path, content).ok()
    } else {
        None
    };
    if let Some(ops) = ops {
        for op in ops {
            map.entry(op.key.clone())
                .or_insert_with(|| format!("{} {}", op.method, op.norm_path));
        }
    }
    map
}

// ── infra-plane symbol diff ─────────────────────────────────────────────────────

/// Diff the infra resources of one template between old and new content.
///
/// Both sides are extracted with [`CfnSamAdapter`](strata_infra::CfnSamAdapter);
/// resources are compared by logical id. A `modified` resource is one whose
/// serialized [`InfraResource`](strata_infra::InfraResource) differs. A file
/// that is not a CFN/SAM template yields nothing.
fn diff_infra_symbols(path: &str, old: Option<&str>, new: Option<&str>) -> Vec<ChangedSymbol> {
    let old_res = old.map(infra_resource_sigs).unwrap_or_default();
    let new_res = new.map(infra_resource_sigs).unwrap_or_default();
    diff_symbol_maps(Plane::Infra, path, &old_res, &new_res)
}

/// `logical id → signature` for every resource in a CFN/SAM template. The
/// signature is the resource's FULL raw parsed sub-tree (via
/// [`strata_infra::raw_resource_signatures`]), NOT the typed `InfraResource` —
/// so a change to ANY property (`Timeout`, `MemorySize`, `Environment`,
/// `QueueName`, tags) reads as `Modified`, not just the graph-wired fields. A
/// non-template yields an empty map.
fn infra_resource_sigs(content: &str) -> BTreeMap<String, String> {
    strata_infra::raw_resource_signatures(content).unwrap_or_default()
}

// ── shared symbol-map diff ──────────────────────────────────────────────────────

/// Diff two `key → signature` maps into [`ChangedSymbol`]s on `plane`/`file`:
/// keys only in `new` are Added, only in `old` are Removed, and keys in both
/// with a differing signature are Modified. Deterministic (BTreeMap order).
fn diff_symbol_maps(
    plane: Plane,
    file: &str,
    old: &BTreeMap<String, String>,
    new: &BTreeMap<String, String>,
) -> Vec<ChangedSymbol> {
    let mut out = Vec::new();
    for (key, new_sig) in new {
        match old.get(key) {
            None => out.push(ChangedSymbol::new(
                plane,
                ChangeKind::Added,
                key.clone(),
                file.to_string(),
            )),
            Some(old_sig) if old_sig != new_sig => out.push(ChangedSymbol::new(
                plane,
                ChangeKind::Modified,
                key.clone(),
                file.to_string(),
            )),
            Some(_) => {} // unchanged
        }
    }
    for key in old.keys() {
        if !new.contains_key(key) {
            out.push(ChangedSymbol::new(
                plane,
                ChangeKind::Removed,
                key.clone(),
                file.to_string(),
            ));
        }
    }
    out
}

// ── blast radius ────────────────────────────────────────────────────────────────

/// Aggregate the reverse blast radius of every removed/modified changed symbol
/// that resolves in the loaded graph (added symbols have no upstream), deduping
/// across symbols by uid keeping the min-depth / max-confidence occurrence.
///
/// `impact` is CALLED (contracts + infra on, defaults) — never reimplemented.
/// A changed symbol resolves to a graph node by matching its `key` against node
/// `fqn` first, then `name`, restricted to nodes in the changed `file` (so a
/// same-named symbol in another file is not impacted on this change's behalf).
///
/// Thin wrapper over [`aggregate_impact_in_repo`] with `repo = None`.
fn aggregate_impact(graph: &Graph, symbols: &[ChangedSymbol]) -> Vec<AffectedNode> {
    aggregate_impact_in_repo(graph, symbols, None)
}

/// Like [`aggregate_impact`] but restricts initial symbol-resolution to nodes
/// whose UID `package` field equals `repo`. When `repo` is `None`, all nodes are
/// considered (identical to [`aggregate_impact`]). The impact traversal fans out
/// over the full graph regardless, so cross-repo consumers still appear in
/// `affected`.
fn aggregate_impact_in_repo(
    graph: &Graph,
    symbols: &[ChangedSymbol],
    repo: Option<&str>,
) -> Vec<AffectedNode> {
    let opts = ImpactOptions::default();
    // uid → (depth, confidence, ambiguous), best (min depth / max conf) kept.
    let mut best: BTreeMap<String, (usize, f32, bool)> = BTreeMap::new();

    for sym in symbols {
        if sym.change == ChangeKind::Added {
            continue; // nothing upstream of a brand-new symbol
        }
        for node_uid in resolve_changed_symbol_in_repo(graph, sym, repo) {
            let result = impact(graph, &node_uid, &opts);
            for a in result.affected {
                let entry =
                    best.entry(a.uid.0.clone())
                        .or_insert((a.depth, a.confidence, a.ambiguous));
                entry.0 = entry.0.min(a.depth);
                entry.1 = entry.1.max(a.confidence);
                entry.2 = entry.2 || a.ambiguous;
            }
        }
    }

    let mut affected: Vec<AffectedNode> = best
        .into_iter()
        .filter_map(|(uid, (depth, confidence, ambiguous))| {
            let node = graph.get_node(&strata_core::Uid(uid.clone()))?;
            Some(AffectedNode {
                uid,
                name: node.name.clone(),
                kind: kind_name(node.kind),
                path: node.path.clone(),
                depth,
                confidence,
                ambiguous,
                will_break: strata_core::will_break_label(confidence, ambiguous),
            })
        })
        .collect();
    // Deterministic: confidence desc, then uid asc (the impact tool's order).
    affected.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.uid.cmp(&b.uid))
    });
    affected
}

/// Like [`resolve_changed_symbol_in_repo`] with `repo = None` (all nodes):
/// `package` field equals `repo` when `repo` is `Some`. Prevents path collisions
/// in an estate graph where two repos share a relative file path.
fn resolve_changed_symbol_in_repo(
    graph: &Graph,
    sym: &ChangedSymbol,
    repo: Option<&str>,
) -> Vec<strata_core::Uid> {
    let repo_filter = |n: &&strata_core::Node| match repo {
        Some(want) => uid_package(n.uid.as_str()).as_deref() == Some(want),
        None => true,
    };
    let by_fqn: Vec<strata_core::Uid> = graph
        .nodes()
        .filter(|n| n.fqn == sym.key && path_matches(&n.path, &sym.file))
        .filter(repo_filter)
        .map(|n| n.uid.clone())
        .collect();
    if !by_fqn.is_empty() {
        return by_fqn;
    }
    graph
        .nodes()
        .filter(|n| n.name == sym.key && path_matches(&n.path, &sym.file))
        .filter(repo_filter)
        .map(|n| n.uid.clone())
        .collect()
}

/// Whether a graph node `path` refers to the changed `file`. The graph stores
/// repo-relative paths; for a contract/infra symbol the changed `file` IS that
/// path. Exact match, or suffix match (a contract node's path may carry the spec
/// path while the changed file is the same repo-relative string).
fn path_matches(node_path: &str, file: &str) -> bool {
    node_path == file || node_path.ends_with(file) || file.ends_with(node_path)
}

// ── risk ────────────────────────────────────────────────────────────────────────

/// Classify the risk on the steering's published rubric:
/// LOW `< 5` affected; MEDIUM `5–15`; HIGH `> 15`; **CRITICAL** when any changed
/// or affected node is contract surface (`ApiOperation`/`GraphqlField`) or the
/// change crosses a repo boundary (an affected node in a different package than
/// the changed nodes).
///
/// The reasons spell out the verdict: the count band always, plus the specific
/// contract surface / cross-repo trigger for CRITICAL.
///
/// `repo` is the optional estate member-repo scope for the `changed_packages`
/// lookup (prevents a path-collision from producing a wrong package set).
fn classify_risk(
    graph: &Graph,
    symbols: &[ChangedSymbol],
    affected: &[AffectedNode],
    repo: Option<&str>,
) -> Risk {
    let mut reasons: Vec<String> = Vec::new();
    let count = affected.len();

    // ── CRITICAL trigger 1: a BREAKING change to contract surface (a removed or
    // modified operation key breaks its consumers). An ADDITIVE contract change —
    // new surface, no existing consumers — is named honestly in the reasons but
    // does not, by itself, escalate: crying CRITICAL for every added endpoint is
    // the cry-wolf failure the rubric exists to avoid. (Trigger 2 still escalates
    // whenever the blast radius REACHES existing contract surface.) ──
    let mut critical = false;
    for sym in symbols {
        match sym.contract_change {
            Some(ContractChange::Breaking) => {
                critical = true;
                let how = match sym.change {
                    ChangeKind::Removed => "removed",
                    _ => "modified",
                };
                reasons.push(format!(
                    "breaking change to contract surface: {} ({how})",
                    sym.key
                ));
            }
            Some(ContractChange::Additive) => {
                reasons.push(format!(
                    "additive contract change: {} (new surface; existing consumers unaffected)",
                    sym.key
                ));
            }
            None => {}
        }
    }
    // ── CRITICAL trigger 2: any AFFECTED node is contract surface. ──
    for a in affected {
        if a.kind == kind_name(NodeKind::ApiOperation)
            || a.kind == kind_name(NodeKind::GraphqlField)
        {
            critical = true;
            reasons.push(format!("affects contract surface: {}", a.name));
        }
    }
    // ── CRITICAL trigger 3: cross-repo reach (an affected node lives in a
    // different package than the changed nodes). The package is the 2nd Uid
    // field (`language|package|path|…`). ──
    let changed_pkgs = changed_packages(graph, symbols, repo);
    if !changed_pkgs.is_empty() {
        for a in affected {
            if let Some(pkg) = uid_package(&a.uid) {
                if !changed_pkgs.contains(&pkg) {
                    critical = true;
                    reasons.push(format!("crosses repo boundary into {pkg}: {}", a.name));
                    break;
                }
            }
        }
    }

    let level = if critical {
        RiskLevel::Critical
    } else if count > MEDIUM_MAX {
        RiskLevel::High
    } else if count >= LOW_MAX {
        RiskLevel::Medium
    } else {
        RiskLevel::Low
    };

    // Always include the affected-count band reason (after any critical reasons).
    reasons.push(match level {
        RiskLevel::Critical => format!("{count} affected (critical override)"),
        _ => format!("{count} affected"),
    });

    Risk { level, reasons }
}

/// The set of packages (the Uid's 2nd field) the changed symbols' graph nodes
/// belong to. Used to detect cross-repo reach in [`classify_risk`].
/// When `repo` is `Some`, only nodes in that repo package are considered (estate
/// mode; prevents a path-collision from inserting a wrong package).
fn changed_packages(
    graph: &Graph,
    symbols: &[ChangedSymbol],
    repo: Option<&str>,
) -> BTreeSet<String> {
    let mut pkgs = BTreeSet::new();
    for sym in symbols {
        for uid in resolve_changed_symbol_in_repo(graph, sym, repo) {
            if let Some(pkg) = uid_package(uid.as_str()) {
                pkgs.insert(pkg);
            }
        }
    }
    pkgs
}

/// The `package` component of a `language|package|path|fqn|signature` uid, if the
/// uid has that canonical shape.
fn uid_package(uid: &str) -> Option<String> {
    let mut parts = uid.split('|');
    let _lang = parts.next()?;
    let pkg = parts.next()?;
    if pkg.is_empty() {
        None
    } else {
        Some(pkg.to_string())
    }
}

/// The serde unit-variant name of a node kind (e.g. `"GraphqlField"`), matching
/// the MCP tool payloads' `kind` strings.
fn kind_name(kind: NodeKind) -> String {
    serde_json::to_value(kind)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| format!("{kind:?}"))
}

// ── pre-edit blast radius (Slice 20) ──────────────────────────────────────────────

/// Which plane a graph node's KIND belongs to, for the blast-radius risk rubric.
///
/// `detect_changes` derives the plane from a *changed file's extension*; the
/// pre-edit blast walks the *graph nodes* a file already defines, so it derives
/// the plane from each node's [`NodeKind`] instead. Contract surface
/// (`ApiOperation`/`GraphqlField`) → [`Plane::Contract`] so the synthetic
/// changed symbol fires `classify_risk`'s CRITICAL "breaking change to contract surface"
/// trigger exactly as a real contract-file change would; the infra kinds →
/// [`Plane::Infra`]; everything else → [`Plane::Code`]. (The plane only affects
/// risk classification — the impact aggregation is plane-agnostic.)
fn plane_for_kind(kind: NodeKind) -> Plane {
    match kind {
        NodeKind::ApiOperation | NodeKind::GraphqlField => Plane::Contract,
        NodeKind::LambdaFn
        | NodeKind::IamRole
        | NodeKind::AppSyncApi
        | NodeKind::AppSyncResolver
        | NodeKind::AppSyncDataSource
        | NodeKind::CloudResource => Plane::Infra,
        _ => Plane::Code,
    }
}

/// Whether a node KIND is a real *symbol* an edit could change — not a structural
/// container the indexer also stores. A blast report lists the file's symbols (the
/// things you'd run `impact` on), so the `Repo`/`Package`/`File` containers are
/// excluded; the per-file `Module` and every code/contract/infra symbol are kept.
///
/// This also guards the headline bug a precise blast must avoid: a `Repo` node's
/// `path` is empty, and an empty `path` suffix-matches *every* file under the
/// loose [`path_matches`] — so without this filter every file's blast would list
/// the repo. The kind filter (plus the non-empty-path check in [`node_in_file`])
/// keeps the symbol set honest.
fn is_blast_symbol_kind(kind: NodeKind) -> bool {
    !matches!(kind, NodeKind::Repo | NodeKind::Package | NodeKind::File)
}

/// Whether a graph node lives in the blast target `file`, **strictly** — the
/// precise match the pre-edit blast needs (unlike `detect_changes`, whose `file`
/// is a git path that always equals the stored node path).
///
/// Requires a NON-EMPTY node path (an empty path is a structural container, never
/// a file member — see [`is_blast_symbol_kind`]) and an exact match or a
/// **path-component-boundary** suffix either way (so `src/a.ts` matches an
/// absolute `/repo/src/a.ts`, but `a.ts` does NOT match `schema.ts` and `""`
/// matches nothing). Stricter than `path_matches`, which a bare suffix can fool.
fn node_in_file(node_path: &str, file: &str) -> bool {
    if node_path.is_empty() {
        return false;
    }
    if node_path == file {
        return true;
    }
    // Component-boundary suffix: `longer` ends with `/shorter` (or equals it).
    let boundary_suffix = |longer: &str, shorter: &str| -> bool {
        longer.len() > shorter.len()
            && longer.ends_with(shorter)
            && longer.as_bytes()[longer.len() - shorter.len() - 1] == b'/'
    };
    boundary_suffix(file, node_path) || boundary_suffix(node_path, file)
}

/// The pre-edit blast radius of editing **one file**, optionally scoped to a
/// single repo by name.
///
/// In an estate, two repos can share a relative path; when `repo` is
/// `Some(name)` only nodes whose UID `package` (the 2nd `|`-delimited field)
/// equals `name` are included. `repo = None` is identical to the unscoped
/// [`blast_for_file`] — it sees symbols from all repos.
///
/// File nodes are lowered to [`ChangedSymbol`]s (`Modified`, keyed by fqn),
/// then handed to [`aggregate_impact`] and [`classify_risk`] verbatim — the
/// same aggregation path `detect_changes` uses.
///
/// Honesty: a file with **no indexed symbols** (new, unindexed, or non-code)
/// returns an explicit empty report — `symbols` empty, `affected` empty, a LOW
/// risk whose reason is "no indexed symbols", and a `note` saying so — never a
/// fabricated all-clear.
pub fn blast_for_file_in_repo(graph: &Graph, file: &str, repo: Option<&str>) -> BlastReport {
    // The graph nodes this file defines (the symbols an edit could change):
    // restricted to real symbol kinds (not the Repo/Package/File containers) that
    // live strictly in this file (non-empty, component-boundary path match) — so
    // an empty-path repo node can never leak into a file's blast.
    // When `repo` is Some, further restrict to nodes whose UID package matches.
    let mut file_nodes: Vec<&strata_core::Node> = graph
        .nodes()
        .filter(|n| is_blast_symbol_kind(n.kind) && node_in_file(&n.path, file))
        .filter(|n| match repo {
            Some(want) => uid_package(n.uid.as_str()).as_deref() == Some(want),
            None => true,
        })
        .collect();
    // Deterministic: by uid.
    file_nodes.sort_by(|a, b| a.uid.cmp(&b.uid));

    // Lower each to a *modified* changed symbol (keyed by fqn, in this file), so
    // the existing aggregation/risk run unchanged.
    let symbols: Vec<ChangedSymbol> = file_nodes
        .iter()
        .map(|n| {
            ChangedSymbol::new(
                plane_for_kind(n.kind),
                ChangeKind::Modified,
                n.fqn.clone(),
                n.path.clone(),
            )
        })
        .collect();

    // The pre-edit symbol list (sorted by fqn for a stable display).
    let mut blast_symbols: Vec<BlastSymbol> = file_nodes
        .iter()
        .map(|n| BlastSymbol {
            fqn: n.fqn.clone(),
            name: n.name.clone(),
            kind: kind_name(n.kind),
        })
        .collect();
    blast_symbols.sort_by(|a, b| a.fqn.cmp(&b.fqn));

    // No indexed symbols → an explicit empty report (never a fake all-clear).
    if symbols.is_empty() {
        return BlastReport {
            file: file.to_string(),
            symbols: blast_symbols,
            affected: Vec::new(),
            risk: Risk {
                level: RiskLevel::Low,
                reasons: vec!["no indexed symbols (new/unindexed file)".to_string()],
            },
            note: Some(
                "no indexed symbols for this file — it is new, unindexed, or not a code/contract/infra file; this is not a guarantee that nothing depends on it".to_string(),
            ),
        };
    }

    // REUSE the detect_changes aggregation + risk verbatim (parity by construction).
    let affected = aggregate_impact(graph, &symbols);
    let risk = classify_risk(graph, &symbols, &affected, None);

    BlastReport {
        file: file.to_string(),
        symbols: blast_symbols,
        affected,
        risk,
        note: None,
    }
}

/// Back-compat wrapper: the pre-edit blast radius of one file across all repos.
///
/// Equivalent to `blast_for_file_in_repo(graph, file, None)`. Prefer
/// [`blast_for_file_in_repo`] when the caller knows the repo name.
pub fn blast_for_file(graph: &Graph, file: &str) -> BlastReport {
    blast_for_file_in_repo(graph, file, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_name_status ──

    #[test]
    fn parse_name_status_handles_a_m_d_and_rename() {
        let out =
            "A\tsrc/new.ts\nM\tsrc/mod.ts\nD\tsrc/gone.ts\nR096\tsrc/old.ts\tsrc/renamed.ts\n";
        let changes = parse_name_status(out);
        assert_eq!(
            changes,
            vec![
                FileChange::Added {
                    path: "src/new.ts".into()
                },
                FileChange::Modified {
                    path: "src/mod.ts".into()
                },
                FileChange::Deleted {
                    path: "src/gone.ts".into()
                },
                FileChange::Renamed {
                    old_path: "src/old.ts".into(),
                    path: "src/renamed.ts".into()
                },
            ]
        );
    }

    // ── plane routing ──

    #[test]
    fn plane_for_path_routes_extensions() {
        assert_eq!(plane_for_path("src/a.ts"), Some(Plane::Code));
        assert_eq!(plane_for_path("svc/handler.py"), Some(Plane::Code));
        assert_eq!(plane_for_path("Svc/Handler.cs"), Some(Plane::Code));
        assert_eq!(plane_for_path("schema.graphql"), Some(Plane::Contract));
        assert_eq!(plane_for_path("openapi.yaml"), Some(Plane::Contract));
        assert_eq!(plane_for_path("README.md"), None);
        assert_eq!(plane_for_path("package.json"), Some(Plane::Contract)); // candidate; helper rejects
    }

    // ── code symbol diff (pure, no git) ──

    #[test]
    fn diff_code_detects_added_removed_modified() {
        let old = "export function keep() { return 1; }\nexport function gone() { return 2; }\n";
        // keep's BODY changed; gone removed; fresh added.
        let new = "export function keep() { return 99; }\nexport function fresh() { return 3; }\n";
        let mut syms = diff_code_symbols("src/a.ts", Some(old), Some(new));
        syms.sort_by(|a, b| a.key.cmp(&b.key));

        let find = |k: &str| syms.iter().find(|s| s.key == k).cloned();
        assert_eq!(find("fresh").unwrap().change, ChangeKind::Added);
        assert_eq!(find("gone").unwrap().change, ChangeKind::Removed);
        assert_eq!(find("keep").unwrap().change, ChangeKind::Modified);
        for s in &syms {
            assert_eq!(s.plane, Plane::Code);
            assert_eq!(s.file, "src/a.ts");
        }
    }

    #[test]
    fn diff_code_body_move_is_not_a_modification() {
        // `target`'s body is identical; only a blank line was added ABOVE it, so
        // its span shifts but the body slice text is the same → NOT modified.
        let old = "export function target() {\n  return 1;\n}\n";
        let new = "\nexport function target() {\n  return 1;\n}\n";
        let syms = diff_code_symbols("src/a.ts", Some(old), Some(new));
        assert!(
            syms.is_empty(),
            "a moved-but-identical body must not be a modification, got {syms:?}"
        );
    }

    #[test]
    fn diff_code_added_file_has_no_old() {
        let new = "export function brandNew() {}\n";
        let syms = diff_code_symbols("src/a.ts", None, Some(new));
        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0].change, ChangeKind::Added);
        assert_eq!(syms[0].key, "brandNew");
    }

    // ── contract symbol diff (pure) ──

    #[test]
    fn diff_contract_detects_removed_graphql_field() {
        let old = "type Query {\n  getUser: User\n  getOrder: Order\n}\n";
        let new = "type Query {\n  getUser: User\n}\n";
        let syms = diff_contract_symbols("schema.graphql", Some(old), Some(new));
        // getOrder removed.
        let removed: Vec<&str> = syms
            .iter()
            .filter(|s| s.change == ChangeKind::Removed)
            .map(|s| s.key.as_str())
            .collect();
        assert!(
            removed.iter().any(|k| k.contains("getOrder")),
            "getOrder field must be detected removed; got {syms:?}"
        );
        assert!(syms.iter().all(|s| s.plane == Plane::Contract));
    }

    // ── infra symbol diff (pure) ──

    #[test]
    fn diff_infra_detects_added_resource() {
        let old = r#"{
  "Resources": {
    "Fn1": { "Type": "AWS::Serverless::Function", "Properties": { "Handler": "a.h" } }
  }
}"#;
        let new = r#"{
  "Resources": {
    "Fn1": { "Type": "AWS::Serverless::Function", "Properties": { "Handler": "a.h" } },
    "Fn2": { "Type": "AWS::Serverless::Function", "Properties": { "Handler": "b.h" } }
  }
}"#;
        let syms = diff_infra_symbols("template.json", Some(old), Some(new));
        let added: Vec<&str> = syms
            .iter()
            .filter(|s| s.change == ChangeKind::Added)
            .map(|s| s.key.as_str())
            .collect();
        assert_eq!(added, vec!["Fn2"], "Fn2 added; got {syms:?}");
        assert!(syms.iter().all(|s| s.plane == Plane::Infra));
    }

    #[test]
    fn diff_infra_detects_untyped_property_change() {
        // Regression (reviewer, slice 12): a change to a property the typed
        // `InfraResource` does NOT capture — here `Timeout` — must still read as
        // Modified. The signature is the raw sub-tree, not the lossy struct.
        let old = r#"{
  "Resources": {
    "Fn1": { "Type": "AWS::Serverless::Function",
             "Properties": { "Handler": "a.h", "Timeout": 10 } }
  }
}"#;
        let new = r#"{
  "Resources": {
    "Fn1": { "Type": "AWS::Serverless::Function",
             "Properties": { "Handler": "a.h", "Timeout": 30 } }
  }
}"#;
        let syms = diff_infra_symbols("template.json", Some(old), Some(new));
        assert_eq!(syms.len(), 1, "exactly Fn1 modified; got {syms:?}");
        assert_eq!(syms[0].key, "Fn1");
        assert_eq!(syms[0].change, ChangeKind::Modified);
        assert_eq!(syms[0].plane, Plane::Infra);
    }

    // ── risk classification (pure) ──

    fn affected_n(n: usize) -> Vec<AffectedNode> {
        (0..n)
            .map(|i| AffectedNode {
                uid: format!("ts|app|f{i}.ts|x|()"),
                name: format!("x{i}"),
                kind: "Function".into(),
                path: format!("f{i}.ts"),
                depth: 1,
                confidence: 0.9,
                ambiguous: false,
                will_break: true,
            })
            .collect()
    }

    #[test]
    fn risk_bands_match_steering_rubric() {
        let g = Graph::new();
        // <5 → LOW
        assert_eq!(
            classify_risk(&g, &[], &affected_n(4), None).level,
            RiskLevel::Low
        );
        // exactly 5 → MEDIUM (5–15 band, inclusive lower bound)
        assert_eq!(
            classify_risk(&g, &[], &affected_n(5), None).level,
            RiskLevel::Medium
        );
        // 15 → MEDIUM (inclusive upper bound)
        assert_eq!(
            classify_risk(&g, &[], &affected_n(15), None).level,
            RiskLevel::Medium
        );
        // 16 → HIGH (>15)
        assert_eq!(
            classify_risk(&g, &[], &affected_n(16), None).level,
            RiskLevel::High
        );
    }

    #[test]
    fn risk_critical_when_changed_symbol_is_contract_surface() {
        let g = Graph::new();
        let symbols = vec![ChangedSymbol::new(
            Plane::Contract,
            ChangeKind::Removed,
            "Query.getPolicyStats".into(),
            "schema.graphql".into(),
        )];
        let risk = classify_risk(&g, &symbols, &[], None);
        assert_eq!(risk.level, RiskLevel::Critical);
        assert!(
            risk.reasons
                .iter()
                .any(|r| r.contains("contract surface") && r.contains("getPolicyStats")),
            "the reason must name the contract surface; got {:?}",
            risk.reasons
        );
    }

    #[test]
    fn risk_critical_when_affected_is_contract_surface() {
        let g = Graph::new();
        let affected = vec![AffectedNode {
            uid: "gql|app|schema.graphql|Query.getX|()".into(),
            name: "Query.getX".into(),
            kind: "GraphqlField".into(),
            path: "schema.graphql".into(),
            depth: 1,
            confidence: 0.95,
            ambiguous: false,
            will_break: true,
        }];
        let risk = classify_risk(&g, &[], &affected, None);
        assert_eq!(risk.level, RiskLevel::Critical);
    }

    // ── will-break label on the aggregated blast radius (§15.6) ──
    //
    // The detect_changes blast radius carries the same will-break label as the
    // impact tool, re-derived from the AGGREGATED (confidence, ambiguous) so a
    // node that reaches a change cleanly at/above the 0.40 floor is "will break"
    // and an ambiguous reach is "may be affected, review".
    #[test]
    fn aggregate_impact_stamps_will_break_from_aggregated_values() {
        use strata_core::{Confidence, Edge, EdgeKind, Node, Provenance, Span, Uid};
        let mk_node = |uid: &str, fqn: &str, path: &str| Node {
            uid: Uid(uid.into()),
            kind: NodeKind::Function,
            name: uid.into(),
            fqn: fqn.into(),
            path: path.into(),
            span: Span::default(),
            provenance: Provenance::Extracted,
            confidence: Confidence::new(1.0),
        };
        let mk_edge = |src: &str, dst: &str, prov: Provenance| Edge {
            src: Uid(src.into()),
            dst: Uid(dst.into()),
            kind: EdgeKind::Calls,
            provenance: prov,
            confidence: Confidence::new(0.9),
        };
        let mut g = Graph::new();
        g.add_node(mk_node("changed", "changed", "a.ts"));
        g.add_node(mk_node("hi", "hi", "b.ts"));
        g.add_node(mk_node("amb", "amb", "c.ts"));
        // hi reaches `changed` cleanly at 0.9; amb reaches it at 0.9 but ambiguous.
        g.add_edge(mk_edge("hi", "changed", Provenance::Inferred));
        g.add_edge(mk_edge("amb", "changed", Provenance::Ambiguous));

        let symbols = vec![ChangedSymbol::new(
            Plane::Code,
            ChangeKind::Modified,
            "changed".into(),
            "a.ts".into(),
        )];

        let affected = aggregate_impact(&g, &symbols);
        let get = |id: &str| affected.iter().find(|a| a.uid == id).unwrap();
        assert!(
            get("hi").will_break,
            "a clean 0.9 dependent is labelled will-break"
        );
        assert!(
            !get("amb").will_break,
            "an ambiguous dependent is may-affect (re-derived post-aggregation)"
        );
    }

    // ── blast_for_file (Slice 20: the pre-edit blast radius of a whole file) ──
    //
    // The shared graph builders for the blast tests: a Node with an explicit kind,
    // and an Inferred edge at conf 0.9.
    fn blast_node(uid: &str, name: &str, path: &str, kind: NodeKind) -> strata_core::Node {
        use strata_core::{Confidence, Provenance, Span, Uid};
        strata_core::Node {
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
    fn blast_edge(src: &str, dst: &str, kind: strata_core::EdgeKind) -> strata_core::Edge {
        use strata_core::{Confidence, Provenance, Uid};
        strata_core::Edge {
            src: Uid(src.into()),
            dst: Uid(dst.into()),
            kind,
            provenance: Provenance::Inferred,
            confidence: Confidence::new(0.9),
        }
    }

    #[test]
    fn blast_for_file_reports_dependents_of_a_files_symbols() {
        use strata_core::{EdgeKind, NodeKind};
        // a.ts defines `target`; b.ts's `caller` depends on it (caller CALLS target).
        // blast("a.ts") must report `caller` as affected and list `target` as a
        // defined symbol.
        let mut g = Graph::new();
        g.add_node(blast_node("a|target", "target", "a.ts", NodeKind::Function));
        g.add_node(blast_node("b|caller", "caller", "b.ts", NodeKind::Function));
        g.add_edge(blast_edge("b|caller", "a|target", EdgeKind::Calls));

        let report = blast_for_file(&g, "a.ts");
        assert_eq!(report.file, "a.ts");
        // The file's defined symbol is listed.
        assert!(
            report.symbols.iter().any(|s| s.fqn == "target"),
            "blast must list the file's symbol target; got {:?}",
            report.symbols
        );
        // The dependent is in the blast radius.
        assert!(
            report.affected.iter().any(|a| a.name == "caller"),
            "blast(a.ts) must surface caller as affected; got {:?}",
            report.affected
        );
        assert!(report.note.is_none(), "an indexed file has no empty-note");
    }

    #[test]
    fn blast_for_file_excludes_empty_path_repo_nodes() {
        use strata_core::NodeKind;
        // THE precision bug: a Repo node has an empty `path`, which suffix-matches
        // EVERY file under the loose path matcher. The blast must NOT list it — only
        // the file's real symbols. (Regression for the over-listing the e2e caught.)
        let mut g = Graph::new();
        g.add_node(blast_node("ts|repo||repo", "myrepo", "", NodeKind::Repo));
        g.add_node(blast_node(
            "ts|repo|src/a.ts|<module>|",
            "a.ts",
            "src/a.ts",
            NodeKind::Module,
        ));
        g.add_node(blast_node(
            "ts|repo|src/a.ts|target|",
            "target",
            "src/a.ts",
            NodeKind::Function,
        ));

        let report = blast_for_file(&g, "src/a.ts");
        let kinds: Vec<&str> = report.symbols.iter().map(|s| s.kind.as_str()).collect();
        assert!(
            !kinds.contains(&"Repo"),
            "the empty-path Repo node must NOT appear in a file's blast; got {:?}",
            report.symbols
        );
        // The real symbol is still listed (Module is kept; Function is kept).
        assert!(
            report.symbols.iter().any(|s| s.fqn == "target"),
            "the file's real symbol must still be listed; got {:?}",
            report.symbols
        );
    }

    #[test]
    fn blast_for_file_path_match_is_component_boundary_not_bare_suffix() {
        use strata_core::NodeKind;
        // `a.ts` must NOT match `schema_a.ts` (a bare suffix would wrongly match).
        let mut g = Graph::new();
        g.add_node(blast_node(
            "ts|r|src/schema_a.ts|x|",
            "x",
            "src/schema_a.ts",
            NodeKind::Function,
        ));
        let report = blast_for_file(&g, "a.ts");
        assert!(
            report.symbols.is_empty(),
            "a.ts must not match schema_a.ts (component-boundary); got {:?}",
            report.symbols
        );
        // But the absolute form of the same file DOES match its repo-relative node.
        let report2 = blast_for_file(&g, "/repo/src/schema_a.ts");
        assert!(
            report2.symbols.iter().any(|s| s.fqn == "x"),
            "an absolute path must match its repo-relative node at a boundary; got {:?}",
            report2.symbols
        );
    }

    #[test]
    fn blast_for_file_with_no_symbols_is_an_honest_empty_report() {
        use strata_core::NodeKind;
        // The graph has a node in OTHER files only; blast on an unindexed file must
        // be an explicit empty report (LOW, with the honest reason + note) — never a
        // fabricated all-clear.
        let mut g = Graph::new();
        g.add_node(blast_node("x|f", "f", "other.ts", NodeKind::Function));

        let report = blast_for_file(&g, "brand/new.ts");
        assert!(
            report.symbols.is_empty(),
            "no symbols for an unindexed file"
        );
        assert!(report.affected.is_empty(), "and nothing aggregated");
        assert_eq!(report.risk.level, RiskLevel::Low);
        assert!(
            report
                .risk
                .reasons
                .iter()
                .any(|r| r.contains("no indexed symbols")),
            "the risk reason must say there are no indexed symbols; got {:?}",
            report.risk.reasons
        );
        let note = report.note.expect("empty report must carry an honest note");
        assert!(
            note.contains("not a guarantee"),
            "the note must not read as a fake all-clear; got {note:?}"
        );
    }

    #[test]
    fn blast_for_file_on_contract_surface_is_critical() {
        use strata_core::NodeKind;
        // A schema file defining a GraphqlField: editing it touches contract surface,
        // so the blast risk is CRITICAL (classify_risk trigger 1), with the field
        // named in the reasons — identical rubric to detect_changes.
        let mut g = Graph::new();
        g.add_node(blast_node(
            "gql|field",
            "Query.getPolicyStats",
            "schema.graphql",
            NodeKind::GraphqlField,
        ));

        let report = blast_for_file(&g, "schema.graphql");
        assert_eq!(
            report.risk.level,
            RiskLevel::Critical,
            "editing contract surface is CRITICAL; got {:?}",
            report.risk
        );
        assert!(
            report
                .risk
                .reasons
                .iter()
                .any(|r| r.contains("contract surface") && r.contains("getPolicyStats")),
            "the reason must name the contract surface; got {:?}",
            report.risk.reasons
        );
    }

    #[test]
    fn blast_scopes_to_repo_when_paths_collide() {
        use strata_core::{NodeKind, Uid};
        // Two repos, same relative path "src/x.ts", different package.
        let mut g = Graph::new();
        g.add_node(blast_node(
            Uid::new("ts", "repo-a", "src/x.ts", "fa", "").as_str(),
            "fa",
            "src/x.ts",
            NodeKind::Function,
        ));
        g.add_node(blast_node(
            Uid::new("ts", "repo-b", "src/x.ts", "fb", "").as_str(),
            "fb",
            "src/x.ts",
            NodeKind::Function,
        ));

        let all = blast_for_file_in_repo(&g, "src/x.ts", None);
        assert_eq!(all.symbols.len(), 2, "unscoped sees both repos' symbols");

        let scoped = blast_for_file_in_repo(&g, "src/x.ts", Some("repo-a"));
        assert_eq!(scoped.symbols.len(), 1);
        assert_eq!(scoped.symbols[0].name, "fa");
    }

    #[test]
    fn blast_for_file_matches_detect_changes_aggregation_for_the_same_symbols() {
        use strata_core::{EdgeKind, NodeKind};
        // PARITY: blast_for_file(f) must equal — affected set + risk — what the
        // detect_changes aggregation/risk produce when every symbol in f is modified.
        // a.ts defines two functions; b.ts and c.ts depend on them at different
        // depths/confidence, so the dedupe (min-depth/max-conf) is exercised.
        let mut g = Graph::new();
        g.add_node(blast_node("a|one", "one", "a.ts", NodeKind::Function));
        g.add_node(blast_node("a|two", "two", "a.ts", NodeKind::Function));
        g.add_node(blast_node("b|mid", "mid", "b.ts", NodeKind::Function));
        g.add_node(blast_node("c|far", "far", "c.ts", NodeKind::Function));
        // mid depends on one directly; far depends on one via mid AND on two directly.
        g.add_edge(blast_edge("b|mid", "a|one", EdgeKind::Calls));
        g.add_edge(blast_edge("c|far", "b|mid", EdgeKind::Calls));
        g.add_edge(blast_edge("c|far", "a|two", EdgeKind::Calls));

        // The reference: the SAME synthetic modified-symbol set the engine builds,
        // run through the detect_changes helpers directly.
        let ref_symbols = vec![
            ChangedSymbol::new(
                Plane::Code,
                ChangeKind::Modified,
                "one".into(),
                "a.ts".into(),
            ),
            ChangedSymbol::new(
                Plane::Code,
                ChangeKind::Modified,
                "two".into(),
                "a.ts".into(),
            ),
        ];
        let ref_affected = aggregate_impact(&g, &ref_symbols);
        let ref_risk = classify_risk(&g, &ref_symbols, &ref_affected, None);

        let report = blast_for_file(&g, "a.ts");
        assert_eq!(
            report.affected, ref_affected,
            "blast affected set must equal the detect_changes aggregation for the file's symbols"
        );
        assert_eq!(
            report.risk, ref_risk,
            "blast risk must equal the detect_changes risk for the same symbols"
        );
        // Sanity: both dependents are reached.
        let names: Vec<&str> = report.affected.iter().map(|a| a.name.as_str()).collect();
        assert!(
            names.contains(&"mid") && names.contains(&"far"),
            "got {names:?}"
        );
    }
}
