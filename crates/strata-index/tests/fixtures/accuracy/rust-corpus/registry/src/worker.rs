//! A worker exercising cross-module type-qualified calls (Inferred), a
//! cross-module UNIQUE bare name (Inferred), same-file calls (Extracted), and an
//! unknown-receiver method fan-out across two same-named methods (Ambiguous).

use crate::store::{reduce, Store};

pub struct Job {
    pub id: i64,
}

impl Job {
    pub fn run(&self) -> i64 {
        // self.<method> on the enclosing type -> Inferred.
        self.payload()
    }

    pub fn payload(&self) -> i64 {
        self.id * 2
    }
}

pub fn build_store() -> Store {
    // Type-qualified `Store::new()` (cross-file) -> Inferred (type-qualified rule).
    Store::new()
}

pub fn total(store: &Store) -> i64 {
    // `.sum()` on an unknown receiver: `sum` exists on Store (and `Tally::sum`
    // below) -> Ambiguous fan-out across both same-named methods.
    store.sum()
}

pub fn tally_total(t: &Tally) -> i64 {
    // The SAME `.sum()` fan-out, resolved by rust-analyzer to Tally::sum here.
    t.sum()
}

pub struct Tally {
    pub n: i64,
}

impl Tally {
    pub fn sum(&self) -> i64 {
        self.n
    }
}

pub fn crunch(items: &[i64]) -> i64 {
    // Cross-module UNIQUE bare name: `reduce` exists exactly once repo-wide (in
    // store.rs) -> Inferred (unique cross-module name).
    reduce(items)
}

pub fn pipeline(items: &[i64]) -> i64 {
    // Same-file bare calls -> Extracted.
    let s = stage_one(items);
    stage_two(s)
}

pub fn stage_one(items: &[i64]) -> i64 {
    crunch(items)
}

pub fn stage_two(value: i64) -> i64 {
    value + 1
}
