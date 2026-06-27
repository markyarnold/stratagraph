//! Resolution tests (brief Definition of Done #8–#13). Each uses a fake
//! `ModuleFs` seeded with a known set of existing paths.

use std::collections::HashSet;

use strata_lang_ts::{resolve, ModuleFs, ResolveOptions, ResolveResult};

/// In-memory file set for testing. Paths are matched exactly (already `/`-form).
struct FakeFs {
    files: HashSet<String>,
}

impl FakeFs {
    fn new(paths: &[&str]) -> FakeFs {
        FakeFs {
            files: paths.iter().map(|p| p.to_string()).collect(),
        }
    }
}

impl ModuleFs for FakeFs {
    fn exists(&self, path: &str) -> bool {
        self.files.contains(path)
    }
}

fn resolved(s: &str) -> ResolveResult {
    ResolveResult::Resolved(s.to_string())
}

fn external(s: &str) -> ResolveResult {
    ResolveResult::External(s.to_string())
}

// --- #8: relative import resolves via extension probing ----------------------

#[test]
fn relative_import_resolves_with_ts_extension() {
    let fs = FakeFs::new(&["src/util.ts"]);
    let opts = ResolveOptions::default();
    let got = resolve("./util", "src/a.ts", &opts, &fs);
    assert_eq!(got, resolved("src/util.ts"));
}

// --- #9: directory import resolves via index file ----------------------------

#[test]
fn relative_import_resolves_to_index_file() {
    let fs = FakeFs::new(&["src/widgets/index.tsx"]);
    let opts = ResolveOptions::default();
    let got = resolve("./widgets", "src/a.ts", &opts, &fs);
    assert_eq!(got, resolved("src/widgets/index.tsx"));
}

// --- #10: `..` is normalized correctly ---------------------------------------

#[test]
fn relative_import_normalizes_parent_segments() {
    let fs = FakeFs::new(&["src/lib/x.ts"]);
    let opts = ResolveOptions::default();
    // Importer is src/a/b.ts; "../lib/x" -> src/lib/x.
    let got = resolve("../lib/x", "src/a/b.ts", &opts, &fs);
    assert_eq!(got, resolved("src/lib/x.ts"));
}

// --- #11: tsconfig paths alias -----------------------------------------------

#[test]
fn tsconfig_paths_alias_resolves() {
    let fs = FakeFs::new(&["src/util.ts"]);
    let opts = ResolveOptions {
        base_url: Some(".".to_string()),
        paths: vec![("@app/*".to_string(), vec!["src/*".to_string()])],
    };
    let got = resolve("@app/util", "src/a.ts", &opts, &fs);
    assert_eq!(got, resolved("src/util.ts"));
}

// --- #12: bare specifiers are External ---------------------------------------

#[test]
fn bare_specifier_is_external() {
    let fs = FakeFs::new(&[]);
    let opts = ResolveOptions::default();
    assert_eq!(resolve("react", "src/a.ts", &opts, &fs), external("react"));
}

#[test]
fn scoped_bare_specifier_with_subpath_is_external_package() {
    let fs = FakeFs::new(&[]);
    let opts = ResolveOptions::default();
    assert_eq!(
        resolve("@scope/pkg/sub", "src/a.ts", &opts, &fs),
        external("@scope/pkg")
    );
}

// --- #13: unresolved relative import -----------------------------------------

#[test]
fn missing_relative_import_is_unresolved() {
    let fs = FakeFs::new(&[]);
    let opts = ResolveOptions::default();
    assert_eq!(
        resolve("./missing", "src/a.ts", &opts, &fs),
        ResolveResult::Unresolved
    );
}
