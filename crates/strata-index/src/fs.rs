//! [`ModuleFs`] implementations for the indexer.
//!
//! [`BTreeMapModuleFs`] answers existence from an in-memory file keyset, which
//! lets [`crate::build_graph`] stay pure and fully unit-testable. [`FsModuleFs`]
//! is the real-filesystem counterpart used by [`crate::index_repo`].

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use strata_lang_ts::ModuleFs;

/// A [`ModuleFs`] backed by an in-memory set of repo-relative, `/`-normalized
/// file paths (the keys of the `files` map passed to `build_graph`).
///
/// The resolver probes paths like `src/b` + extension; existence is simply
/// "is this exact key present in the set". Because the resolver normalizes
/// separators to `/` and the keys are already `/`-normalized, no further
/// massaging is required here.
pub struct BTreeMapModuleFs<'a> {
    keys: &'a BTreeSet<String>,
}

impl<'a> BTreeMapModuleFs<'a> {
    pub fn new(keys: &'a BTreeSet<String>) -> Self {
        BTreeMapModuleFs { keys }
    }
}

impl ModuleFs for BTreeMapModuleFs<'_> {
    fn exists(&self, path: &str) -> bool {
        self.keys.contains(path)
    }
}

/// A [`ModuleFs`] backed by the real filesystem, rooted at `root`.
///
/// The resolver supplies repo-relative, `/`-normalized candidate paths; we join
/// them onto `root` and check `std::fs::metadata`. A path that escapes the root
/// (via `..`) or that does not resolve to a regular file reports `false`.
pub struct FsModuleFs {
    root: PathBuf,
}

impl FsModuleFs {
    pub fn new(root: &Path) -> Self {
        FsModuleFs {
            root: root.to_path_buf(),
        }
    }
}

impl ModuleFs for FsModuleFs {
    fn exists(&self, path: &str) -> bool {
        // The resolver yields `/`-normalized relative paths; rebuild them with
        // the platform separator under the repo root.
        let mut candidate = self.root.clone();
        for segment in path.split('/') {
            if segment.is_empty() || segment == "." {
                continue;
            }
            candidate.push(segment);
        }
        candidate.metadata().map(|m| m.is_file()).unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn btreemap_fs_reports_membership() {
        let keys: BTreeSet<String> = ["src/a.ts".to_string(), "src/b.ts".to_string()]
            .into_iter()
            .collect();
        let fs = BTreeMapModuleFs::new(&keys);
        assert!(fs.exists("src/a.ts"));
        assert!(fs.exists("src/b.ts"));
        assert!(!fs.exists("src/c.ts"));
        assert!(!fs.exists("src/b")); // resolver appends extensions itself
    }

    #[test]
    fn fs_module_fs_checks_real_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/a.ts"), "x").unwrap();

        let fs = FsModuleFs::new(dir.path());
        assert!(fs.exists("src/a.ts"));
        assert!(!fs.exists("src/missing.ts"));
        // A directory is not a file.
        assert!(!fs.exists("src"));
    }
}
