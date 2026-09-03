//! Reader trait — pluggable session-log reader abstraction.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};

use crate::error::JilogReviewError;

// ---------------------------------------------------------------------------
// Message — Schema-B chat message (Anthropic-style content blocks)
// ---------------------------------------------------------------------------

/// A single message from a transcript.jsonl line (Schema-B format).
/// Port from opsctl/crates/opsctl/src/review_nightly.rs verbatim.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Message {
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub content: Option<serde_json::Value>,
    #[serde(default)]
    pub name: Option<String>,
}

// ---------------------------------------------------------------------------
// SessionEvent — kernel-ish session events for health detectors
// ---------------------------------------------------------------------------

/// Kernel-ish session events for detectors that need more than messages
/// (see [`crate::health`]). Produced by [`Reader::load_events`]; readers
/// whose source format has no event stream return `Ok(None)` and health
/// detectors simply produce nothing for them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionEvent {
    pub kind: SessionEventKind,
    pub timestamp: DateTime<Utc>,
    /// Tool name for `ToolCall` events; None otherwise.
    pub tool_name: Option<String>,
    /// Kind-specific payload. For `ToolCall` this is the canonical
    /// (key-sorted) JSON serialization of the tool arguments, so identical
    /// arguments compare equal as strings.
    pub detail: Option<String>,
}

/// The event classes health detectors care about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionEventKind {
    /// A context compaction ran.
    Compaction,
    /// The session was resumed.
    Resume,
    /// A tool was invoked.
    ToolCall,
    /// The LLM produced a response.
    LlmResponse,
    /// The user submitted a message.
    UserMessage,
}

// ---------------------------------------------------------------------------
// SessionStats — observed per-session usage/cost
// ---------------------------------------------------------------------------

/// Per-session usage and spend, as observed in the session files.
///
/// Produced by [`Reader::load_stats`]. jilog reports spend it observed —
/// it does not fetch prices, maintain rate tables, or reconcile with
/// provider billing.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SessionStats {
    /// Sum of per-call `cost_usd`. String-decimal to preserve upstream
    /// precision; None when no call carried a cost (e.g. unpriced models).
    pub cost_usd: Option<String>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Sub-agent role parsed from the session-id suffix (`<uuid>_<role>`),
    /// if any.
    pub role: Option<String>,
    /// Model → summed cost (string-decimal), for calls that carried a cost.
    pub model_costs: std::collections::BTreeMap<String, String>,
}

/// Parse the sub-agent role from a session id of the form `<uuid>_<role>`.
/// Returns None for root sessions (no underscore) and empty suffixes.
pub fn parse_session_role(session_id: &str) -> Option<String> {
    match session_id.split_once('_') {
        Some((_, role)) if !role.is_empty() => Some(role.to_string()),
        _ => None,
    }
}

/// Session-id prefix Amplifier gives sub-agent sessions
/// (`0000000000000000-<hex>_<role>`). A session carrying it was spawned and
/// driven by a parent agent, not by a person.
pub const SUB_AGENT_PREFIX: &str = "0000000000000000";

/// True for sub-agent (parent-driven) sessions — the only autonomy marker
/// the transcripts carry. Prompt count is not one: an interactive root
/// session with five user prompts produced a 44-call stretch Joi ruled
/// normal (jilog#5a9h), so detectors that reason about "no user
/// intervention" use this prefix, never the prompt count (jilog#42fd).
pub fn is_sub_agent_session(session_id: &str) -> bool {
    session_id.starts_with(SUB_AGENT_PREFIX)
}

// ---------------------------------------------------------------------------
// TranscriptHandle — a discovered transcript file
// ---------------------------------------------------------------------------

/// A discovered transcript file, returned by a Reader's discover() method.
#[derive(Debug, Clone)]
pub struct TranscriptHandle {
    /// Stable session identifier (reader-specific convention).
    pub session_id: String,
    /// Absolute path to the transcript file.
    pub path: PathBuf,
    /// Last-modified timestamp.
    pub modified: DateTime<Utc>,
    /// The reader that produced this handle (e.g. "amplifier", "claude-code").
    pub reader_name: String,
    /// Which bot produced this session (e.g. "jibot", "bifbot"). None for
    /// coding-harness sessions. When set, the session is treated as a chat
    /// conversation: signals are stamped with persona/channel and the
    /// chat-tuned correction detector is used.
    pub persona: Option<String>,
    /// Which group/surface the session serves (e.g. a WhatsApp group name).
    /// None when unknown or not applicable.
    pub channel: Option<String>,
}

// ---------------------------------------------------------------------------
// Reader trait
// ---------------------------------------------------------------------------

/// Pluggable session-log reader.
///
/// Implementations discover transcript files and parse them into [`Message`]
/// slices. The two built-in readers are [`readers::AmplifierReader`] and
/// [`readers::ClaudeCodeReader`].
pub trait Reader: Send + Sync {
    /// Stable name used in digest output and logs (e.g. "amplifier").
    fn name(&self) -> &str;

    /// Discover transcript files modified at-or-after `since`.
    fn discover(&self, since: DateTime<Utc>) -> Result<Vec<TranscriptHandle>, JilogReviewError>;

    /// Parse a single transcript file into messages.
    /// Implementations MUST silently skip unparseable lines.
    fn load(&self, handle: &TranscriptHandle) -> Result<Vec<Message>, JilogReviewError>;

    /// Optional richer event stream for health-pattern detection.
    ///
    /// Default: `Ok(None)` — the reader has messages only, and health
    /// detectors produce nothing for its sessions. Implementations MUST
    /// silently skip unparseable lines and events without a parseable
    /// timestamp (the window-based detectors depend on real timestamps).
    fn load_events(
        &self,
        _handle: &TranscriptHandle,
    ) -> Result<Option<Vec<SessionEvent>>, JilogReviewError> {
        Ok(None)
    }

    /// Optional per-session usage/spend stats for cost-weighted digests.
    ///
    /// Default: `Ok(None)` — the reader's source format carries no usage
    /// data, and that session simply doesn't contribute to the digest's
    /// Spend section.
    fn load_stats(
        &self,
        _handle: &TranscriptHandle,
    ) -> Result<Option<SessionStats>, JilogReviewError> {
        Ok(None)
    }
}

// ---------------------------------------------------------------------------
// ProcessedSessions — dedup file for "session already processed"
// ---------------------------------------------------------------------------

/// Persistent set of session IDs that have already been processed.
///
/// Unlike the opsctl version, `mark()` only updates the in-memory set.
/// Call `save()` to write the full set to disk.
pub struct ProcessedSessions {
    seen: HashSet<String>,
}

impl ProcessedSessions {
    /// Load from `path`, creating an empty set if the file doesn't exist.
    pub fn load(path: &Path) -> Result<Self, JilogReviewError> {
        let mut seen = HashSet::new();
        if path.exists() {
            let content = std::fs::read_to_string(path)?;
            for line in content.lines() {
                let s = line.trim();
                if !s.is_empty() {
                    seen.insert(s.to_string());
                }
            }
        }
        Ok(Self { seen })
    }

    /// Return true if `session_id` has already been processed.
    pub fn contains(&self, session_id: &str) -> bool {
        self.seen.contains(session_id)
    }

    /// Mark `session_id` as processed (in-memory only; call `save()` to persist).
    pub fn mark(&mut self, session_id: &str) {
        self.seen.insert(session_id.to_string());
    }

    /// Remove `session_id` from the processed set (in-memory only), so the
    /// next run rescans it. Used when a session's required tracker
    /// operations failed — persisting it as processed would silently drop
    /// its signals forever (jilog#1dvk).
    pub fn unmark(&mut self, session_id: &str) {
        self.seen.remove(session_id);
    }
}

/// Write `content` to `path` atomically: a uniquely named, exclusively
/// created temp file in the same directory, then rename. An interruption or
/// short write can never leave partial state behind — a truncated sidecar
/// or processed file silently loses retry/dedup records (fresheyes
/// 2026-08-26 rounds 3-4). The temp name embeds the pid and a process-wide
/// counter and is opened with `create_new`, so it never truncates an
/// unrelated file, follows a planted symlink, or collides with the
/// destination or a concurrent writer.
fn write_atomic(path: &Path, content: &str) -> Result<(), JilogReviewError> {
    use std::io::Write as _;
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let base = path
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_string());
    loop {
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tmp = path.with_file_name(format!(
            ".{}.{}.{}.tmp",
            base,
            std::process::id(),
            n
        ));
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp) {
            Ok(mut f) => {
                if let Err(e) = f.write_all(content.as_bytes()).and_then(|_| f.sync_all()) {
                    drop(f);
                    let _ = std::fs::remove_file(&tmp);
                    return Err(e.into());
                }
                drop(f);
                // Preserve the destination's permissions across the
                // replace — a fresh temp file would otherwise reset a
                // deliberately tightened mode to the umask default.
                if let Ok(meta) = std::fs::metadata(path) {
                    let _ = std::fs::set_permissions(&tmp, meta.permissions());
                }
                if let Err(e) = std::fs::rename(&tmp, path) {
                    let _ = std::fs::remove_file(&tmp);
                    return Err(e.into());
                }
                return Ok(());
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
}

// ---------------------------------------------------------------------------
// RetrySessions — sidecar for sessions whose tracker ops failed (jilog#1dvk)
// ---------------------------------------------------------------------------

/// Sessions whose required tracker operations failed on a previous run,
/// with each session's transcript `modified` timestamp.
///
/// Unmarking a failed session from [`ProcessedSessions`] is not enough to
/// retry it: discovery is bounded by the run's `since` cutoff, and a daily
/// job's next window usually no longer covers the failed session's
/// transcript (fresheyes 2026-08-26 on jilog#1dvk). This sidecar records
/// the failed sessions' modified times so the next run can widen its
/// discovery window back to the oldest pending retry.
///
/// File format: one `<RFC3339 modified>\t<session_id>` per line.
///
/// [`RETRY_LOOKBACK_CAP_DAYS`] bounds how far a pending entry may widen a
/// run's discovery window past its `since` (and doubles as the GC horizon
/// for entries that can no longer be rediscovered).
pub struct RetrySessions {
    entries: std::collections::HashMap<String, chrono::DateTime<chrono::Utc>>,
}

/// Upper bound on how far the retry sidecar may widen the discovery window
/// past the caller's `since` (jilog#1dvk). A permanently failing session
/// stops widening the window after this many days; its retry entry keeps
/// being rewritten, but scans stay bounded.
pub const RETRY_LOOKBACK_CAP_DAYS: i64 = 14;

impl RetrySessions {
    /// An empty queue, touching no filesystem path.
    pub fn empty() -> Self {
        Self { entries: std::collections::HashMap::new() }
    }

    /// Load from `path`; missing file = empty queue. Malformed lines are
    /// skipped with a warning (a corrupt sidecar must not kill the run —
    /// worst case is a narrower window, i.e. the pre-sidecar behavior).
    pub fn load(path: &Path) -> Result<Self, JilogReviewError> {
        let mut entries = std::collections::HashMap::new();
        if path.exists() {
            let content = std::fs::read_to_string(path)?;
            for line in content.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                match line.split_once('\t') {
                    Some((ts, id)) => match chrono::DateTime::parse_from_rfc3339(ts) {
                        Ok(t) => {
                            entries.insert(id.to_string(), t.with_timezone(&chrono::Utc));
                        }
                        Err(e) => {
                            // Keep the entry alive rather than dropping the
                            // retry — the real modified time is unknowable,
                            // so assume the oldest age that still survives
                            // GC (the cap is measured from the run's
                            // `since`, which is at least "now" minus the
                            // window): this widens discovery as far as the
                            // cap allows, maximizing the chance the old
                            // transcript is rediscovered (fresheyes rounds
                            // 4-5; a "now" stamp never widened the window).
                            let assumed = chrono::Utc::now()
                                - chrono::Duration::days(RETRY_LOOKBACK_CAP_DAYS)
                                + chrono::Duration::minutes(5);
                            tracing::warn!(
                                "retry-sessions: bad timestamp '{}' for {} — keeping entry, assuming modified {}: {}",
                                ts, id, assumed.to_rfc3339(), e
                            );
                            entries.insert(id.to_string(), assumed);
                        }
                    },
                    None => tracing::warn!(
                        "retry-sessions: malformed line '{}' (no id recoverable) — dropped",
                        line
                    ),
                }
            }
        }
        Ok(Self { entries })
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The pending entries: session_id → transcript modified time.
    pub fn entries(&self) -> &std::collections::HashMap<String, chrono::DateTime<chrono::Utc>> {
        &self.entries
    }

    /// The oldest pending retry's transcript modified time — the point the
    /// next run's discovery window must reach back to.
    pub fn min_modified(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        self.entries.values().min().copied()
    }

    /// Overwrite `path` with `entries` (empty map truncates the file).
    pub fn save_entries(
        path: &Path,
        entries: &std::collections::HashMap<String, chrono::DateTime<chrono::Utc>>,
    ) -> Result<(), JilogReviewError> {
        let mut sorted: Vec<(&String, &chrono::DateTime<chrono::Utc>)> =
            entries.iter().collect();
        sorted.sort();
        let mut content = String::with_capacity(sorted.len() * 64);
        for (id, ts) in sorted {
            content.push_str(&ts.to_rfc3339());
            content.push('\t');
            content.push_str(id);
            content.push('\n');
        }
        write_atomic(path, &content)
    }
}

impl ProcessedSessions {
    /// Write the full set of session IDs to `path` (sorted, one per line,
    /// atomic temp-file + rename).
    pub fn save(&self, path: &Path) -> Result<(), JilogReviewError> {
        let mut sorted: Vec<&String> = self.seen.iter().collect();
        sorted.sort();
        let mut content = String::with_capacity(sorted.len() * 32);
        for id in sorted {
            content.push_str(id);
            content.push('\n');
        }
        write_atomic(path, &content)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("jilog-test-reader")
            .join(name);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn parse_session_role_suffix_forms() {
        assert_eq!(parse_session_role("abc-123_explore").as_deref(), Some("explore"));
        // Everything after the FIRST underscore is the role (roles may
        // themselves contain underscores; uuids never do).
        assert_eq!(parse_session_role("abc-123_web_search").as_deref(), Some("web_search"));
        assert_eq!(parse_session_role("abc-123"), None);
        assert_eq!(parse_session_role("abc-123_"), None);
        assert_eq!(parse_session_role(""), None);
    }

    #[test]
    fn processed_sessions_persist_across_load() {
        let dir = test_dir("processed");
        let path = dir.join("processed.txt");

        let mut p = ProcessedSessions::load(&path).unwrap();
        assert!(!p.contains("a"));
        p.mark("a");
        p.mark("b");
        assert!(p.contains("a"));
        p.save(&path).unwrap();

        // Re-load: persists.
        let p2 = ProcessedSessions::load(&path).unwrap();
        assert!(p2.contains("a"));
        assert!(p2.contains("b"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn processed_sessions_idempotent_mark() {
        let dir = test_dir("processed-idempotent");
        let path = dir.join("processed.txt");
        let mut p = ProcessedSessions::load(&path).unwrap();
        p.mark("a");
        p.mark("a"); // second call must not duplicate
        p.save(&path).unwrap();
        let content = fs::read_to_string(&path).unwrap();
        assert_eq!(content.lines().count(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn retry_sessions_round_trip_and_min_modified() {
        let dir = test_dir("retry-sessions");
        let path = dir.join("retry-sessions.txt");
        let older = chrono::Utc::now() - chrono::Duration::hours(30);
        let newer = chrono::Utc::now() - chrono::Duration::hours(2);
        let entries = std::collections::HashMap::from([
            ("sess-old".to_string(), older),
            ("sess-new".to_string(), newer),
        ]);
        RetrySessions::save_entries(&path, &entries).unwrap();
        let loaded = RetrySessions::load(&path).unwrap();
        assert!(!loaded.is_empty());
        // RFC3339 round-trip is second-preserving; compare timestamps.
        assert_eq!(
            loaded.min_modified().unwrap().timestamp(),
            older.timestamp(),
            "min_modified must be the oldest entry"
        );
        // Empty save truncates.
        RetrySessions::save_entries(&path, &std::collections::HashMap::new()).unwrap();
        let drained = RetrySessions::load(&path).unwrap();
        assert!(drained.is_empty());
        assert_eq!(drained.min_modified(), None);
        // Missing file loads empty.
        let missing = RetrySessions::load(&dir.join("nope.txt")).unwrap();
        assert!(missing.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn processed_sessions_unmark_removes_for_retry() {
        let dir = test_dir("processed-unmark");
        let path = dir.join("processed.txt");
        let mut p = ProcessedSessions::load(&path).unwrap();
        p.mark("keep");
        p.mark("retry-me");
        p.unmark("retry-me");
        p.unmark("never-marked"); // no-op, must not panic
        p.save(&path).unwrap();
        let p2 = ProcessedSessions::load(&path).unwrap();
        assert!(p2.contains("keep"));
        assert!(!p2.contains("retry-me"), "unmarked session must not persist (jilog#1dvk)");
        let _ = fs::remove_dir_all(&dir);
    }
}
