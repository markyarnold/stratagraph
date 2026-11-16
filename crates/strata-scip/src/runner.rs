//! Out-of-process `scip-typescript` runner.
//!
//! [`run_scip`] invokes `scip-typescript` (pinned version) in a project
//! directory to produce an `index.scip`. It is a thin, *bounded* subprocess
//! wrapper: it ensures `typescript` is installed first, runs under a timeout
//! with captured stdout/stderr, and maps **every** failure mode to a
//! [`ScipError`]. It never panics and never hangs (spec R1/R3).

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::error::ScipError;
use crate::layout::{detect_layout, detect_package_manager};

/// The exact `scip-typescript` version this crate is built and tested against.
///
/// Pinned for reproducibility (spec R2): the same project + this version yields
/// the same SCIP index. Verified in this environment with Node 19.
pub const PINNED_SCIP_TYPESCRIPT_VERSION: &str = "0.4.0";

/// Options controlling a `scip-typescript` run.
#[derive(Debug, Clone)]
pub struct RunOptions {
    /// The pinned `scip-typescript` version to invoke (`@<version>` via `npx`).
    pub scip_typescript_version: String,
    /// Wall-clock bound on the indexing subprocess, in seconds.
    pub timeout_secs: u64,
    /// Ensure `typescript` is installed (`npm install`) before indexing.
    pub run_npm_install: bool,
}

impl Default for RunOptions {
    fn default() -> Self {
        RunOptions {
            scip_typescript_version: PINNED_SCIP_TYPESCRIPT_VERSION.to_string(),
            timeout_secs: 600,
            run_npm_install: true,
        }
    }
}

/// How often to poll a child process while waiting for it to exit.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Run `scip-typescript` in `project_dir`, writing the index to `output` and
/// returning the path it wrote.
///
/// `output` is taken explicitly rather than defaulted, because the two callers
/// want different places and the wrong default pollutes a user's repository:
/// indexing passes a path under the system temp dir, while the fixture
/// generator deliberately writes into the project. Previously this always wrote
/// `<project_dir>/index.scip` and never removed it, so every successful index
/// left a multi-megabyte untracked `index.scip` in the repo being indexed —
/// 1.4 MB on the repo that prompted this change, sitting in `git status` ready
/// to be committed by accident.
///
/// A relative `output` is resolved by `scip-typescript` against its own cwd,
/// which is `project_dir`; pass an absolute path to write elsewhere.
///
/// Ensures `typescript` is installed first (when `run_npm_install`). All failure
/// modes are [`ScipError`] variants — this NEVER panics, so callers can degrade
/// (spec R1/R3).
pub fn run_scip(
    project_dir: &Path,
    output: &Path,
    opts: &RunOptions,
) -> Result<PathBuf, ScipError> {
    // Fail fast and structurally if the directory is unusable, rather than
    // letting the subprocess produce a confusing error.
    if !project_dir.is_dir() {
        return Err(ScipError::Io(format!(
            "project_dir is not a directory: {}",
            project_dir.display()
        )));
    }

    if opts.run_npm_install {
        ensure_typescript_installed(project_dir, opts)?;
    }

    // Create the output's parent so scip-typescript is never the thing that
    // fails on a missing directory.
    if let Some(parent) = output.parent() {
        if !parent.as_os_str().is_empty() && !parent.is_dir() {
            std::fs::create_dir_all(parent)
                .map_err(|e| ScipError::Io(format!("creating {}: {e}", parent.display())))?;
        }
    }

    let version_spec = format!(
        "@sourcegraph/scip-typescript@{}",
        opts.scip_typescript_version
    );

    // Which flags this repo's shape needs. A monorepo keeps its projects in
    // packages, so a root-only invocation exits 1 with "missing tsconfig.json"
    // and the caller degrades across every TS file in the repo; asking
    // scip-typescript to walk the workspaces produces one merged index
    // instead. See `layout::detect_layout`.
    let layout = detect_layout(project_dir);

    let mut cmd = Command::new("npx");
    cmd.arg("--yes")
        .arg(version_spec)
        .arg("index")
        .arg("--output")
        .arg(output);
    for flag in layout.scip_flags() {
        cmd.arg(flag);
    }
    cmd.current_dir(project_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let run = run_bounded(cmd, opts.timeout_secs)?;
    if !run.status_success {
        return Err(ScipError::IndexFailed {
            code: run.code,
            stderr: tail(&run.stderr),
        });
    }

    let index_path = resolve_output_path(project_dir, output);
    if !index_path.is_file() {
        return Err(ScipError::IndexFailed {
            code: run.code,
            stderr: format!(
                "scip-typescript reported success but {} was not written; stderr: {}",
                index_path.display(),
                tail(&run.stderr)
            ),
        });
    }
    Ok(index_path)
}

/// Ensure `typescript` is present in `project_dir` via its own package manager.
///
/// `scip-typescript` needs the project's `typescript` to be installed; this is
/// the verified prerequisite. A missing/failing package manager, or a non-zero
/// exit, maps to a structured [`ScipError`].
///
/// The manager is DETECTED rather than assumed
/// ([`detect_package_manager`]): `npm install` in a pnpm repo fails outright
/// (`Cannot read properties of null (reading 'matches')` on the repo that
/// prompted this), and in a repo where it did not fail it would write a
/// `package-lock.json` beside the real lockfile and install a tree the repo
/// does not use. All three managers accept `install`.
fn ensure_typescript_installed(project_dir: &Path, opts: &RunOptions) -> Result<(), ScipError> {
    // Already installed? Skip the (network) install entirely.
    if project_dir.join("node_modules").join("typescript").is_dir() {
        return Ok(());
    }

    let pm = detect_package_manager(project_dir);
    let program = pm.program();

    let mut cmd = Command::new(program);
    cmd.arg("install")
        .current_dir(project_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let run = run_bounded(cmd, opts.timeout_secs).map_err(|e| match e {
        // Re-badge runner-level failures as install failures for clarity.
        ScipError::ToolUnavailable(msg) => ScipError::InstallFailed {
            dir: project_dir.display().to_string(),
            msg,
        },
        ScipError::Timeout { secs } => ScipError::InstallFailed {
            dir: project_dir.display().to_string(),
            msg: format!("{program} install timed out after {secs}s"),
        },
        other => other,
    })?;

    if !run.status_success {
        return Err(ScipError::InstallFailed {
            dir: project_dir.display().to_string(),
            msg: format!(
                "{program} install exited {}: {}",
                run.code,
                tail(&run.stderr)
            ),
        });
    }
    Ok(())
}

/// The captured outcome of a bounded subprocess.
struct BoundedRun {
    status_success: bool,
    code: i32,
    stderr: String,
}

/// Spawn `cmd`, capture stdout/stderr, and wait at most `timeout_secs`.
///
/// On timeout the child is killed and [`ScipError::Timeout`] is returned. A
/// spawn failure (e.g. `npx`/`npm` not on PATH) becomes
/// [`ScipError::ToolUnavailable`]. This is the single bounded-execution path
/// shared by the install and index steps — neither can hang the caller.
fn run_bounded(mut cmd: Command, timeout_secs: u64) -> Result<BoundedRun, ScipError> {
    let mut child = cmd
        .spawn()
        .map_err(|e| ScipError::ToolUnavailable(format!("{}: {e}", program_name(&cmd))))?;

    // Drain the pipes on background threads so a chatty child cannot deadlock by
    // filling its stdout/stderr buffer while we poll for exit.
    let stdout_handle = take_reader(child.stdout.take());
    let stderr_handle = take_reader(child.stderr.take());

    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if Instant::now() >= deadline {
                    kill_child(&mut child);
                    return Err(ScipError::Timeout { secs: timeout_secs });
                }
                std::thread::sleep(POLL_INTERVAL);
            }
            Err(e) => {
                kill_child(&mut child);
                return Err(ScipError::Io(format!("waiting on subprocess: {e}")));
            }
        }
    };

    let _stdout = stdout_handle
        .and_then(|h| h.join().ok())
        .unwrap_or_default();
    let stderr = stderr_handle
        .and_then(|h| h.join().ok())
        .unwrap_or_default();

    Ok(BoundedRun {
        status_success: status.success(),
        code: status.code().unwrap_or(-1),
        stderr,
    })
}

/// Spawn a thread that reads a captured pipe to end, returning its contents.
fn take_reader<R: Read + Send + 'static>(
    reader: Option<R>,
) -> Option<std::thread::JoinHandle<String>> {
    reader.map(|mut r| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = r.read_to_end(&mut buf);
            String::from_utf8_lossy(&buf).into_owned()
        })
    })
}

/// Best-effort terminate-and-reap of a child we are giving up on.
fn kill_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// The program name of a command, for diagnostics.
fn program_name(cmd: &Command) -> String {
    cmd.get_program().to_string_lossy().into_owned()
}

/// Where the index will actually land.
///
/// `scip-typescript` resolves a relative `--output` against its own cwd, which
/// is `project_dir`; an absolute one it honours verbatim. Indexing passes an
/// absolute path under the system temp dir so it never writes into the
/// repository it is reading, while the fixture generator passes a relative one
/// on purpose.
fn resolve_output_path(project_dir: &Path, output: &Path) -> PathBuf {
    if output.is_absolute() {
        output.to_path_buf()
    } else {
        project_dir.join(output)
    }
}

/// Drop the package manager's own advisory chatter from captured stderr.
///
/// A user-level `~/.npmrc` carrying a key the installed npm no longer knows
/// makes npm open EVERY captured stderr with
/// `npm warn Unknown user config "…"`. That is unrelated to the command that
/// just failed, and with [`tail`]'s 2 KiB bound it can push the actual cause
/// out of the window — on the repo that prompted this, the real message
/// (`missing tsconfig.json`) arrived behind exactly such a warning.
///
/// Only advisory prefixes are dropped (`npm warn`, `npm notice`, and their
/// deprecated `npm WARN` spelling). `npm error` lines are the diagnosis and are
/// always kept, as is anything the indexer itself wrote. A message that was
/// ONLY chatter comes back empty rather than misleadingly blank-but-present,
/// which [`run_scip`]'s caller renders as the exit code alone.
fn strip_pm_chatter(s: &str) -> String {
    // Lowercased once per line, so the historical `npm WARN` spelling and
    // today's `npm warn` are the same case to match.
    s.lines()
        .filter(|line| {
            let lower = line.trim_start().to_ascii_lowercase();
            !(lower.starts_with("npm warn") || lower.starts_with("npm notice"))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Keep only the last ~2 KiB of captured output so error messages stay bounded.
///
/// Package-manager chatter is removed FIRST ([`strip_pm_chatter`]), so the
/// bound is spent on the failure rather than on warnings about an unrelated
/// npm config key.
fn tail(s: &str) -> String {
    const MAX: usize = 2048;
    let cleaned = strip_pm_chatter(s);
    let trimmed = cleaned.trim();
    if trimmed.len() <= MAX {
        return trimmed.to_string();
    }
    let start = trimmed.len() - MAX;
    // Align to a char boundary so we never slice a multi-byte char.
    let start = (start..trimmed.len())
        .find(|&i| trimmed.is_char_boundary(i))
        .unwrap_or(trimmed.len());
    format!("...{}", &trimmed[start..])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact shape that buried the real cause on the reporting repo: a
    /// user-level `~/.npmrc` key the installed npm no longer knows, so npm
    /// opens every captured stderr with a warning about it.
    #[test]
    fn package_manager_chatter_is_dropped_so_the_real_cause_survives() {
        let captured = "npm warn Unknown user config \"manage-package-manager-versions\". \
                        This will stop working in the next major version of npm.\n\
                        npm notice New version available\n\
                        - /repo (missing tsconfig.json)";
        let out = tail(captured);
        assert!(
            out.contains("missing tsconfig.json"),
            "the diagnosis must survive: {out}"
        );
        assert!(
            !out.contains("npm warn") && !out.contains("npm notice"),
            "advisory chatter must be gone: {out}"
        );
    }

    /// `npm error` lines ARE the diagnosis when the package manager itself is
    /// what failed, so they are never filtered.
    #[test]
    fn npm_error_lines_are_always_kept() {
        let captured = "npm warn Unknown user config \"x\"\n\
                        npm error Cannot read properties of null (reading 'matches')";
        let out = tail(captured);
        assert!(out.contains("npm error Cannot read properties"), "{out}");
        assert!(!out.contains("npm warn"), "{out}");
    }

    /// The historical uppercase spelling is matched too.
    #[test]
    fn the_legacy_uppercase_warn_spelling_is_dropped() {
        let out = tail("npm WARN deprecated foo@1.0.0\nreal failure here");
        assert_eq!(out, "real failure here");
    }

    /// Output that was ONLY chatter comes back empty rather than carrying a
    /// warning the reader would mistake for the cause.
    #[test]
    fn chatter_only_output_collapses_to_empty() {
        assert_eq!(tail("npm warn a\nnpm notice b"), "");
    }

    /// The 2 KiB bound is spent on the failure, not on warnings: chatter is
    /// removed BEFORE truncation, so a long warning preamble cannot push the
    /// diagnosis out of the window.
    #[test]
    fn the_size_bound_is_applied_after_chatter_is_removed() {
        let mut captured = String::new();
        for i in 0..200 {
            captured.push_str(&format!(
                "npm warn padding line number {i} aaaaaaaaaaaaaaaa\n"
            ));
        }
        captured.push_str("- /repo (missing tsconfig.json)");
        assert!(captured.len() > 2048, "preamble must exceed the bound");
        let out = tail(&captured);
        assert_eq!(out, "- /repo (missing tsconfig.json)");
    }

    /// Non-npm output is untouched, including lines that merely mention npm.
    #[test]
    fn unrelated_output_is_left_alone() {
        let captured = "tsc: error TS2307\nsee npm warnings in the docs";
        assert_eq!(tail(captured), captured);
    }

    /// Indexing must not modify the tree it reads. `run_scip` used to hardcode
    /// `<project_dir>/index.scip` and never remove it, leaving a
    /// multi-megabyte untracked file in the indexed repo (1.4 MB on the repo
    /// that prompted this) — visible in `git status` and committable by
    /// accident. An absolute `output` is now honoured verbatim so the caller
    /// can place it under the system temp dir.
    #[test]
    fn an_absolute_output_path_lands_outside_the_project() {
        let project = Path::new("/repo");
        let out = Path::new("/tmp/strata-scip-run-x/index.scip");
        let resolved = resolve_output_path(project, out);
        assert_eq!(resolved, out);
        assert!(
            !resolved.starts_with(project),
            "an absolute output must not land inside the project: {resolved:?}"
        );
    }

    /// A relative `output` still resolves against the project — the fixture
    /// generator depends on exactly that.
    #[test]
    fn a_relative_output_path_stays_project_relative() {
        let project = Path::new("/repo");
        let resolved = resolve_output_path(project, Path::new("index.scip"));
        assert_eq!(resolved, Path::new("/repo/index.scip"));
    }

    /// A monorepo gets the workspace walk; a single-project repo is invoked
    /// exactly as before this change.
    #[test]
    fn layout_decides_the_flags_the_runner_adds() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("pnpm-workspace.yaml"),
            "packages:\n  - 'packages/*'\n",
        )
        .unwrap();
        assert_eq!(detect_layout(d.path()).scip_flags(), &["--pnpm-workspaces"]);

        let single = tempfile::tempdir().unwrap();
        std::fs::write(single.path().join("tsconfig.json"), "{}").unwrap();
        assert!(detect_layout(single.path()).scip_flags().is_empty());
    }
}
