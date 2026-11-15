//! Repository-shape detection for `scip-typescript`: which TypeScript project
//! layout a directory has, and which package manager owns its dependencies.
//!
//! Both answers are read from files on disk only — no subprocess, no network —
//! so [`runner`](crate::runner) can decide how to invoke `scip-typescript`
//! before spending anything.
//!
//! The layout question exists because `scip-typescript index` with no project
//! argument requires a `tsconfig.json` in its working directory. A monorepo
//! keeps its projects in packages and often has no buildable root project at
//! all (a root `tsconfig.base.json` holding `compilerOptions` for packages to
//! `extends` is the common shape), so a root-only invocation exits 1 and the
//! caller degrades to heuristic resolution across every TS file in the repo.
//! `scip-typescript` 0.4.0 can walk the workspaces itself, producing ONE merged
//! index; this module works out which flag to ask it for.

use std::path::Path;

/// Which TypeScript project layout a directory has, and therefore how
/// `scip-typescript` must be invoked for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TsLayout {
    /// A pnpm workspace (`pnpm-workspace.yaml`). Indexed with
    /// `--pnpm-workspaces`.
    PnpmWorkspaces,
    /// An npm or yarn workspace (a `workspaces` field in `package.json`).
    /// Indexed with `--yarn-workspaces`, which reads that same field.
    YarnWorkspaces,
    /// A single project with its own `tsconfig.json`. Needs no flag — this is
    /// the shape the runner has always handled.
    RootProject,
    /// TypeScript sources but no `tsconfig.json` and no workspace manifest.
    /// Indexed with `--infer-tsconfig` so a config-less project still resolves
    /// instead of failing outright.
    NoTsconfig,
}

impl TsLayout {
    /// The `scip-typescript index` flags this layout requires, in the order
    /// they should be passed. Empty for [`TsLayout::RootProject`].
    pub fn scip_flags(self) -> &'static [&'static str] {
        match self {
            TsLayout::PnpmWorkspaces => &["--pnpm-workspaces"],
            TsLayout::YarnWorkspaces => &["--yarn-workspaces"],
            TsLayout::NoTsconfig => &["--infer-tsconfig"],
            TsLayout::RootProject => &[],
        }
    }
}

/// Detect the TypeScript project layout of `dir`.
///
/// A **workspace manifest wins over a root `tsconfig.json`**, deliberately. A
/// monorepo that happens to have both indexes only its root project under
/// today's root-only invocation and silently misses every package — the same
/// bug as an outright failure, with a quieter symptom. Asking for the
/// workspaces covers the root project too, because a workspace walk includes
/// any root project it finds.
///
/// pnpm is checked before the `workspaces` field because a pnpm repo may carry
/// both (some tools read `workspaces` even when pnpm owns the install), and
/// `--pnpm-workspaces` is the correct walk for it.
pub fn detect_layout(dir: &Path) -> TsLayout {
    if dir.join("pnpm-workspace.yaml").is_file() || dir.join("pnpm-workspace.yml").is_file() {
        return TsLayout::PnpmWorkspaces;
    }
    if package_json_declares_workspaces(dir) {
        return TsLayout::YarnWorkspaces;
    }
    if dir.join("tsconfig.json").is_file() {
        return TsLayout::RootProject;
    }
    TsLayout::NoTsconfig
}

/// Whether `<dir>/package.json` carries a non-empty `workspaces` field.
///
/// Both documented shapes count: the array form (`"workspaces": ["packages/*"]`)
/// and the object form (`{"packages": ["packages/*"]}`). An unreadable or
/// unparseable `package.json` answers `false` — detection must never fail the
/// caller, only decline to claim a workspace.
fn package_json_declares_workspaces(dir: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(dir.join("package.json")) else {
        return false;
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
        return false;
    };
    match json.get("workspaces") {
        Some(serde_json::Value::Array(a)) => !a.is_empty(),
        Some(serde_json::Value::Object(o)) => o
            .get("packages")
            .and_then(|p| p.as_array())
            .is_some_and(|a| !a.is_empty()),
        _ => false,
    }
}

/// Which package manager owns a directory's dependencies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageManager {
    Npm,
    Pnpm,
    Yarn,
}

impl PackageManager {
    /// The executable to invoke.
    pub fn program(self) -> &'static str {
        match self {
            PackageManager::Npm => "npm",
            PackageManager::Pnpm => "pnpm",
            PackageManager::Yarn => "yarn",
        }
    }
}

/// Detect the package manager for `dir`, most authoritative signal first.
///
/// 1. `packageManager` in `package.json` — Corepack's own field, which the repo
///    author set deliberately.
/// 2. A lockfile: `pnpm-lock.yaml`, `yarn.lock`, `package-lock.json`.
/// 3. npm, as the default.
///
/// Getting this wrong is not cosmetic: `npm install` in the pnpm repo that
/// prompted this work fails outright
/// (`Cannot read properties of null (reading 'matches')`), and in a repo where
/// it did NOT fail it would write a `package-lock.json` beside a
/// `pnpm-lock.yaml` and install a tree the repo does not use.
pub fn detect_package_manager(dir: &Path) -> PackageManager {
    if let Some(pm) = package_manager_field(dir) {
        return pm;
    }
    if dir.join("pnpm-lock.yaml").is_file() {
        return PackageManager::Pnpm;
    }
    if dir.join("yarn.lock").is_file() {
        return PackageManager::Yarn;
    }
    PackageManager::Npm
}

/// Parse the `packageManager` field (`"pnpm@10.17.1"`) into a
/// [`PackageManager`], or `None` when absent/unreadable/unrecognised.
fn package_manager_field(dir: &Path) -> Option<PackageManager> {
    let text = std::fs::read_to_string(dir.join("package.json")).ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    let spec = json.get("packageManager")?.as_str()?;
    // `name@version`; the version is irrelevant here — we only need the name,
    // and we deliberately do not honour it, because provisioning a specific
    // version is Corepack's job, not ours.
    let name = spec.split('@').next().unwrap_or("").trim();
    match name {
        "pnpm" => Some(PackageManager::Pnpm),
        "yarn" => Some(PackageManager::Yarn),
        "npm" => Some(PackageManager::Npm),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn write(d: &Path, rel: &str, body: &str) {
        let p = d.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(p, body).unwrap();
    }

    // ── layout ────────────────────────────────────────────────────────────

    #[test]
    fn pnpm_workspace_manifest_selects_the_pnpm_walk() {
        let d = dir();
        write(
            d.path(),
            "pnpm-workspace.yaml",
            "packages:\n  - 'packages/*'\n",
        );
        assert_eq!(detect_layout(d.path()), TsLayout::PnpmWorkspaces);
        assert_eq!(detect_layout(d.path()).scip_flags(), &["--pnpm-workspaces"]);
    }

    #[test]
    fn the_yml_spelling_of_the_pnpm_manifest_counts_too() {
        let d = dir();
        write(
            d.path(),
            "pnpm-workspace.yml",
            "packages:\n  - 'packages/*'\n",
        );
        assert_eq!(detect_layout(d.path()), TsLayout::PnpmWorkspaces);
    }

    #[test]
    fn package_json_workspaces_array_selects_the_yarn_walk() {
        let d = dir();
        write(d.path(), "package.json", r#"{"workspaces":["packages/*"]}"#);
        assert_eq!(detect_layout(d.path()), TsLayout::YarnWorkspaces);
        assert_eq!(detect_layout(d.path()).scip_flags(), &["--yarn-workspaces"]);
    }

    #[test]
    fn package_json_workspaces_object_form_counts_too() {
        let d = dir();
        write(
            d.path(),
            "package.json",
            r#"{"workspaces":{"packages":["packages/*"]}}"#,
        );
        assert_eq!(detect_layout(d.path()), TsLayout::YarnWorkspaces);
    }

    #[test]
    fn an_empty_workspaces_field_is_not_a_workspace() {
        let d = dir();
        write(d.path(), "package.json", r#"{"workspaces":[]}"#);
        write(d.path(), "tsconfig.json", "{}");
        assert_eq!(detect_layout(d.path()), TsLayout::RootProject);
    }

    #[test]
    fn a_root_tsconfig_alone_is_the_unchanged_single_project_path() {
        let d = dir();
        write(d.path(), "tsconfig.json", "{}");
        assert_eq!(detect_layout(d.path()), TsLayout::RootProject);
        assert!(
            detect_layout(d.path()).scip_flags().is_empty(),
            "a single-project repo must be invoked exactly as before"
        );
    }

    /// The reporting repo's exact shape: a pnpm workspace whose root carries
    /// `tsconfig.base.json` (compilerOptions only) and NO `tsconfig.json`.
    /// Before this fix scip was invoked at the root and exited 1.
    #[test]
    fn the_reported_shape_resolves_to_the_pnpm_walk() {
        let d = dir();
        write(d.path(), "pnpm-workspace.yaml", "packages:\n  - 'apps/*'\n");
        write(d.path(), "pnpm-lock.yaml", "lockfileVersion: '9.0'\n");
        write(
            d.path(),
            "tsconfig.base.json",
            r#"{"compilerOptions":{"strict":true}}"#,
        );
        write(
            d.path(),
            "package.json",
            r#"{"packageManager":"pnpm@10.17.1"}"#,
        );
        write(d.path(), "apps/web/tsconfig.json", "{}");

        assert_eq!(detect_layout(d.path()), TsLayout::PnpmWorkspaces);
        assert_eq!(detect_package_manager(d.path()), PackageManager::Pnpm);
    }

    /// A workspace manifest wins over a root `tsconfig.json`: indexing only the
    /// root project would silently miss every package.
    #[test]
    fn a_workspace_manifest_wins_over_a_root_tsconfig() {
        let d = dir();
        write(
            d.path(),
            "pnpm-workspace.yaml",
            "packages:\n  - 'packages/*'\n",
        );
        write(d.path(), "tsconfig.json", "{}");
        assert_eq!(detect_layout(d.path()), TsLayout::PnpmWorkspaces);
    }

    #[test]
    fn no_tsconfig_and_no_workspace_asks_scip_to_infer_one() {
        let d = dir();
        write(d.path(), "package.json", r#"{"name":"x"}"#);
        assert_eq!(detect_layout(d.path()), TsLayout::NoTsconfig);
        assert_eq!(detect_layout(d.path()).scip_flags(), &["--infer-tsconfig"]);
    }

    /// A root `tsconfig.base.json` is NOT a buildable project: it carries
    /// `compilerOptions` for packages to extend. Treating it as one is what
    /// produced the original failure.
    #[test]
    fn a_base_tsconfig_is_not_mistaken_for_a_root_project() {
        let d = dir();
        write(d.path(), "tsconfig.base.json", r#"{"compilerOptions":{}}"#);
        assert_eq!(
            detect_layout(d.path()),
            TsLayout::NoTsconfig,
            "tsconfig.base.json must not be read as a root project"
        );
    }

    #[test]
    fn unreadable_or_malformed_package_json_never_claims_a_workspace() {
        let d = dir();
        write(d.path(), "package.json", "{ not json");
        write(d.path(), "tsconfig.json", "{}");
        assert_eq!(detect_layout(d.path()), TsLayout::RootProject);

        let empty = dir();
        assert_eq!(detect_layout(empty.path()), TsLayout::NoTsconfig);
    }

    // ── package manager ───────────────────────────────────────────────────

    #[test]
    fn the_package_manager_field_is_the_most_authoritative_signal() {
        let d = dir();
        // A yarn.lock present, but the field says pnpm: the field wins.
        write(d.path(), "yarn.lock", "");
        write(
            d.path(),
            "package.json",
            r#"{"packageManager":"pnpm@10.17.1"}"#,
        );
        assert_eq!(detect_package_manager(d.path()), PackageManager::Pnpm);
    }

    #[test]
    fn lockfiles_select_the_manager_when_no_field_is_set() {
        for (lock, want) in [
            ("pnpm-lock.yaml", PackageManager::Pnpm),
            ("yarn.lock", PackageManager::Yarn),
            ("package-lock.json", PackageManager::Npm),
        ] {
            let d = dir();
            write(d.path(), lock, "");
            assert_eq!(detect_package_manager(d.path()), want, "for {lock}");
        }
    }

    #[test]
    fn npm_is_the_default_with_no_signal_at_all() {
        let d = dir();
        assert_eq!(detect_package_manager(d.path()), PackageManager::Npm);
        assert_eq!(PackageManager::Npm.program(), "npm");
        assert_eq!(PackageManager::Pnpm.program(), "pnpm");
        assert_eq!(PackageManager::Yarn.program(), "yarn");
    }

    #[test]
    fn an_unrecognised_package_manager_field_falls_through_to_the_lockfile() {
        let d = dir();
        write(d.path(), "pnpm-lock.yaml", "");
        write(
            d.path(),
            "package.json",
            r#"{"packageManager":"bun@1.0.0"}"#,
        );
        assert_eq!(detect_package_manager(d.path()), PackageManager::Pnpm);
    }
}
