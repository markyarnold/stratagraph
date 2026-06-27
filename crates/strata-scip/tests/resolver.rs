//! Hermetic parser/resolver tests.
//!
//! These load the **committed** `index.scip` fixtures (generated once by
//! `scip-typescript` and checked in) and require no Node at test time. The
//! query positions below were read directly out of those indexes.
//!
//! Fixture sources (for reference):
//!   sample/src/b.ts: `export function foo() {}`
//!   sample/src/a.ts: line 0 `import { foo as bar } from "./b";`
//!                    line 1 `export function run() { bar(); }`
//!                    line 2 `export function ext() { return Math.max(1, 2); }`
//!   unicode/src/u.ts: line 0 `export function café() {}`
//!                     line 1 `export function caller() { café(); }`

use strata_scip::{Position, ScipResolver};

/// The committed sample index (aliased import + an external `Math.max` call).
const SAMPLE_INDEX: &[u8] = include_bytes!("fixtures/sample/index.scip");
/// The committed unicode index (non-ASCII `café` identifier, spec A3).
const UNICODE_INDEX: &[u8] = include_bytes!("fixtures/unicode/index.scip");

fn sample() -> ScipResolver {
    ScipResolver::from_bytes(SAMPLE_INDEX).expect("sample index parses")
}

fn unicode() -> ScipResolver {
    ScipResolver::from_bytes(UNICODE_INDEX).expect("unicode index parses")
}

// Test 1: the index parses and reports the expected shape.
#[test]
fn parses_sample_index_with_expected_stats() {
    let r = sample();
    let stats = r.stats();
    assert_eq!(stats.documents, 2, "sample has b.ts + a.ts");
    assert!(
        stats.definitions >= 1,
        "expected at least one definition, got {}",
        stats.definitions
    );
    assert!(stats.occurrences > 0, "expected occurrences");
}

// Test 2 (headline): the aliased-import call resolves through `foo as bar` to
// the real `foo` definition in b.ts.
#[test]
fn aliased_import_call_resolves_to_real_definition() {
    let r = sample();
    // `bar()` callee in `run` body is at a.ts line 1, range [24, 27).
    let target = r
        .resolve_at("src/a.ts", Position::new(1, 25))
        .expect("bar() call resolves");

    assert_eq!(
        target.def_file.as_deref(),
        Some("src/b.ts"),
        "SCIP resolved the alias to b.ts's definition"
    );
    assert!(
        target.moniker.contains("foo()"),
        "moniker should identify foo, got {}",
        target.moniker
    );
    assert!(!target.is_external);
    // foo is defined at b.ts line 0, identifier starting at char 16.
    assert_eq!(target.def_position, Some(Position::new(0, 16)));
}

// Test 3: the import site (the `foo` in `import { foo as bar }`) also resolves
// to b.ts's foo.
#[test]
fn import_site_resolves_to_definition() {
    let r = sample();
    // `foo` in the import clause is at a.ts line 0, range [9, 12).
    let target = r
        .resolve_at("src/a.ts", Position::new(0, 10))
        .expect("import-site foo resolves");

    assert_eq!(target.def_file.as_deref(), Some("src/b.ts"));
    assert!(target.moniker.contains("foo()"), "got {}", target.moniker);
    assert!(!target.is_external);
}

// Test 4: a position covered by no occurrence yields None.
#[test]
fn no_occurrence_returns_none() {
    let r = sample();
    // a.ts line 1: `export function run() { bar(); }` — char 22 is the space
    // between `run()` (ends at 19) and `bar` (starts at 24); no occurrence.
    assert!(r.resolve_at("src/a.ts", Position::new(1, 22)).is_none());
    // A position well past the end of any line: also None.
    assert!(r.resolve_at("src/a.ts", Position::new(1, 200)).is_none());
    // An unknown file: None.
    assert!(r.resolve_at("src/nope.ts", Position::new(0, 0)).is_none());
}

// Test 5 (spec A3): non-ASCII alignment. The `café()` call resolves to the
// `café` definition — proving the UTF-16 code-unit offsets are handled across a
// multi-byte character. If the encoding conversion were wrong (e.g. treating
// SCIP offsets as UTF-8 bytes), the queried position would miss the occurrence.
#[test]
fn unicode_call_resolves_across_non_ascii_identifier() {
    let r = unicode();
    // `café()` call is at u.ts line 1, range [27, 31) in UTF-16 code units.
    // `é` counts as ONE unit here: the definition range is [16, 20) = 16 + 4
    // ("café"), not 16 + 5 it would be under UTF-8 bytes.
    let target = r
        .resolve_at("src/u.ts", Position::new(1, 28))
        .expect("café() call resolves");

    assert_eq!(target.def_file.as_deref(), Some("src/u.ts"));
    assert!(
        target.moniker.contains("café"),
        "moniker should identify café, got {}",
        target.moniker
    );
    assert!(!target.is_external);
    // The definition identifier starts at line 0, char 16 (after
    // "export function ", 16 chars). Its end (proving the unit) is asserted via
    // the dedicated encoding test below.
    assert_eq!(target.def_position, Some(Position::new(0, 16)));
}

// Test 5b: pin the empirical encoding evidence so a regression in how we decode
// ranges (or a change in what scip-typescript emits) is caught explicitly.
// Querying the LAST UTF-16 unit of `café` (char 19, the `é`) must still hit the
// definition; querying char 20 (one past, where UTF-8 bytes would still place
// `é`) must NOT — that boundary is what distinguishes UTF-16 from UTF-8.
#[test]
fn unicode_definition_range_is_utf16_code_units() {
    let r = unicode();
    // café definition spans [16, 20): chars 16..=19 are inside, 20 is past end.
    assert!(
        r.resolve_at("src/u.ts", Position::new(0, 19)).is_some(),
        "char 19 (é, last UTF-16 unit of café) is inside the definition"
    );
    assert!(
        r.resolve_at("src/u.ts", Position::new(0, 20)).is_none(),
        "char 20 is one past café's end; if this resolved, ranges would be \
         mis-decoded as UTF-8 bytes (which would push the end to 21)"
    );
}

// Test 6: a symbol with no first-party definition (the external `Math.max` lib
// symbol) resolves with is_external = true and def_file = None.
#[test]
fn external_symbol_has_no_first_party_definition() {
    let r = sample();
    // `max` callee in `Math.max(1, 2)` is at a.ts line 2, range [36, 39).
    let target = r
        .resolve_at("src/a.ts", Position::new(2, 37))
        .expect("Math.max call resolves to a symbol");

    assert!(
        target.is_external,
        "Math.max is defined in node_modules/typescript, not first-party"
    );
    assert_eq!(target.def_file, None);
    assert_eq!(target.def_position, None);
    assert!(
        target.moniker.contains("typescript") && target.moniker.contains("max"),
        "moniker should point at the typescript lib max(), got {}",
        target.moniker
    );
}
