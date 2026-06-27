//! A store with same-file helper calls (Extracted) and a struct+impl whose
//! `self.` methods resolve to the enclosing type (Inferred).

pub struct Store {
    pub items: Vec<i64>,
}

impl Store {
    pub fn new() -> Store {
        // Same-file bare call -> Extracted.
        Store { items: empty() }
    }

    pub fn count(&self) -> usize {
        // self.<method> on the enclosing type -> Inferred.
        self.snapshot().len()
    }

    pub fn snapshot(&self) -> Vec<i64> {
        self.items.clone()
    }

    pub fn sum(&self) -> i64 {
        // self.<method> -> Inferred.
        let s = self.snapshot();
        fold(&s)
    }
}

impl Default for Store {
    fn default() -> Store {
        // Type-qualified `Store::new()` cross... actually same-file -> Extracted.
        Store::new()
    }
}

pub fn empty() -> Vec<i64> {
    Vec::new()
}

pub fn fold(items: &[i64]) -> i64 {
    // Same-file bare call -> Extracted.
    reduce(items)
}

pub fn reduce(items: &[i64]) -> i64 {
    let mut total = 0;
    for i in items {
        total += i;
    }
    total
}
