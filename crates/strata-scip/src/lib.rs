//! `strata-scip` — a self-contained SCIP adapter for TS/JS precise resolution.
//!
//! Two responsibilities, decoupled from `strata-core`:
//!
//! 1. **Runner** ([`run_scip`]): invoke `scip-typescript` out-of-process (pinned
//!    [`PINNED_SCIP_TYPESCRIPT_VERSION`]) to produce an `index.scip`. Bounded by
//!    a timeout, with captured output, and every failure mapped to a
//!    [`ScipError`] — it never panics and never hangs (spec R1/R3).
//! 2. **Parser + resolver** ([`ScipResolver`]): parse a SCIP index and resolve a
//!    source [`Position`] to the precise symbol it references and that symbol's
//!    definition ([`ResolvedTarget`]). Pure and hermetic — tested against
//!    checked-in `index.scip` fixtures with no Node at test time.
//!
//! Positions use SCIP's character encoding (UTF-16 code units; see [`Position`]).
//! Milestone 2 bridges [`ResolvedTarget`]s onto graph nodes.

mod error;
mod model;
mod resolver;
mod runner;

pub use error::ScipError;
pub use model::{Position, ResolvedTarget, ScipStats};
pub use resolver::ScipResolver;
pub use runner::{run_scip, RunOptions, PINNED_SCIP_TYPESCRIPT_VERSION};
