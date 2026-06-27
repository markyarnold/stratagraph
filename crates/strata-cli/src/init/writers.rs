//! The three merge-safe file writers behind `strata init`.
//!
//! Merge-safety is the product: **foreign bytes are preserved exactly**. Each
//! writer reports what it did via [`Outcome`] so `init` can print an honest
//! per-file summary and a re-run can report all-Unchanged.
//!
//! 1. [`upsert_managed_block`] — owns only the text *between* the
//!    `<!-- strata:begin -->` / `<!-- strata:end -->` markers in a Markdown file;
//!    never touches a byte outside them (used for `CLAUDE.md`, `AGENTS.md`,
//!    `.kiro/steering/strata.md`).
//! 2. [`merge_json`] — deep-merges **only our keys** into possibly-existing
//!    JSON, preserving every foreign key/server/hook; malformed existing JSON is
//!    an actionable error, never a clobber.
//! 3. [`write_owned`] — for files wholly ours (skills, Kiro hooks): write or
//!    overwrite freely, reporting Created/Updated/Unchanged by byte-compare.

use std::fmt;
use std::path::Path;

use serde_json::Value;

/// What a writer did to a file. Returned by every writer so `init` can print a
/// truthful summary and a second run can report all-Unchanged (idempotency).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The file did not exist and was created.
    Created,
    /// The file existed and its content (or our slice of it) changed.
    Updated,
    /// The file existed and is already exactly what we would write.
    Unchanged,
}

impl Outcome {
    /// Lower-case label for the per-file summary line.
    pub fn label(self) -> &'static str {
        match self {
            Outcome::Created => "created",
            Outcome::Updated => "updated",
            Outcome::Unchanged => "unchanged",
        }
    }
}

/// A writer failure that must not clobber the user's file (malformed JSON, IO).
#[derive(Debug)]
pub enum WriteError {
    /// Existing JSON could not be parsed — the file is left untouched.
    MalformedJson { path: String, detail: String },
    /// An underlying filesystem error.
    Io { path: String, detail: String },
}

impl fmt::Display for WriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WriteError::MalformedJson { path, detail } => write!(
                f,
                "{path} contains invalid JSON ({detail}); fix or remove it, then re-run `strata init` (left untouched)"
            ),
            WriteError::Io { path, detail } => write!(f, "could not write {path}: {detail}"),
        }
    }
}

impl std::error::Error for WriteError {}

// ── managed-block writer ─────────────────────────────────────────────────────

/// The opening marker for a StrataGraph-managed block in a Markdown file.
pub const BLOCK_BEGIN: &str = "<!-- strata:begin -->";
/// The closing marker for a StrataGraph-managed block in a Markdown file.
pub const BLOCK_END: &str = "<!-- strata:end -->";

/// Upsert `body` between the managed markers in the file at `path`.
///
/// * file missing → create it containing just the marked block;
/// * markers absent → append the marked block (one blank line after existing
///   content), preserving every existing byte;
/// * markers present → replace **only** the text between them, leaving every
///   byte before `<!-- strata:begin -->` and after `<!-- strata:end -->`
///   exactly as it was.
///
/// `body` is the inner content; the markers themselves are written by this
/// function, so callers pass only the steering text.
pub fn upsert_managed_block(path: &Path, body: &str) -> Result<Outcome, WriteError> {
    let existing = read_to_string_opt(path)?;
    let block = render_block(body);

    let new_contents = match &existing {
        None => format!("{block}\n"),
        Some(current) => match splice_block(current, &block) {
            Some(spliced) => spliced,
            None => append_block(current, &block),
        },
    };

    commit(path, existing.as_deref(), &new_contents)
}

/// Render the full managed block (markers + body) without a trailing newline.
fn render_block(body: &str) -> String {
    format!(
        "{BLOCK_BEGIN}\n{}\n{BLOCK_END}",
        body.trim_end_matches('\n')
    )
}

/// If `current` contains both markers, return it with the inter-marker region
/// (markers included) replaced by `block`. Bytes outside the markers are
/// preserved verbatim. Returns `None` if either marker is missing.
fn splice_block(current: &str, block: &str) -> Option<String> {
    let begin = current.find(BLOCK_BEGIN)?;
    // The end marker must come after the begin marker; search from begin.
    let end_rel = current[begin..].find(BLOCK_END)?;
    let end = begin + end_rel + BLOCK_END.len();
    let mut out = String::with_capacity(current.len());
    out.push_str(&current[..begin]);
    out.push_str(block);
    out.push_str(&current[end..]);
    Some(out)
}

/// Append `block` to `current`, separated by exactly one blank line, preserving
/// all of `current` verbatim.
fn append_block(current: &str, block: &str) -> String {
    let trimmed = current.trim_end_matches('\n');
    if trimmed.is_empty() {
        format!("{block}\n")
    } else {
        format!("{trimmed}\n\n{block}\n")
    }
}

// ── JSON deep-merge writer ───────────────────────────────────────────────────

/// Deep-merge `ours` into the JSON at `path`, preserving every foreign key.
///
/// The merge is recursive object-overlay: for each key in `ours`, if both sides
/// hold an object the merge recurses; otherwise our value replaces (this is how
/// `mcpServers.strata` is upserted while foreign servers under `mcpServers` are
/// left intact). Arrays and scalars from `ours` overwrite at their key — callers
/// keep array idempotency structural (see [`upsert_hook`]).
///
/// * file missing → created containing `ours` (pretty, 2-space);
/// * file present, valid JSON → merged and rewritten only if the bytes change;
/// * file present, **malformed** JSON → [`WriteError::MalformedJson`], file
///   left untouched (never clobbered).
pub fn merge_json(path: &Path, ours: &Value) -> Result<Outcome, WriteError> {
    edit_json(path, |root| merge_into(root, ours))
}

/// Read the JSON at `path` (an empty object if missing), let `edit` mutate the
/// parsed root, then write it back (pretty, 2-space, trailing newline) only if
/// the bytes change.
///
/// This is the shared, malformed-safe, idempotent core behind [`merge_json`] and
/// the array-aware hook merge in the agent renderers (which need to upsert a
/// marker-identified entry inside an existing hook array — something a plain
/// object-overlay cannot do). Malformed existing JSON → [`WriteError::MalformedJson`],
/// file untouched.
pub fn edit_json(path: &Path, edit: impl FnOnce(&mut Value)) -> Result<Outcome, WriteError> {
    let existing = read_to_string_opt(path)?;

    let mut root = match &existing {
        None => Value::Object(serde_json::Map::new()),
        Some(text) => serde_json::from_str(text).map_err(|e| WriteError::MalformedJson {
            path: display(path),
            detail: e.to_string(),
        })?,
    };

    edit(&mut root);

    // Pretty-print with a trailing newline (POSIX text file convention).
    let mut rendered = serde_json::to_string_pretty(&root).map_err(|e| WriteError::Io {
        path: display(path),
        detail: e.to_string(),
    })?;
    rendered.push('\n');

    commit(path, existing.as_deref(), &rendered)
}

/// Navigate to (creating if absent) the array at `obj[event].(matcher-group)`
/// for Claude Code hook settings, returning a mutable handle to the array of
/// hook **matcher-groups** under `root.hooks.<event>`.
///
/// Claude Code's settings shape is `hooks: { <Event>: [ {matcher, hooks:[…]}, … ] }`.
/// We upsert our marker-identified matcher-group into that array, preserving
/// foreign groups. Returns `None` only if a *foreign* non-array value already
/// occupies `hooks.<event>` (we never clobber a foreign shape).
pub fn hooks_event_array<'a>(root: &'a mut Value, event: &str) -> Option<&'a mut Vec<Value>> {
    let obj = root.as_object_mut()?;
    let hooks = obj
        .entry("hooks")
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    let hooks_obj = hooks.as_object_mut()?;
    let ev = hooks_obj
        .entry(event)
        .or_insert_with(|| Value::Array(Vec::new()));
    ev.as_array_mut()
}

/// Recursively overlay `src` onto `dst`: object keys merge, everything else is
/// replaced at the key. Foreign keys in `dst` are never removed.
fn merge_into(dst: &mut Value, src: &Value) {
    match (dst, src) {
        (Value::Object(dst_map), Value::Object(src_map)) => {
            for (k, v) in src_map {
                match dst_map.get_mut(k) {
                    Some(existing) => merge_into(existing, v),
                    None => {
                        dst_map.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        (dst_slot, src_val) => {
            *dst_slot = src_val.clone();
        }
    }
}

/// Ensure a hook entry whose command contains the `strata-hook` marker token is
/// present exactly once under `arr`, replacing any prior StrataGraph entry in place.
///
/// Hook arrays are merged **structurally**, not by object-overlay: our entry is
/// identified by the marker token appearing anywhere in its serialized form, so
/// a re-run updates the StrataGraph hook without ever duplicating it and without
/// disturbing foreign hooks (which never carry the token).
pub fn upsert_hook(arr: &mut Vec<Value>, ours: Value) {
    let is_strata = |v: &Value| serialized_contains_marker(v);
    // Drop any prior StrataGraph-owned entries (there should be at most one).
    arr.retain(|v| !is_strata(v));
    arr.push(ours);
}

/// The marker token embedded in every StrataGraph-owned hook command, used to
/// recognise our hook entries structurally for idempotent re-runs.
pub const HOOK_MARKER: &str = "strata-hook";

/// True if the serialized JSON of `v` contains the [`HOOK_MARKER`] token.
fn serialized_contains_marker(v: &Value) -> bool {
    serde_json::to_string(v)
        .map(|s| s.contains(HOOK_MARKER))
        .unwrap_or(false)
}

// ── owned-file writer ────────────────────────────────────────────────────────

/// Write `contents` to `path` (a file StrataGraph wholly owns: a skill, a Kiro hook).
///
/// Parent directories are created. Reports [`Outcome::Created`] if the file was
/// absent, [`Outcome::Unchanged`] if it already holds exactly `contents`, else
/// [`Outcome::Updated`]. Safe to overwrite because no foreign bytes live here.
pub fn write_owned(path: &Path, contents: &str) -> Result<Outcome, WriteError> {
    let existing = read_to_string_opt(path)?;
    commit(path, existing.as_deref(), contents)
}

// ── shared IO ────────────────────────────────────────────────────────────────

/// Read a file to a `String`, mapping a *missing* file to `None` and any other
/// IO error to [`WriteError::Io`].
fn read_to_string_opt(path: &Path) -> Result<Option<String>, WriteError> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(WriteError::Io {
            path: display(path),
            detail: e.to_string(),
        }),
    }
}

/// Write `new_contents` to `path` (creating parent dirs), reporting the outcome
/// relative to `existing`. A no-op write (bytes already match) does not touch
/// the file and reports [`Outcome::Unchanged`].
fn commit(path: &Path, existing: Option<&str>, new_contents: &str) -> Result<Outcome, WriteError> {
    match existing {
        Some(current) if current == new_contents => return Ok(Outcome::Unchanged),
        _ => {}
    }
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| WriteError::Io {
                path: display(parent),
                detail: e.to_string(),
            })?;
        }
    }
    std::fs::write(path, new_contents).map_err(|e| WriteError::Io {
        path: display(path),
        detail: e.to_string(),
    })?;
    Ok(if existing.is_some() {
        Outcome::Updated
    } else {
        Outcome::Created
    })
}

/// Lossy path display for error messages.
fn display(path: &Path) -> String {
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    // ── managed-block writer ─────────────────────────────────────────────────

    #[test]
    fn managed_block_creates_file_when_missing() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("CLAUDE.md");
        let out = upsert_managed_block(&p, "hello body").unwrap();
        assert_eq!(out, Outcome::Created);
        let got = std::fs::read_to_string(&p).unwrap();
        assert_eq!(got, format!("{BLOCK_BEGIN}\nhello body\n{BLOCK_END}\n"));
    }

    #[test]
    fn managed_block_appends_preserving_foreign_bytes() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("CLAUDE.md");
        let user = "# My project\n\nSome prose the user wrote.\n";
        std::fs::write(&p, user).unwrap();

        let out = upsert_managed_block(&p, "STRATA BODY").unwrap();
        // The file existed (only without our block), so appending is an Update.
        assert_eq!(out, Outcome::Updated);
        let got = std::fs::read_to_string(&p).unwrap();
        // Every original byte is still present, verbatim, at the front.
        assert!(
            got.starts_with(user),
            "user prose must be preserved: {got:?}"
        );
        assert!(got.contains(&format!("{BLOCK_BEGIN}\nSTRATA BODY\n{BLOCK_END}")));
    }

    #[test]
    fn managed_block_replaces_in_place_preserving_surrounding_bytes() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("AGENTS.md");
        let before = "TOP PROSE\n";
        let after = "\nBOTTOM PROSE\n";
        let original = format!("{before}{BLOCK_BEGIN}\nOLD BODY\n{BLOCK_END}{after}");
        std::fs::write(&p, &original).unwrap();

        let out = upsert_managed_block(&p, "NEW BODY").unwrap();
        assert_eq!(out, Outcome::Updated);
        let got = std::fs::read_to_string(&p).unwrap();
        // Bytes above and below the markers are byte-identical; only the inner
        // body changed.
        assert_eq!(
            got,
            format!("{before}{BLOCK_BEGIN}\nNEW BODY\n{BLOCK_END}{after}")
        );
        assert!(got.starts_with("TOP PROSE\n"));
        assert!(got.ends_with("\nBOTTOM PROSE\n"));
        assert!(!got.contains("OLD BODY"));
    }

    #[test]
    fn managed_block_preserves_a_foreign_gitnexus_block() {
        // The coexistence case: a gitnexus block must survive untouched when we
        // add ours (different markers, so it is foreign to us).
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("CLAUDE.md");
        let gitnexus = "<!-- gitnexus:start -->\nGN CONTENT\n<!-- gitnexus:end -->\n";
        std::fs::write(&p, gitnexus).unwrap();

        upsert_managed_block(&p, "STRATA BODY").unwrap();
        let got = std::fs::read_to_string(&p).unwrap();
        assert!(
            got.contains("<!-- gitnexus:start -->\nGN CONTENT\n<!-- gitnexus:end -->"),
            "foreign gitnexus block must be preserved exactly: {got:?}"
        );
        assert!(got.contains("STRATA BODY"));
    }

    #[test]
    fn managed_block_second_run_is_unchanged() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("CLAUDE.md");
        upsert_managed_block(&p, "BODY").unwrap();
        let second = upsert_managed_block(&p, "BODY").unwrap();
        assert_eq!(second, Outcome::Unchanged, "idempotent re-run");
    }

    // ── JSON deep-merge writer ───────────────────────────────────────────────

    #[test]
    fn json_creates_file_when_missing() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join(".mcp.json");
        let out = merge_json(
            &p,
            &json!({"mcpServers": {"strata": {"command": "strata"}}}),
        )
        .unwrap();
        assert_eq!(out, Outcome::Created);
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v["mcpServers"]["strata"]["command"], "strata");
    }

    #[test]
    fn json_merge_preserves_foreign_servers() {
        // The critical merge-safety case for JSON: a foreign MCP server under
        // `mcpServers` must be intact after we add ours alongside it.
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join(".mcp.json");
        std::fs::write(
            &p,
            r#"{
  "mcpServers": {
    "gitnexus": { "command": "npx", "args": ["gitnexus", "mcp"] }
  },
  "someUserKey": 42
}
"#,
        )
        .unwrap();

        let out = merge_json(
            &p,
            &json!({"mcpServers": {"strata": {"command": "strata", "args": ["mcp"]}}}),
        )
        .unwrap();
        assert_eq!(out, Outcome::Updated);

        let v: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        // Foreign server preserved exactly.
        assert_eq!(v["mcpServers"]["gitnexus"]["command"], "npx");
        assert_eq!(v["mcpServers"]["gitnexus"]["args"][0], "gitnexus");
        // Foreign top-level key preserved.
        assert_eq!(v["someUserKey"], 42);
        // Ours added alongside.
        assert_eq!(v["mcpServers"]["strata"]["command"], "strata");
    }

    #[test]
    fn json_malformed_existing_is_error_and_file_untouched() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join(".mcp.json");
        let garbage = "{ this is not valid json ]";
        std::fs::write(&p, garbage).unwrap();

        let err = merge_json(&p, &json!({"mcpServers": {"strata": {}}})).unwrap_err();
        assert!(
            matches!(err, WriteError::MalformedJson { .. }),
            "malformed JSON must be a MalformedJson error, got {err:?}"
        );
        // File left exactly as it was — never clobbered.
        assert_eq!(std::fs::read_to_string(&p).unwrap(), garbage);
    }

    #[test]
    fn json_second_run_is_unchanged() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join(".mcp.json");
        let ours = json!({"mcpServers": {"strata": {"command": "strata", "args": ["mcp"]}}});
        merge_json(&p, &ours).unwrap();
        let second = merge_json(&p, &ours).unwrap();
        assert_eq!(second, Outcome::Unchanged, "idempotent JSON re-run");
    }

    #[test]
    fn upsert_hook_replaces_strata_entry_and_keeps_foreign() {
        // A foreign hook (no marker) plus our hook (marker). Re-running upsert
        // must keep the foreign hook and replace ours in place (no duplicate).
        let foreign = json!({"type": "command", "command": "echo foreign"});
        let ours_v1 = json!({"type": "command", "command": "sh -c '# strata-hook v1'"});
        let ours_v2 = json!({"type": "command", "command": "sh -c '# strata-hook v2'"});

        let mut arr = vec![foreign.clone(), ours_v1];
        upsert_hook(&mut arr, ours_v2.clone());

        assert_eq!(arr.len(), 2, "no duplicate StrataGraph hook");
        assert!(arr.contains(&foreign), "foreign hook preserved");
        assert!(arr.contains(&ours_v2), "StrataGraph hook updated to v2");
        assert!(
            !arr.iter()
                .any(|v| v["command"].as_str() == Some("sh -c '# strata-hook v1'")),
            "old StrataGraph hook removed"
        );
    }

    // ── owned-file writer ────────────────────────────────────────────────────

    #[test]
    fn owned_create_update_unchanged_cycle() {
        let tmp = TempDir::new().unwrap();
        let p = tmp
            .path()
            .join(".claude/skills/strata/strata-guide/SKILL.md");

        let c = write_owned(&p, "v1").unwrap();
        assert_eq!(c, Outcome::Created);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "v1");

        let u = write_owned(&p, "v2").unwrap();
        assert_eq!(u, Outcome::Updated);

        let n = write_owned(&p, "v2").unwrap();
        assert_eq!(n, Outcome::Unchanged);
    }
}
