//! The estate membership marker: `.strata/estate.toml`.
//!
//! Written by `strata index --workspace` into each member repo's `.strata/`
//! directory. Records which estate manifest this repo belongs to and the
//! repo's declared name (the UID `package` used to estate-qualify its graph),
//! so commands/hooks run from inside a member repo can resolve the estate.
//! `.strata/` is gitignored, so the marker is never committed.

use std::path::{Path, PathBuf};

pub const ESTATE_MARKER: &str = "estate.toml";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EstateMarker {
    /// Resolved (absolute) path to the manifest, so it is re-findable from any cwd.
    pub manifest: PathBuf,
    /// `[workspace].name`.
    pub estate: String,
    /// This repo's `[[repos]].name` (the estate-qualified UID `package`).
    pub repo: String,
}

pub fn write_marker(strata_dir: &Path, marker: &EstateMarker) -> std::io::Result<()> {
    let body = toml::to_string_pretty(marker)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let final_path = strata_dir.join(ESTATE_MARKER);
    let tmp_path = strata_dir.join(format!("{ESTATE_MARKER}.{}.tmp", std::process::id()));
    std::fs::write(&tmp_path, body)?;
    std::fs::rename(&tmp_path, &final_path)?;
    Ok(())
}

pub fn read_marker(strata_dir: &Path) -> Option<EstateMarker> {
    let text = std::fs::read_to_string(strata_dir.join(ESTATE_MARKER)).ok()?;
    toml::from_str(&text).ok()
}
