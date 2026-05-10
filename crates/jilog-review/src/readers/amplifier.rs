//! AmplifierReader — scans `~/.amplifier/projects/*/transcript.jsonl`.

use std::path::PathBuf;

use chrono::{DateTime, TimeZone, Utc};

use crate::error::JilogReviewError;
use crate::reader::{Message, Reader, TranscriptHandle};
use crate::util::expand_tilde;

/// Reader for Amplifier-style session transcripts.
///
/// Discovers `<projects_dir>/*/transcript.jsonl` files and parses them
/// into Schema-B messages. Session ID = parent directory basename.
pub struct AmplifierReader {
    pub projects_dir: PathBuf,
}

impl AmplifierReader {
    pub fn new(projects_dir: impl Into<PathBuf>) -> Self {
        Self { projects_dir: projects_dir.into() }
    }

    /// Use the default Amplifier projects directory: `~/.amplifier/projects`.
    pub fn from_default() -> Self {
        Self::new(expand_tilde("~/.amplifier/projects"))
    }
}

impl Reader for AmplifierReader {
    fn name(&self) -> &str {
        "amplifier"
    }

    fn discover(&self, since: DateTime<Utc>) -> Result<Vec<TranscriptHandle>, JilogReviewError> {
        let mut handles = Vec::new();

        if !self.projects_dir.exists() {
            return Ok(handles);
        }

        // Walk one level: <projects_dir>/<session_id>/transcript.jsonl
        let entries = match std::fs::read_dir(&self.projects_dir) {
            Ok(e) => e,
            Err(err) => {
                return Err(JilogReviewError::Reader(format!(
                    "amplifier: cannot read {}: {}",
                    self.projects_dir.display(),
                    err
                )));
            }
        };

        for entry in entries.flatten() {
            let session_dir = entry.path();
            if !session_dir.is_dir() {
                continue;
            }
            let transcript = session_dir.join("transcript.jsonl");
            if !transcript.exists() {
                continue;
            }

            let session_id = match session_dir.file_name().and_then(|n| n.to_str()) {
                Some(s) => s.to_string(),
                None => continue,
            };

            let modified = match transcript.metadata().and_then(|m| m.modified()) {
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
                path: transcript,
                modified,
                reader_name: self.name().to_string(),
            });
        }

        handles.sort_by_key(|h| h.path.clone());
        Ok(handles)
    }

    fn load(&self, handle: &TranscriptHandle) -> Result<Vec<Message>, JilogReviewError> {
        load_transcript_jsonl(&handle.path)
    }
}

/// Parse a transcript.jsonl file into messages.
/// Invalid lines are skipped silently (matches Python behavior).
pub(crate) fn load_transcript_jsonl(path: &std::path::Path) -> Result<Vec<Message>, JilogReviewError> {
    let content = std::fs::read_to_string(path)?;
    let mut out = Vec::new();
    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(msg) = serde_json::from_str::<Message>(line) {
            out.push(msg);
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Tests — ported from opsctl/crates/opsctl/src/review_nightly.rs
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use chrono::Duration;

    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("jilog-test-amplifier")
            .join(name);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn discover_transcripts_finds_nested() {
        let root = test_dir("discover");
        let s1 = root.join("session-aaaa");
        // Amplifier only looks one level deep
        fs::create_dir_all(&s1).unwrap();
        fs::write(s1.join("transcript.jsonl"), "").unwrap();
        // Random other file should be ignored.
        fs::write(s1.join("events.jsonl"), "").unwrap();

        let reader = AmplifierReader::new(&root);
        let since = Utc::now() - Duration::days(1);
        let found = reader.discover(since).unwrap();
        // One transcript found
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].session_id, "session-aaaa");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn load_transcript_skips_blank_and_invalid() {
        let dir = test_dir("load");
        let path = dir.join("transcript.jsonl");
        fs::write(
            &path,
            r#"{"role":"user","content":"hello world"}

not json
{"role":"assistant","content":"hi"}
{"unclosed":"json
"#,
        )
        .unwrap();

        let msgs = load_transcript_jsonl(&path).unwrap();
        assert_eq!(msgs.len(), 2);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn amplifier_reader_basic() {
        let root = test_dir("basic");
        let sess = root.join("sess-abc");
        fs::create_dir_all(&sess).unwrap();
        let transcript_content = r#"{"role":"user","content":"hello"}
{"role":"assistant","content":"hi there"}"#;
        fs::write(sess.join("transcript.jsonl"), transcript_content).unwrap();

        let reader = AmplifierReader::new(&root);
        let since = Utc::now() - Duration::days(1);
        let handles = reader.discover(since).unwrap();
        assert_eq!(handles.len(), 1);
        assert_eq!(handles[0].session_id, "sess-abc");
        assert_eq!(handles[0].reader_name, "amplifier");

        let msgs = reader.load(&handles[0]).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role.as_deref(), Some("user"));
        let _ = fs::remove_dir_all(&root);
    }
}
