//! The `auto`/`on`/`off` precise-resolution switch, capability detection, and a
//! source-hash-keyed cache of the produced `index.scip` (spec R1/R5, §5).
//!
//! [`resolve_scip`] decides whether to run `scip-typescript` for a repository
//! and returns a parsed [`ScipResolver`] (or `None` to indicate the heuristic
//! path). The three modes differ only in how a [`ScipError`] is treated:
//!   * [`ResolveMode::Off`]  — never runs SCIP.
//!   * [`ResolveMode::Auto`] — runs SCIP when prerequisites are present; ANY
//!     error degrades to the heuristic (indexing still succeeds, R1).
//!   * [`ResolveMode::On`]   — SCIP is required; any error is propagated (R5).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use strata_scip::{run_scip, RunOptions, ScipError, ScipResolver};

/// The precise-resolution mode (project config / CLI `--resolve auto|on|off`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ResolveMode {
    /// Use SCIP when available; degrade to the heuristic on any failure (R1).
    #[default]
    Auto,
    /// Require SCIP; a failure is a hard error (R5).
    On,
    /// Never run SCIP; pure heuristic.
    Off,
}

impl ResolveMode {
    /// Parse a CLI/config string (`auto`/`on`/`off`, case-insensitive).
    pub fn parse(s: &str) -> Option<ResolveMode> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(ResolveMode::Auto),
            "on" => Some(ResolveMode::On),
            "off" => Some(ResolveMode::Off),
            _ => None,
        }
    }
}

/// Options governing how `index_repo` performs precise resolution.
#[derive(Debug, Clone)]
pub struct IndexOptions {
    /// The resolution mode.
    pub resolve_mode: ResolveMode,
    /// Whether SCIP is allowed to run a (network) `npm install` to provision
    /// `typescript`. Off by default so ordinary indexing never blocks on the
    /// network: `Auto` then runs SCIP only when `typescript` is already
    /// installed. The live integration test opts in.
    pub allow_install: bool,
    /// Whether to index *vendored* third-party dependency bundles instead of
    /// pruning them. Off by default: a committed `pip install -t .` bundle (the
    /// AWS Lambda anti-pattern) is detected via its `*.dist-info` and excluded so
    /// it never inflates the graph; set true (`--include-vendored`) to index it
    /// anyway. The `.strataignore` exclude list is honored regardless.
    pub include_vendored: bool,
}

impl Default for IndexOptions {
    fn default() -> Self {
        IndexOptions {
            resolve_mode: ResolveMode::Auto,
            allow_install: false,
            include_vendored: false,
        }
    }
}

/// The result of attempting precise resolution: the parsed resolver (if any) and
/// whether an `Auto` run degraded to the heuristic.
pub struct ScipOutcome {
    pub resolver: Option<ScipResolver>,
    pub degraded: bool,
}

/// Decide on, run (with caching), and parse SCIP for `repo_path` per `mode`.
///
/// `sources` is the current TS/JS file set (path → text); its blake3 hash keys
/// the on-disk `index.scip` cache so an unchanged source set skips re-running
/// `scip-typescript`.
pub fn resolve_scip(
    repo_path: &Path,
    sources: &BTreeMap<String, String>,
    opts: &IndexOptions,
) -> Result<ScipOutcome, ScipError> {
    match opts.resolve_mode {
        ResolveMode::Off => Ok(ScipOutcome {
            resolver: None,
            degraded: false,
        }),
        ResolveMode::Auto => {
            if sources.is_empty() || !scip_runnable(repo_path, opts.allow_install) {
                // Prerequisites absent: degrade silently (no error in Auto).
                return Ok(ScipOutcome {
                    resolver: None,
                    degraded: true,
                });
            }
            match run_cached(repo_path, sources, opts.allow_install) {
                Ok(resolver) => Ok(ScipOutcome {
                    resolver: Some(resolver),
                    degraded: false,
                }),
                Err(e) => {
                    // R1: structured diagnostic, then degrade — indexing succeeds.
                    eprintln!("strata: precise resolution degraded to heuristic ({e})");
                    Ok(ScipOutcome {
                        resolver: None,
                        degraded: true,
                    })
                }
            }
        }
        ResolveMode::On => {
            if sources.is_empty() {
                return Err(ScipError::ToolUnavailable(
                    "resolve mode `on` requires TS/JS sources, but none were found".to_string(),
                ));
            }
            if !scip_runnable(repo_path, opts.allow_install) {
                // R5: hard-fail with a clear, actionable message (no network probe).
                return Err(ScipError::ToolUnavailable(format!(
                    "resolve mode `on` requires scip-typescript, but `typescript` is not \
                     installed in {} and installs are disabled (pass allow_install or run \
                     `npm install`)",
                    repo_path.display()
                )));
            }
            let resolver = run_cached(repo_path, sources, opts.allow_install)?;
            Ok(ScipOutcome {
                resolver: Some(resolver),
                degraded: false,
            })
        }
    }
}

/// Cheap capability gate: can `scip-typescript` run without a network install?
/// True when installs are permitted, or when `typescript` is already present.
fn scip_runnable(repo_path: &Path, allow_install: bool) -> bool {
    allow_install || repo_path.join("node_modules").join("typescript").is_dir()
}

/// Run `scip-typescript` (or reuse a cached index keyed by the source hash) and
/// parse the result.
fn run_cached(
    repo_path: &Path,
    sources: &BTreeMap<String, String>,
    allow_install: bool,
) -> Result<ScipResolver, ScipError> {
    let hash = sources_hash(sources);
    let cached = cache_path(&hash);

    // Reuse a previously produced index for this exact source set.
    if cached.is_file() {
        if let Ok(resolver) = ScipResolver::from_index_file(&cached) {
            return Ok(resolver);
        }
        // A corrupt cache entry must not blind us: fall through and re-run.
    }

    let run_opts = RunOptions {
        run_npm_install: allow_install,
        ..RunOptions::default()
    };
    let index_path = run_scip(repo_path, &run_opts)?;
    let resolver = ScipResolver::from_index_file(&index_path)?;

    // Best-effort cache write; failures here are non-fatal (we already parsed).
    let _ = store_in_cache(&cached, &index_path);
    Ok(resolver)
}

/// A stable blake3 hash of the (sorted) TS/JS sources — the cache key.
///
/// Keyed on source *content*, not the index bytes: a freshly produced live index
/// carries non-deterministic metadata bytes but is occurrence-identical, so
/// content is the correct invariant (M1 finding).
fn sources_hash(sources: &BTreeMap<String, String>) -> String {
    let mut hasher = blake3::Hasher::new();
    // BTreeMap iterates in sorted key order → deterministic.
    for (path, text) in sources {
        hasher.update(path.as_bytes());
        hasher.update(&[0]);
        hasher.update(text.as_bytes());
        hasher.update(&[0]);
    }
    hasher.finalize().to_hex().to_string()
}

/// Where a cached `index.scip` for a given source hash lives.
fn cache_path(hash: &str) -> PathBuf {
    std::env::temp_dir()
        .join("strata-scip-cache")
        .join(format!("{hash}.scip"))
}

/// Copy a produced index into the cache location (creating the directory).
fn store_in_cache(cached: &Path, produced: &Path) -> std::io::Result<()> {
    if let Some(parent) = cached.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(produced, cached)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_mode_parses_case_insensitively() {
        assert_eq!(ResolveMode::parse("auto"), Some(ResolveMode::Auto));
        assert_eq!(ResolveMode::parse("ON"), Some(ResolveMode::On));
        assert_eq!(ResolveMode::parse(" Off "), Some(ResolveMode::Off));
        assert_eq!(ResolveMode::parse("nonsense"), None);
    }

    #[test]
    fn off_mode_never_runs_scip() {
        let dir = tempfile::tempdir().unwrap();
        let sources =
            BTreeMap::from([("src/a.ts".to_string(), "export function f(){}".to_string())]);
        let opts = IndexOptions {
            resolve_mode: ResolveMode::Off,
            allow_install: false,
            include_vendored: false,
        };
        let out = resolve_scip(dir.path(), &sources, &opts).expect("off never errors");
        assert!(out.resolver.is_none());
        assert!(!out.degraded, "off is not a degradation");
    }

    #[test]
    fn auto_degrades_when_typescript_absent() {
        // A temp dir with TS sources but no node_modules: Auto must degrade
        // (no error, no network) because typescript is not installed.
        let dir = tempfile::tempdir().unwrap();
        let sources =
            BTreeMap::from([("src/a.ts".to_string(), "export function f(){}".to_string())]);
        let opts = IndexOptions {
            resolve_mode: ResolveMode::Auto,
            allow_install: false,
            include_vendored: false,
        };
        let out = resolve_scip(dir.path(), &sources, &opts).expect("auto degrades, never errors");
        assert!(out.resolver.is_none(), "no resolver when degraded");
        assert!(out.degraded, "auto records the degradation");
    }

    #[test]
    fn on_hard_fails_when_scip_unavailable() {
        // On + prerequisites missing (no typescript, installs disabled) → Err,
        // cheaply (no npx/network probe).
        let dir = tempfile::tempdir().unwrap();
        let sources =
            BTreeMap::from([("src/a.ts".to_string(), "export function f(){}".to_string())]);
        let opts = IndexOptions {
            resolve_mode: ResolveMode::On,
            allow_install: false,
            include_vendored: false,
        };
        let msg = match resolve_scip(dir.path(), &sources, &opts) {
            Err(e) => e.to_string(),
            Ok(_) => panic!("on must hard-fail when scip is unavailable"),
        };
        assert!(
            msg.contains("on") && msg.contains("typescript"),
            "error must explain the `on` prerequisite, got: {msg}"
        );
    }

    #[test]
    fn on_with_no_sources_errors() {
        let dir = tempfile::tempdir().unwrap();
        let sources: BTreeMap<String, String> = BTreeMap::new();
        let opts = IndexOptions {
            resolve_mode: ResolveMode::On,
            allow_install: false,
            include_vendored: false,
        };
        let msg = match resolve_scip(dir.path(), &sources, &opts) {
            Err(e) => e.to_string(),
            Ok(_) => panic!("on + no sources must be an error"),
        };
        assert!(msg.contains("sources"));
    }

    #[test]
    fn sources_hash_is_order_independent_and_content_sensitive() {
        let a = BTreeMap::from([
            ("src/a.ts".to_string(), "x".to_string()),
            ("src/b.ts".to_string(), "y".to_string()),
        ]);
        // Same content, constructed differently → same hash.
        let mut b = BTreeMap::new();
        b.insert("src/b.ts".to_string(), "y".to_string());
        b.insert("src/a.ts".to_string(), "x".to_string());
        assert_eq!(sources_hash(&a), sources_hash(&b));

        // Different content → different hash.
        let c = BTreeMap::from([
            ("src/a.ts".to_string(), "x".to_string()),
            ("src/b.ts".to_string(), "Y".to_string()),
        ]);
        assert_ne!(sources_hash(&a), sources_hash(&c));
    }
}
