//! Embed an engine build id (short git hash + dirty marker) so every surface —
//! `strata --version`, the index summary, the desktop footer — can say exactly
//! which engine produced an answer. Born of a dogfood incident: a stale
//! running app reindexed a graph with an older engine and silently lost edges;
//! with the id surfaced everywhere, version skew is visible instead of spooky.

use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn main() {
    let hash = git(&["rev-parse", "--short=12", "HEAD"]);
    let dirty = git(&["status", "--porcelain"]).map(|s| !s.is_empty());
    let id = match (hash, dirty) {
        (Some(h), Some(true)) => format!("{h}-dirty"),
        (Some(h), _) => h,
        // No git (e.g. a published-crate build): honest unknown, never a guess.
        (None, _) => "unknown".to_string(),
    };
    println!("cargo:rustc-env=STRATA_ENGINE_ID={id}");
    // Re-stamp when the checked-out commit moves (HEAD covers branch switches;
    // the ref file under .git covers commits to the same branch).
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/index");
}
