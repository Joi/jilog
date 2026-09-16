//! ClaudeCodeReader — scans one or more Claude Code project roots
//! (`<root>/**/*.jsonl`).
//!
//! Claude Code nests projects by hashed cwd and stores multiple event shapes
//! per file. Only Schema-B-shaped lines (those with `role`/`content`/`name`
//! fields) are kept; everything else is silently skipped.
//!
//! Roots come from `roots` plus, for each `profile_parents` entry, every
//! `<parent>/<name>/projects` directory that exists at scan time
//! (`~/.claude-pool/profiles/seat-NN`, `~/.claude-profiles/glm`, …).
//! Sessions found under a profile parent carry `seat = <name>`; sessions
//! under an explicit root carry no seat, so the single-root default is
//! unchanged (jilog#heyg).

use std::path::PathBuf;

use chrono::{DateTime, TimeZone, Utc};

use crate::error::JilogReviewError;
use crate::reader::{Message, Reader, TranscriptHandle};
use crate::util::expand_tilde;

/// Profile parents `discover_profiles = true` adds (tilde-expanded at
/// config time): the Claude pool seats and the context profiles.
pub const DEFAULT_PROFILE_PARENTS: [&str; 2] = ["~/.claude-pool/profiles", "~/.claude-profiles"];

/// Reader for Claude Code session transcripts.
///
/// Glob: `<root>/**/*.jsonl` (recursive) for every root.
/// Session ID = filename stem of the .jsonl file.
pub struct ClaudeCodeReader {
    /// Explicit roots. Walked first; a session under one never carries a seat.
    pub roots: Vec<PathBuf>,
    /// Parents whose `<name>/projects` children are scanned as roots, each
    /// tagged `seat = <name>`.
    pub profile_parents: Vec<PathBuf>,
}

impl ClaudeCodeReader {
    /// One explicit root, no profile discovery.
    pub fn new(projects_dir: impl Into<PathBuf>) -> Self {
        Self::from_roots(vec![projects_dir.into()])
    }

    /// Explicit roots replace the default and do not imply profile discovery.
    pub fn from_roots(roots: Vec<PathBuf>) -> Self {
        Self { roots, profile_parents: Vec::new() }
    }

    /// Add parents whose `<name>/projects` children are scanned as roots.
    pub fn with_profile_parents(mut self, parents: Vec<PathBuf>) -> Self {
        self.profile_parents = parents;
        self
    }

    /// Use the default Claude Code projects directory: `~/.claude/projects`.
    pub fn from_default() -> Self {
        Self::new(expand_tilde("~/.claude/projects"))
    }

    /// Explicit roots FIRST (sorted, deduplicated), then the profile roots
    /// discovered under each parent (sorted, deduplicated, minus any that
    /// is also explicit). `discover` walks them in this order and keeps the
    /// first occurrence of each canonical file, so an explicit root always
    /// wins over a profile copy of the same directory — including when the
    /// explicit root is a symlink to a profile home (the retained handle
    /// path is the explicit one, and `seat()` sees it as explicit).
    fn scan_roots(&self) -> Result<Vec<PathBuf>, JilogReviewError> {
        let mut explicit = self.roots.clone();
        explicit.sort();
        explicit.dedup();
        let mut profiles = Vec::new();
        for parent in &self.profile_parents {
            match std::fs::read_dir(parent) {
                Ok(entries) => {
                    for entry in entries {
                        let entry = match entry {
                            Ok(entry) => entry,
                            Err(error) => {
                                tracing::warn!(
                                    "claude-code: profile entry under {}: {error}",
                                    parent.display()
                                );
                                continue;
                            }
                        };
                        let projects = entry.path().join("projects");
                        if projects.is_dir() && !explicit.contains(&projects) {
                            profiles.push(projects);
                        }
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                // A stray file or a permission problem under $HOME must not
                // cost the explicit roots their scan: skip this parent.
                Err(e) => tracing::warn!(
                    "claude-code: profile parent {} skipped: {e}",
                    parent.display()
                ),
            }
        }
        profiles.sort();
        profiles.dedup();
        explicit.extend(profiles);
        Ok(explicit)
    }
}

impl Reader for ClaudeCodeReader {
    fn name(&self) -> &str {
        "claude-code"
    }

    fn discover(&self, since: DateTime<Utc>) -> Result<Vec<TranscriptHandle>, JilogReviewError> {
        let mut handles = Vec::new();
        let mut seen = std::collections::HashSet::new();
        // Canonical-file dedupe exists for roots that overlap (an explicit
        // root that is also a profile home, or a symlinked root). A legacy
        // single-root configuration keeps its exact pre-0.8 discovery — a
        // transcript reachable twice through symlinks inside that root is
        // still scanned twice, as it always was (byte-identity lens).
        let dedupe = self.roots.len() > 1 || !self.profile_parents.is_empty();

        for root in self.scan_roots()? {
            if !root.is_dir() {
                continue;
            }
            // Recursive walk for all .jsonl files; the root is escaped so a
            // glob metacharacter in a path matches literally.
            let pattern =
                format!("{}/**/*.jsonl", glob::Pattern::escape(&root.to_string_lossy()));
            let entries = match glob::glob(&pattern) {
                Ok(e) => e,
                Err(e) => {
                    return Err(JilogReviewError::Reader(format!(
                        "claude-code: glob error: {}",
                        e
                    )));
                }
            };

            for entry in entries.flatten() {
                if entry.is_dir() {
                    continue;
                }
                // One canonical file is scanned once even when two roots
                // reach it (an explicit symlink root plus its profile copy).
                if dedupe {
                    let canonical = match std::fs::canonicalize(&entry) {
                        Ok(path) => path,
                        Err(error) => {
                            tracing::warn!("claude-code: {}: {error}", entry.display());
                            continue;
                        }
                    };
                    if !seen.insert(canonical) {
                        continue;
                    }
                }

                let session_id = entry
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| entry.display().to_string());

                let modified = match entry.metadata().and_then(|m| m.modified()) {
                    Ok(st) => {
                        let secs = st
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs())
                            .unwrap_or(0);
                        Utc.timestamp_opt(secs as i64, 0).single().unwrap_or(Utc::now())
                    }
                    Err(_) => Utc::now(),
                };

                if modified < since {
                    continue;
                }

                handles.push(TranscriptHandle {
                    session_id,
                    path: entry,
                    modified,
                    reader_name: self.name().to_string(),
                    persona: None,
                    channel: None,
                });
            }
        }

        handles.sort_by_key(|h| h.path.clone());
        Ok(handles)
    }

    fn seat(&self, handle: &TranscriptHandle) -> Option<String> {
        // Explicit roots win: a session under one carries no seat even when
        // the same directory is reachable through a profile parent.
        if self.roots.iter().any(|root| handle.path.starts_with(root)) {
            return None;
        }
        // `<parent>/<name>/projects/...` → `<name>`.
        self.profile_parents.iter().find_map(|parent| {
            handle
                .path
                .strip_prefix(parent)
                .ok()
                .and_then(|rest| rest.components().next())
                .and_then(|c| c.as_os_str().to_str())
                .map(|s| s.to_string())
        })
    }

    fn load(&self, handle: &TranscriptHandle) -> Result<Vec<Message>, JilogReviewError> {
        let content = std::fs::read_to_string(&handle.path)?;
        let mut out = Vec::new();
        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            // Parse to Value first so we can handle both shapes:
            //   legacy flat: {"role": "...", "content": "..."}
            //   wrapped:     {"type": "user", "message": {"role": "...", "content": "..."}}
            let value: serde_json::Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if let Some(msg) = extract_message(&value) {
                out.push(msg);
            }
        }
        Ok(out)
    }
}

/// Pull a Schema-B `Message` out of a Claude Code JSONL line.
///
/// Real Claude Code lines wrap the chat content under `message`:
///   `{"type":"user","message":{"role":"user","content":...}}`
///   `{"type":"assistant","message":{"role":"assistant","content":[...]}}`
///
/// Legacy/synthetic lines also appear (`{"role":"user","content":"..."}`),
/// so both shapes are accepted. Returns `None` for non-chat lines such as
/// `type=last-prompt`, `permission-mode`, `attachment`, `ai-title`,
/// `file-history-snapshot`.
fn extract_message(value: &serde_json::Value) -> Option<Message> {
    // Wrapped form: inner `message` object carries role/content.
    if let Some(inner) = value.get("message") {
        if inner.get("role").is_some() {
            // Carry the outer "type" forward as a hint when the inner has no name.
            let mut msg: Message = serde_json::from_value(inner.clone()).ok()?;
            if msg.role.is_some() {
                if msg.name.is_none() {
                    if let Some(t) = value.get("type").and_then(|v| v.as_str()) {
                        if t != "user" && t != "assistant" && t != "system" {
                            msg.name = Some(t.to_string());
                        }
                    }
                }
                return Some(msg);
            }
        }
    }

    // Flat form: top-level role/content.
    let msg: Message = serde_json::from_value(value.clone()).ok()?;
    if msg.role.is_some() {
        Some(msg)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use chrono::Duration;

    fn test_dir(name: &str) -> PathBuf {
        std::env::temp_dir()
            .join("jilog-test-claude-code")
            .join(name)
    }

    #[test]
    fn claude_code_reader_basic() {
        let root = test_dir("basic");
        let _ = fs::remove_dir_all(&root);
        let proj = root.join("-hash-dir");
        fs::create_dir_all(&proj).unwrap();

        let content = r#"{"role":"user","content":"hello"}
{"someOtherShape": true, "notAMessage": 1}
{"role":"assistant","content":"world"}"#;
        fs::write(proj.join("session-uuid.jsonl"), content).unwrap();

        let reader = ClaudeCodeReader::new(&root);
        let since = Utc::now() - Duration::days(1);
        let handles = reader.discover(since).unwrap();
        assert_eq!(handles.len(), 1);
        assert_eq!(handles[0].session_id, "session-uuid");
        assert_eq!(handles[0].reader_name, "claude-code");

        let msgs = reader.load(&handles[0]).unwrap();
        // Only lines with `role` field are kept
        assert_eq!(msgs.len(), 2);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn claude_code_reader_unwraps_message_field() {
        let root = test_dir("wrapped");
        let _ = fs::remove_dir_all(&root);
        let proj = root.join("-Users-joi-repos-x");
        fs::create_dir_all(&proj).unwrap();

        // Real-shape Claude Code lines: type+message wrapper, mixed with
        // session-meta lines that have no role and must be skipped.
        let content = r#"{"type":"last-prompt","leafUuid":"abc","sessionId":"x"}
{"type":"user","uuid":"u1","message":{"role":"user","content":"hello"}}
{"type":"permission-mode","permissionMode":"acceptEdits","sessionId":"x"}
{"type":"assistant","uuid":"a1","message":{"role":"assistant","content":[{"type":"text","text":"hi"}]}}
{"type":"ai-title","title":"t","sessionId":"x"}"#;
        fs::write(proj.join("real-session.jsonl"), content).unwrap();

        let reader = ClaudeCodeReader::new(&root);
        let since = Utc::now() - Duration::days(1);
        let handles = reader.discover(since).unwrap();
        assert_eq!(handles.len(), 1);

        let msgs = reader.load(&handles[0]).unwrap();
        assert_eq!(msgs.len(), 2, "user + assistant only — meta lines skipped");
        assert_eq!(msgs[0].role.as_deref(), Some("user"));
        assert_eq!(msgs[1].role.as_deref(), Some("assistant"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn paths_and_profiles_are_scanned_and_seat_tagged() {
        let tree = tempfile::tempdir().unwrap();
        let main = tree.path().join(".claude/projects/-Users-joi-x");
        let pool = tree.path().join(".claude-pool/profiles");
        let ctx = tree.path().join(".claude-profiles");
        fs::create_dir_all(&main).unwrap();
        fs::write(main.join("main-session.jsonl"), "{\"role\":\"user\",\"content\":\"hi\"}\n").unwrap();
        for seat in ["seat-01", "seat-06"] {
            let p = pool.join(seat).join("projects/-Users-joi-y");
            fs::create_dir_all(&p).unwrap();
            fs::write(p.join(format!("{seat}-session.jsonl")), "{\"role\":\"user\",\"content\":\"hi\"}\n").unwrap();
        }
        // A profile dir without projects/ and a stray file are ignored.
        fs::create_dir_all(pool.join("seat-12")).unwrap();
        fs::write(pool.join("README"), "not a profile").unwrap();
        let glm = ctx.join("glm/projects/-p");
        fs::create_dir_all(&glm).unwrap();
        fs::write(glm.join("glm-session.jsonl"), "{\"role\":\"user\",\"content\":\"hi\"}\n").unwrap();

        let reader = ClaudeCodeReader::from_roots(vec![tree.path().join(".claude/projects")])
            .with_profile_parents(vec![pool.clone(), ctx.clone()]);
        let since = Utc::now() - Duration::days(1);
        let handles = reader.discover(since).unwrap();
        let mut ids: Vec<&str> = handles.iter().map(|h| h.session_id.as_str()).collect();
        ids.sort();
        assert_eq!(ids, ["glm-session", "main-session", "seat-01-session", "seat-06-session"]);
        for h in &handles {
            let seat = reader.seat(h);
            match h.session_id.as_str() {
                "main-session" => assert_eq!(seat, None, "explicit roots carry no seat"),
                "glm-session" => assert_eq!(seat.as_deref(), Some("glm")),
                "seat-01-session" => assert_eq!(seat.as_deref(), Some("seat-01")),
                "seat-06-session" => assert_eq!(seat.as_deref(), Some("seat-06")),
                other => panic!("unexpected {other}"),
            }
        }
        // A missing profile parent is not an error; duplicate roots scan once.
        let reader = ClaudeCodeReader::from_roots(vec![
            tree.path().join(".claude/projects"),
            tree.path().join(".claude/projects"),
        ])
        .with_profile_parents(vec![tree.path().join("nope")]);
        assert_eq!(reader.discover(since).unwrap().len(), 1);
        // Empty roots scan nothing.
        assert!(ClaudeCodeReader::from_roots(vec![]).discover(since).unwrap().is_empty());
        // A profile parent that is a FILE is skipped with a warning; the
        // explicit root still scans.
        let reader = ClaudeCodeReader::from_roots(vec![tree.path().join(".claude/projects")])
            .with_profile_parents(vec![pool.join("README")]);
        assert_eq!(reader.discover(since).unwrap().len(), 1);
        // Glob metacharacters in a root or a profile name match literally.
        let weird = tree.path().join("we[i]rd*");
        let wp = weird.join("q?/projects/-p");
        fs::create_dir_all(&wp).unwrap();
        fs::write(wp.join("weird-session.jsonl"), "{\"role\":\"user\",\"content\":\"hi\"}\n").unwrap();
        let reader = ClaudeCodeReader::from_roots(vec![]).with_profile_parents(vec![weird.clone()]);
        let handles = reader.discover(since).unwrap();
        assert_eq!(handles.len(), 1, "metacharacter paths are escaped, not interpreted");
        assert_eq!(reader.seat(&handles[0]).as_deref(), Some("q?"));
        let reader = ClaudeCodeReader::from_roots(vec![weird.join("q?/projects")]);
        assert_eq!(reader.discover(since).unwrap().len(), 1);
        // An explicit root that is ALSO reachable through a profile parent
        // carries no seat and is scanned once: explicit roots win.
        let reader = ClaudeCodeReader::from_roots(vec![pool.join("seat-06").join("projects")])
            .with_profile_parents(vec![pool.clone()]);
        let handles = reader.discover(since).unwrap();
        assert_eq!(handles.len(), 2, "seat-01 (profile) + seat-06 (explicit), each once");
        for h in &handles {
            match h.session_id.as_str() {
                "seat-06-session" => assert_eq!(reader.seat(h), None, "explicit root wins over the profile parent"),
                "seat-01-session" => assert_eq!(reader.seat(h).as_deref(), Some("seat-01")),
                other => panic!("unexpected {other}"),
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn explicit_symlink_root_beats_the_profile_copy() {
        let tree = tempfile::tempdir().unwrap();
        let pool = tree.path().join(".claude-pool/profiles");
        let home = pool.join("seat-06");
        let proj = home.join("projects/-p");
        fs::create_dir_all(&proj).unwrap();
        fs::write(proj.join("s.jsonl"), "{\"role\":\"user\",\"content\":\"hi\"}\n").unwrap();
        // Lexical order is irrelevant: explicit roots are walked first.
        let link = tree.path().join("zzz-link-to-seat-06");
        std::os::unix::fs::symlink(&home, &link).unwrap();
        let reader = ClaudeCodeReader::from_roots(vec![link.join("projects")])
            .with_profile_parents(vec![pool.clone()]);
        let since = Utc::now() - Duration::days(1);
        let handles = reader.discover(since).unwrap();
        assert_eq!(handles.len(), 1, "one canonical file, scanned once");
        assert!(
            handles[0].path.starts_with(link.join("projects")),
            "the explicit (symlink) path is retained: {}",
            handles[0].path.display()
        );
        assert_eq!(reader.seat(&handles[0]), None, "explicit root wins, even through a symlink");
    }

    #[cfg(unix)]
    #[test]
    fn legacy_single_root_keeps_symlink_duplicates_but_multi_root_dedupes() {
        let tree = tempfile::tempdir().unwrap();
        let root = tree.path().join("projects");
        let proj = root.join("-p");
        fs::create_dir_all(&proj).unwrap();
        fs::write(proj.join("real.jsonl"), "{\"role\":\"user\",\"content\":\"hi\"}\n").unwrap();
        std::os::unix::fs::symlink(proj.join("real.jsonl"), proj.join("alias.jsonl")).unwrap();
        let since = Utc::now() - Duration::days(1);
        // One explicit root, nothing else: exactly the pre-0.8 behaviour,
        // two handles with two session ids.
        let legacy = ClaudeCodeReader::new(&root);
        let ids: Vec<String> = legacy.discover(since).unwrap().into_iter().map(|h| h.session_id).collect();
        assert_eq!(ids, ["alias", "real"]);
        // The same root twice, or with a profile parent: one canonical file.
        let multi = ClaudeCodeReader::from_roots(vec![root.clone(), root.clone()]);
        assert_eq!(multi.discover(since).unwrap().len(), 1);
        let with_profiles = ClaudeCodeReader::new(&root).with_profile_parents(vec![tree.path().join("nope")]);
        assert_eq!(with_profiles.discover(since).unwrap().len(), 1);
    }

    #[test]
    fn default_reader_has_no_profile_parents() {
        let r = ClaudeCodeReader::from_default();
        assert_eq!(r.roots.len(), 1);
        assert!(r.profile_parents.is_empty(), "profile discovery is opt-in");
    }
}
