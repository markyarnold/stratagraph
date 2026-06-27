//! Structured error type for the SCIP adapter.
//!
//! Every failure mode of running `scip-typescript` and parsing its index maps
//! to a [`ScipError`] variant so callers can degrade gracefully (spec R1/R3):
//! the runner and parser NEVER panic.

/// All the ways running or parsing a SCIP index can fail.
#[derive(Debug, thiserror::Error)]
pub enum ScipError {
    /// `scip-typescript` (or the `npx` launcher) could not be executed at all.
    #[error("scip-typescript not runnable: {0}")]
    ToolUnavailable(String),

    /// `npm install` did not complete successfully in the project directory.
    #[error("npm install failed in {dir}: {msg}")]
    InstallFailed {
        /// The project directory the install was attempted in.
        dir: String,
        /// Captured diagnostic (exit status and/or stderr tail).
        msg: String,
    },

    /// `scip-typescript` ran but exited non-zero.
    #[error("scip-typescript failed (exit {code}): {stderr}")]
    IndexFailed {
        /// The process exit code (`-1` when terminated by a signal).
        code: i32,
        /// Captured stderr from the failed run.
        stderr: String,
    },

    /// `scip-typescript` did not finish within the configured timeout.
    #[error("scip-typescript timed out after {secs}s")]
    Timeout {
        /// The timeout that was exceeded, in seconds.
        secs: u64,
    },

    /// The emitted (or supplied) bytes were not a parseable SCIP index.
    #[error("could not parse SCIP index: {0}")]
    Parse(String),

    /// An underlying I/O failure (reading the index, spawning a process, ...).
    #[error("io error: {0}")]
    Io(String),
}
