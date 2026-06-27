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

/// Run `scip-typescript` in `project_dir`, returning the path to the written
/// `index.scip`.
///
/// Ensures `typescript` is installed first (when `run_npm_install`). All failure
/// modes are [`ScipError`] variants — this NEVER panics, so callers can degrade
/// (spec R1/R3).
pub fn run_scip(project_dir: &Path, opts: &RunOptions) -> Result<PathBuf, ScipError> {
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

    let output = PathBuf::from("index.scip");
    let version_spec = format!(
        "@sourcegraph/scip-typescript@{}",
        opts.scip_typescript_version
    );

    let mut cmd = Command::new("npx");
    cmd.arg("--yes")
        .arg(version_spec)
        .arg("index")
        .arg("--output")
        .arg(&output)
        .current_dir(project_dir)
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

    let index_path = project_dir.join(&output);
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

/// Ensure `typescript` is present in `project_dir` via `npm install`.
///
/// `scip-typescript` needs the project's `typescript` to be installed; this is
/// the verified prerequisite. A missing/failing `npm`, or a non-zero exit, maps
/// to a structured [`ScipError`].
fn ensure_typescript_installed(project_dir: &Path, opts: &RunOptions) -> Result<(), ScipError> {
    // Already installed? Skip the (network) install entirely.
    if project_dir.join("node_modules").join("typescript").is_dir() {
        return Ok(());
    }

    let mut cmd = Command::new("npm");
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
            msg: format!("npm install timed out after {secs}s"),
        },
        other => other,
    })?;

    if !run.status_success {
        return Err(ScipError::InstallFailed {
            dir: project_dir.display().to_string(),
            msg: format!("npm install exited {}: {}", run.code, tail(&run.stderr)),
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

/// Keep only the last ~2 KiB of captured output so error messages stay bounded.
fn tail(s: &str) -> String {
    const MAX: usize = 2048;
    let trimmed = s.trim();
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
