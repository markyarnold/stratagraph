//! Parse a SCIP index into a queryable resolver.
//!
//! The resolver is pure and hermetic: it is built from index bytes (the
//! checked-in fixtures in tests) with no Node involved. It answers
//! [`ScipResolver::resolve_at`] by finding the occurrence whose range *contains*
//! a source position, then looking up that symbol's first-party definition.

use std::collections::HashMap;
use std::path::Path;

use protobuf::Message;
use scip::types::Index;

use crate::error::ScipError;
use crate::model::{Position, ResolvedTarget, ScipStats};

/// Bit set in `Occurrence.symbol_roles` when the occurrence is a definition.
const ROLE_DEFINITION: i32 = 1;

/// A single occurrence within a file: a half-open range and the symbol it
/// references. (Whether the occurrence is itself a definition is recorded in
/// the separate symbol -> definition map, so it is not needed here.)
#[derive(Debug, Clone)]
struct Occ {
    start: Position,
    end: Position,
    symbol: String,
}

impl Occ {
    /// Whether this occurrence's half-open `[start, end)` range contains `pos`.
    ///
    /// Ranges may span multiple lines, so the comparison is lexicographic on
    /// `(line, character)`. A position exactly at `end` is *not* contained.
    fn contains(&self, pos: Position) -> bool {
        let after_start = (pos.line, pos.character) >= (self.start.line, self.start.character);
        let before_end = (pos.line, pos.character) < (self.end.line, self.end.character);
        after_start && before_end
    }

    /// The number of characters the range spans on a single line (used to pick
    /// the tightest covering occurrence). Multi-line ranges sort last.
    fn width(&self) -> u64 {
        if self.start.line == self.end.line {
            u64::from(self.end.character.saturating_sub(self.start.character))
        } else {
            u64::MAX
        }
    }
}

/// Where a symbol is defined (first occurrence with the `Definition` role).
#[derive(Debug, Clone)]
struct Definition {
    file: String,
    position: Position,
}

/// A parsed SCIP index, ready to resolve source positions to symbols.
pub struct ScipResolver {
    /// Per-file occurrence lists for the containment search.
    by_file: HashMap<String, Vec<Occ>>,
    /// Symbol moniker -> its first-party definition location.
    definitions: HashMap<String, Definition>,
    documents: usize,
    occurrences: usize,
}

impl ScipResolver {
    /// Parse a SCIP index file into a resolver.
    pub fn from_index_file(path: &Path) -> Result<ScipResolver, ScipError> {
        let bytes = std::fs::read(path).map_err(|e| ScipError::Io(e.to_string()))?;
        Self::from_bytes(&bytes)
    }

    /// Parse from raw index bytes (used by tests with the checked-in fixture).
    pub fn from_bytes(bytes: &[u8]) -> Result<ScipResolver, ScipError> {
        let index = Index::parse_from_bytes(bytes).map_err(|e| ScipError::Parse(e.to_string()))?;

        let mut by_file: HashMap<String, Vec<Occ>> = HashMap::new();
        let mut definitions: HashMap<String, Definition> = HashMap::new();
        let mut occurrences = 0usize;
        let documents = index.documents.len();

        for doc in &index.documents {
            let file = doc.relative_path.clone();
            let entries = by_file.entry(file.clone()).or_default();

            for occ in &doc.occurrences {
                let Some((start, end)) = parse_range(&occ.range) else {
                    // Malformed range: skip it rather than fail the whole parse
                    // (robustness — a single bad occurrence must not blind us).
                    continue;
                };
                occurrences += 1;

                let is_definition = (occ.symbol_roles & ROLE_DEFINITION) != 0;

                // A definition occurrence with a non-empty symbol records where
                // that symbol lives. Keep the first one seen per symbol.
                if is_definition && !occ.symbol.is_empty() {
                    definitions.entry(occ.symbol.clone()).or_insert(Definition {
                        file: file.clone(),
                        position: start,
                    });
                }

                if !occ.symbol.is_empty() {
                    entries.push(Occ {
                        start,
                        end,
                        symbol: occ.symbol.clone(),
                    });
                }
            }
        }

        Ok(ScipResolver {
            by_file,
            definitions,
            documents,
            occurrences,
        })
    }

    /// Resolve the symbol referenced at `(file, pos)` to its definition.
    ///
    /// Finds the occurrence whose range *contains* `pos` (robust to the caller
    /// pointing anywhere within the identifier; the tightest range wins when
    /// several overlap), then looks up that symbol's first-party definition.
    /// Returns `None` when no occurrence covers the position.
    pub fn resolve_at(&self, file: &str, pos: Position) -> Option<ResolvedTarget> {
        let occs = self.by_file.get(file)?;

        // Pick the tightest covering occurrence so that nested ranges resolve to
        // the most specific symbol.
        let hit = occs
            .iter()
            .filter(|o| o.contains(pos))
            .min_by_key(|o| o.width())?;

        let moniker = hit.symbol.clone();
        match self.definitions.get(&moniker) {
            Some(def) => Some(ResolvedTarget {
                moniker,
                def_file: Some(def.file.clone()),
                def_position: Some(def.position),
                is_external: false,
            }),
            None => Some(ResolvedTarget {
                moniker,
                def_file: None,
                def_position: None,
                is_external: true,
            }),
        }
    }

    /// Diagnostics: documents / occurrences / distinct definitions parsed.
    pub fn stats(&self) -> ScipStats {
        ScipStats {
            documents: self.documents,
            occurrences: self.occurrences,
            definitions: self.definitions.len(),
        }
    }
}

/// Decode a SCIP occurrence `range` into `(start, end)` positions.
///
/// SCIP ranges are either four elements `[startLine, startChar, endLine,
/// endChar]` or three elements `[startLine, startChar, endChar]` (the end line
/// equals the start line). Returns `None` for any other shape.
fn parse_range(range: &[i32]) -> Option<(Position, Position)> {
    let to_u32 = |v: i32| u32::try_from(v).ok();
    match range {
        [sl, sc, ec] => {
            let line = to_u32(*sl)?;
            Some((
                Position::new(line, to_u32(*sc)?),
                Position::new(line, to_u32(*ec)?),
            ))
        }
        [sl, sc, el, ec] => Some((
            Position::new(to_u32(*sl)?, to_u32(*sc)?),
            Position::new(to_u32(*el)?, to_u32(*ec)?),
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_range_three_and_four_elements() {
        assert_eq!(
            parse_range(&[1, 2, 5]),
            Some((Position::new(1, 2), Position::new(1, 5)))
        );
        assert_eq!(
            parse_range(&[1, 2, 3, 4]),
            Some((Position::new(1, 2), Position::new(3, 4)))
        );
        assert_eq!(parse_range(&[1, 2]), None);
        assert_eq!(parse_range(&[-1, 2, 3]), None);
    }

    #[test]
    fn contains_is_half_open() {
        let occ = Occ {
            start: Position::new(1, 24),
            end: Position::new(1, 27),
            symbol: "x".to_string(),
        };
        assert!(!occ.contains(Position::new(1, 23)));
        assert!(occ.contains(Position::new(1, 24)));
        assert!(occ.contains(Position::new(1, 26)));
        assert!(!occ.contains(Position::new(1, 27))); // end is exclusive
    }
}
