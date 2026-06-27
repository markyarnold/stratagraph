use serde::{Deserialize, Serialize};

/// A deterministic, human-readable identity for a graph node.
///
/// Slice 1 uses a canonical key string (readable, easy to golden-test).
/// A compact hash and SCIP monikers can replace the internals later
/// without changing this type's interface.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Uid(pub String);

impl Uid {
    pub fn new(language: &str, package: &str, path: &str, fqn: &str, signature: &str) -> Uid {
        Uid(format!("{language}|{package}|{path}|{fqn}|{signature}"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Uid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_inputs_produce_same_uid() {
        let a = Uid::new("ts", "app", "src/a.ts", "foo", "()");
        let b = Uid::new("ts", "app", "src/a.ts", "foo", "()");
        assert_eq!(a, b);
    }

    #[test]
    fn different_inputs_produce_different_uid() {
        let a = Uid::new("ts", "app", "src/a.ts", "foo", "()");
        let b = Uid::new("ts", "app", "src/a.ts", "bar", "()");
        assert_ne!(a, b);
    }
}
