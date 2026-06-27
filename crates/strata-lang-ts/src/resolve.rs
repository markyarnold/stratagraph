//! Pure TypeScript/JavaScript module resolution over a [`ModuleFs`] trait.
//!
//! No real filesystem access: existence checks go through `ModuleFs::exists`,
//! so resolution is fully unit-testable. The indexer (milestone 4) provides a
//! real `ModuleFs` and builds [`ResolveOptions`] from tsconfig. Path separators
//! are normalized to `/` throughout.
//!
//! This is a *bounded* resolver: it covers the common cases (relative imports
//! with extension/index probing, tsconfig `paths` aliases with a single
//! trailing `*`, and bare-package classification) and returns
//! [`ResolveResult::Unresolved`] for anything outside that scope. It does not
//! read `package.json` `exports` maps, follow symlinks, or descend into
//! `node_modules`.

/// File-existence oracle. The only filesystem capability resolution needs.
pub trait ModuleFs {
    /// Whether a file exists at the given (already `/`-normalized) path.
    fn exists(&self, path: &str) -> bool;
}

/// Resolution configuration derived from tsconfig `compilerOptions`.
#[derive(Debug, Default, Clone)]
pub struct ResolveOptions {
    /// `compilerOptions.baseUrl` (a directory). `paths` targets are relative to it.
    pub base_url: Option<String>,
    /// `compilerOptions.paths`, e.g. `("@app/*", ["src/*"])`. Patterns and
    /// targets are matched/substituted relative to `base_url`.
    pub paths: Vec<(String, Vec<String>)>,
}

/// The outcome of resolving one import specifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveResult {
    /// Resolved to a concrete, `/`-normalized module file path.
    Resolved(String),
    /// A bare package name (`react`, `@scope/pkg`) — left for the caller.
    External(String),
    /// Could not be resolved (caller may mark the edge AMBIGUOUS).
    Unresolved,
}

/// Candidate extensions appended to a bare path, in priority order.
const EXTENSIONS: &[&str] = &[".ts", ".tsx", ".d.ts", ".js", ".jsx", ".mjs", ".cjs"];

/// Resolve `specifier` imported from `importer_path`.
///
/// - Relative (`./`, `../`, `/`): join with the importer's directory, normalize
///   `.`/`..`, then probe exact / `+ ext` / `<path>/index + ext`.
/// - tsconfig `paths`: match a `*`-suffixed alias, substitute into each target
///   (relative to `base_url`), probe as above.
/// - Bare specifier: classify as `External(package)`.
/// - Otherwise: `Unresolved`.
pub fn resolve(
    specifier: &str,
    importer_path: &str,
    opts: &ResolveOptions,
    fs: &dyn ModuleFs,
) -> ResolveResult {
    if is_relative(specifier) {
        let base_dir = parent_dir(&normalize_sep(importer_path));
        let joined = join(&base_dir, &normalize_sep(specifier));
        let candidate = normalize_dots(&joined);
        return match probe(&candidate, fs) {
            Some(path) => ResolveResult::Resolved(path),
            None => ResolveResult::Unresolved,
        };
    }

    // tsconfig path aliases take precedence over bare-package classification.
    if let Some(result) = resolve_alias(specifier, opts, fs) {
        return result;
    }

    // Anything left that looks like a package name is External.
    if let Some(pkg) = package_name(specifier) {
        return ResolveResult::External(pkg);
    }

    ResolveResult::Unresolved
}

/// Try each tsconfig `paths` pattern. A pattern is either exact or has a single
/// trailing `*` capturing the remainder; the captured text is substituted for
/// the `*` in each target. Targets are resolved relative to `base_url`.
fn resolve_alias(
    specifier: &str,
    opts: &ResolveOptions,
    fs: &dyn ModuleFs,
) -> Option<ResolveResult> {
    let base = opts.base_url.as_deref().unwrap_or(".");
    for (pattern, targets) in &opts.paths {
        let Some(captured) = match_pattern(pattern, specifier) else {
            continue;
        };
        for target in targets {
            let substituted = substitute(target, captured.as_deref());
            let candidate =
                normalize_dots(&join(&normalize_sep(base), &normalize_sep(&substituted)));
            if let Some(path) = probe(&candidate, fs) {
                return Some(ResolveResult::Resolved(path));
            }
        }
    }
    None
}

/// Match `specifier` against a `paths` pattern.
/// - Exact pattern (no `*`): matches only an identical specifier; capture None.
/// - Trailing-`*` pattern (`@app/*`): the prefix before `*` must match; the
///   remainder is captured. Returns `None` if the specifier does not match.
///
/// The outer `Option` is "did it match"; the inner `Option<String>` is the
/// captured wildcard text (None for exact patterns).
fn match_pattern(pattern: &str, specifier: &str) -> Option<Option<String>> {
    if let Some(prefix) = pattern.strip_suffix('*') {
        if let Some(rest) = specifier.strip_prefix(prefix) {
            return Some(Some(rest.to_string()));
        }
        return None;
    }
    if pattern == specifier {
        return Some(None);
    }
    None
}

/// Substitute the captured wildcard into a target containing a single `*`.
fn substitute(target: &str, captured: Option<&str>) -> String {
    match captured {
        Some(cap) => target.replacen('*', cap, 1),
        None => target.to_string(),
    }
}

/// Probe a normalized base path for a real file: exact, then `+ ext`, then
/// `<base>/index + ext`. Returns the first existing `/`-normalized path.
fn probe(base: &str, fs: &dyn ModuleFs) -> Option<String> {
    if fs.exists(base) {
        return Some(base.to_string());
    }
    for ext in EXTENSIONS {
        let candidate = format!("{base}{ext}");
        if fs.exists(&candidate) {
            return Some(candidate);
        }
    }
    let index_base = if base.is_empty() {
        "index".to_string()
    } else {
        format!("{base}/index")
    };
    for ext in EXTENSIONS {
        let candidate = format!("{index_base}{ext}");
        if fs.exists(&candidate) {
            return Some(candidate);
        }
    }
    None
}

/// Bare-package classification: first path segment, or the first two segments
/// for scoped packages (`@scope/pkg`). Returns `None` for empty input.
fn package_name(specifier: &str) -> Option<String> {
    if specifier.is_empty() {
        return None;
    }
    let mut parts = specifier.split('/');
    let first = parts.next()?;
    if first.starts_with('@') {
        // Scoped: need a second segment for a valid package name.
        let second = parts.next()?;
        Some(format!("{first}/{second}"))
    } else {
        Some(first.to_string())
    }
}

/// Whether a specifier is a relative/absolute path import (vs a bare package).
fn is_relative(specifier: &str) -> bool {
    specifier.starts_with("./")
        || specifier.starts_with("../")
        || specifier.starts_with('/')
        || specifier == "."
        || specifier == ".."
}

/// Replace `\` with `/`.
fn normalize_sep(path: &str) -> String {
    path.replace('\\', "/")
}

/// The directory portion of a file path (everything up to the last `/`), or
/// `""` if the path has no separator.
fn parent_dir(path: &str) -> String {
    match path.rfind('/') {
        Some(idx) => path[..idx].to_string(),
        None => String::new(),
    }
}

/// Join a base directory and a (possibly relative) path with a single `/`.
/// An absolute `path` (leading `/`) replaces the base.
fn join(base: &str, path: &str) -> String {
    if path.starts_with('/') {
        return path.to_string();
    }
    if base.is_empty() {
        return path.to_string();
    }
    format!("{}/{}", base.trim_end_matches('/'), path)
}

/// Collapse `.` and `..` segments. Leading `..` that cannot be collapsed are
/// preserved (so paths above the root remain distinguishable). A leading `/`
/// is preserved.
fn normalize_dots(path: &str) -> String {
    let absolute = path.starts_with('/');
    let mut out: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => continue,
            ".." => {
                if matches!(out.last(), Some(&last) if last != "..") {
                    out.pop();
                } else if !absolute {
                    out.push("..");
                }
                // For absolute paths, `..` above root is dropped.
            }
            other => out.push(other),
        }
    }
    let joined = out.join("/");
    if absolute {
        format!("/{joined}")
    } else {
        joined
    }
}
