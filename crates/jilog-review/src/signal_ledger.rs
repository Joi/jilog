//! Signal ledger — one count and one filing per signal across runs
//! (jilog#1xmf).
//!
//! The processed-sessions file stops a clean session from being rescanned,
//! but a session can still be scanned again: a failed tracker create puts it
//! in the `.retry` sidecar, and a session whose new signals no digest has
//! rendered goes into the `.render` sidecar. Without a ledger every signal
//! of such a session was recounted in the next digest and offered to the
//! tracker again.
//!
//! Each signal carries two independent flags:
//! - `c` (counted): a written digest has rendered it. A signal without `c`
//!   is NEW and is rendered and counted in the next digest written.
//! - `f` (filed): the tracker accepted it. A signal without `f` is offered
//!   to the tracker whenever creates are attempted.
//!
//! File format: one line per signal,
//! `<first-seen date>\t<c|f|cf>\t<session modified|->\t<id>`, next to the
//! processed file as `<processed-file>.signals`. The session's latest seen
//! modified time decides whether the signal falls in a review window.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, NaiveDate, Utc};
use serde::Serialize;

use crate::error::JilogReviewError;
use crate::signal::Signal;

/// Entries whose first-seen date is older than this many days before the
/// run date are dropped on save. Every window the nightly jobs use is far
/// shorter, so a dropped entry can no longer be rescanned.
pub const LEDGER_RETENTION_DAYS: i64 = 90;

/// FNV-1a, 64-bit. Hand-written because its output must be stable across
/// Rust versions (std's `DefaultHasher` makes no such promise) and it needs
/// no new dependency.
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Encode fields unambiguously: `<byte length>:<bytes>` per field, so
/// `("ab", "c")` and `("a", "bc")` hash differently.
fn encode_fields(fields: &[&str]) -> Vec<u8> {
    let mut out = Vec::new();
    for f in fields {
        out.extend_from_slice(f.len().to_string().as_bytes());
        out.push(b':');
        out.extend_from_slice(f.as_bytes());
    }
    out
}

/// A signal's identity: `<kind>:<session_id>:<016x>`, the hash over the
/// fields that make the signal what it is. The session id is part of the
/// identity, so one signal seen under two reader-specific or day-chunk ids
/// is two signals (jilog#26w9, jilog#e31r).
///
/// A health pattern hashes its kind (and, for a stuck loop, the tool) but
/// not its description: the description carries counts — compactions,
/// resumes, repeats, tool calls — that grow while the session runs, and a
/// rescan of a longer session must not turn the same pattern into a new
/// signal. Several stuck loops in one session stay apart through the `#n`
/// suffix of [`assign_ids`].
pub fn signal_identity(signal: &Signal) -> String {
    let fields: Vec<&str> = match signal {
        Signal::Pattern(p) if p.pattern_kind == "stuck_loop" => {
            vec![&p.pattern_kind, backticked(&p.description).unwrap_or("")]
        }
        Signal::Pattern(p) if !p.pattern_kind.is_empty() => vec![&p.pattern_kind],
        Signal::Correction(c) => vec![&c.context],
        Signal::Error(e) => vec![&e.tool_name, &e.message],
        Signal::Workaround(w) => vec![&w.pattern, &w.context],
        Signal::Deferral(d) => vec![&d.item],
        Signal::Pattern(p) => vec![&p.pattern_kind, &p.description],
    };
    format!(
        "{}:{}:{:016x}",
        signal.kind(),
        signal.session_id(),
        fnv1a64(&encode_fields(&fields))
    )
}

/// The first `` `...` `` span of a pattern description — the tool name in
/// "stuck loop: `bash` called 4 times …".
fn backticked(text: &str) -> Option<&str> {
    let start = text.find('`')? + 1;
    let len = text[start..].find('`')?;
    Some(&text[start..start + len])
}

/// Identities for a run's signals, in order. The n-th repeat (n ≥ 2) of
/// one identity gets `#<n>`, so two genuine repeats in one session stay two
/// signals.
pub fn assign_ids(signals: &[Signal]) -> Vec<String> {
    let mut seen: HashMap<String, usize> = HashMap::new();
    signals
        .iter()
        .map(|s| {
            let id = signal_identity(s);
            let n = seen.entry(id.clone()).or_insert(0);
            *n += 1;
            if *n == 1 {
                id
            } else {
                format!("{}#{}", id, n)
            }
        })
        .collect()
}

/// `<processed-file>.signals`, named like the `.retry` sidecar (appended,
/// never an extension replacement) so independent jobs never share one.
pub fn ledger_path(processed_file: &Path) -> PathBuf {
    sidecar(processed_file, "signals")
}

/// `<processed-file>.render`: sessions holding a new signal that no written
/// digest has rendered yet. Same format as `.retry`.
pub fn render_path(processed_file: &Path) -> PathBuf {
    sidecar(processed_file, "render")
}

fn sidecar(processed_file: &Path, suffix: &str) -> PathBuf {
    let name = processed_file
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| "processed-sessions".to_string());
    processed_file.with_file_name(format!("{}.{}", name, suffix))
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Flags {
    counted: bool,
    filed: bool,
}

impl Flags {
    fn as_str(self) -> &'static str {
        match (self.counted, self.filed) {
            (true, true) => "cf",
            (true, false) => "c",
            (false, true) => "f",
            (false, false) => "",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "cf" => Some(Self { counted: true, filed: true }),
            "c" => Some(Self { counted: true, filed: false }),
            "f" => Some(Self { counted: false, filed: true }),
            _ => None,
        }
    }
}

/// The per-job signal ledger.
#[derive(Debug, Clone, Copy)]
struct Entry {
    /// The run date the ledger first saw the signal (filed or counted).
    date: NaiveDate,
    flags: Flags,
    /// The latest modified time seen for the signal's session; `None` for
    /// an entry written before the field existed.
    session_modified: Option<DateTime<Utc>>,
}

#[derive(Debug, Default)]
pub struct SignalLedger {
    entries: HashMap<String, Entry>,
}

impl SignalLedger {
    pub fn empty() -> Self {
        Self::default()
    }

    /// Load from `path`; a missing file is an empty ledger. Malformed lines
    /// are skipped with a warning: a corrupt line can at worst make one
    /// signal count again. An unreadable file (I/O error, invalid UTF-8)
    /// fails the run, as the processed and retry files do.
    pub fn load(path: &Path) -> Result<Self, JilogReviewError> {
        let mut ledger = Self::empty();
        if !path.exists() {
            return Ok(ledger);
        }
        let content = std::fs::read_to_string(path)?;
        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let fields: Vec<&str> = line.splitn(4, '\t').collect();
            // Four fields today; three (no session time) from the first
            // 0.9.0 builds.
            let (d, f, m, id) = match fields.as_slice() {
                [d, f, m, id] => (*d, *f, Some(*m), *id),
                [d, f, id] => (*d, *f, None, *id),
                _ => ("", "", None, ""),
            };
            let modified = match m {
                None | Some("-") => Ok(None),
                Some(t) => DateTime::parse_from_rfc3339(t).map(|t| Some(t.with_timezone(&Utc))),
            };
            let parsed = match (NaiveDate::parse_from_str(d, "%Y-%m-%d"), Flags::parse(f), modified) {
                (Ok(date), Some(flags), Ok(session_modified)) if !id.is_empty() => {
                    Some((id.to_string(), Entry { date, flags, session_modified }))
                }
                _ => None,
            };
            match parsed {
                Some((id, entry)) => {
                    ledger.entries.insert(id, entry);
                }
                None => tracing::warn!(
                    "signal ledger {}: skipping malformed line {:?}",
                    path.display(),
                    line
                ),
            }
        }
        Ok(ledger)
    }

    pub fn is_counted(&self, id: &str) -> bool {
        self.entries.get(id).is_some_and(|e| e.flags.counted)
    }

    pub fn is_filed(&self, id: &str) -> bool {
        self.entries.get(id).is_some_and(|e| e.flags.filed)
    }

    /// Record that an existing entry's session was seen with `modified`
    /// (the latest time wins). A no-op for an id the ledger does not hold.
    pub fn note_session(&mut self, id: &str, modified: DateTime<Utc>) {
        if let Some(e) = self.entries.get_mut(id) {
            if e.session_modified.is_none_or(|m| modified > m) {
                e.session_modified = Some(modified);
            }
        }
    }

    /// Per-kind counts of counted signals whose session was last seen
    /// modified at or after `since` — the window's earlier signals; this
    /// run's new ones are added by [`LedgerCounts::new`] (jilog#1xmf). The
    /// window is the review's own: sessions modified since `since`. An
    /// entry with no recorded session time is outside every window.
    pub fn counted_in_window(&self, since: DateTime<Utc>) -> KindCounts {
        let mut out = KindCounts::default();
        for (id, e) in &self.entries {
            if e.flags.counted && e.session_modified.is_some_and(|m| m >= since) {
                out.add(id.split(':').next().unwrap_or(""));
            }
        }
        out
    }

    pub fn mark_counted(&mut self, id: &str, date: NaiveDate, session_modified: Option<DateTime<Utc>>) {
        self.entry(id, date, session_modified).counted = true;
    }

    pub fn mark_filed(&mut self, id: &str, date: NaiveDate, session_modified: Option<DateTime<Utc>>) {
        self.entry(id, date, session_modified).filed = true;
    }

    fn entry(&mut self, id: &str, date: NaiveDate, session_modified: Option<DateTime<Utc>>) -> &mut Flags {
        let slot = self.entries.entry(id.to_string()).or_insert(Entry {
            date,
            flags: Flags::default(),
            session_modified: None,
        });
        if date < slot.date {
            slot.date = date;
        }
        if let Some(m) = session_modified {
            if slot.session_modified.is_none_or(|old| m > old) {
                slot.session_modified = Some(m);
            }
        }
        &mut slot.flags
    }

    /// Write atomically, dropping entries first seen more than
    /// [`LEDGER_RETENTION_DAYS`] before `run_date`.
    pub fn save(&self, path: &Path, run_date: NaiveDate) -> Result<(), JilogReviewError> {
        let floor = run_date - chrono::Duration::days(LEDGER_RETENTION_DAYS);
        let mut lines: Vec<String> = self
            .entries
            .iter()
            .filter(|(_, e)| e.date >= floor && (e.flags.counted || e.flags.filed))
            .map(|(id, e)| {
                let m = e
                    .session_modified
                    .map(|m| m.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
                    .unwrap_or_else(|| "-".to_string());
                format!("{}\t{}\t{}\t{}", e.date.format("%Y-%m-%d"), e.flags.as_str(), m, id)
            })
            .collect();
        lines.sort();
        let mut body = lines.join("\n");
        if !body.is_empty() {
            body.push('\n');
        }
        crate::reader::write_atomic(path, &body)
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }
}

/// Per-kind signal counts — the digest's `*_detected` header lines and the
/// `detected` object in `--json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct KindCounts {
    pub corrections: usize,
    pub errors: usize,
    pub workarounds: usize,
    pub deferrals: usize,
    pub patterns: usize,
}

impl KindCounts {
    pub fn total(&self) -> usize {
        self.corrections + self.errors + self.workarounds + self.deferrals + self.patterns
    }

    /// Count one signal of `kind` (a [`Signal::kind`] string).
    pub fn add(&mut self, kind: &str) {
        match kind {
            "correction" => self.corrections += 1,
            "error" => self.errors += 1,
            "workaround" => self.workarounds += 1,
            "deferral" => self.deferrals += 1,
            "pattern" => self.patterns += 1,
            _ => {}
        }
    }

    fn plus(&self, other: &KindCounts) -> KindCounts {
        KindCounts {
            corrections: self.corrections + other.corrections,
            errors: self.errors + other.errors,
            workarounds: self.workarounds + other.workarounds,
            deferrals: self.deferrals + other.deferrals,
            patterns: self.patterns + other.patterns,
        }
    }
}

/// The two ledger-derived header blocks (jilog#1xmf).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct LedgerCounts {
    /// Everything detected among the sessions this run scanned, new or
    /// already counted.
    pub detected: KindCounts,
    /// Every counted signal whose session was modified within the window,
    /// plus this run's new signals from sessions in the window: the window
    /// total, including sessions an earlier run already processed and this
    /// run skipped.
    pub in_window: KindCounts,
}

impl LedgerCounts {
    /// `in_window` = the ledger's counted entries whose session was seen
    /// modified at or after `since`, plus `new_in_window` (this run's new
    /// signals from sessions modified at or after `since`, not yet counted).
    pub fn new(ledger: &SignalLedger, since: DateTime<Utc>, detected: KindCounts, new_in_window: &KindCounts) -> Self {
        Self { detected, in_window: ledger.counted_in_window(since).plus(new_in_window) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signal::{Correction, ErrorSignal};

    fn corr(session: &str, text: &str) -> Signal {
        Signal::Correction(Correction {
            session_id: session.into(),
            context: text.into(),
            ..Default::default()
        })
    }

    fn err(session: &str, tool: &str, msg: &str) -> Signal {
        Signal::Error(ErrorSignal {
            session_id: session.into(),
            tool_name: tool.into(),
            message: msg.into(),
            ..Default::default()
        })
    }

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn identity_is_stable_and_scoped() {
        let a = signal_identity(&corr("s1", "no, wrong file"));
        assert_eq!(a, signal_identity(&corr("s1", "no, wrong file")));
        assert!(a.starts_with("correction:s1:"));
        assert_eq!(a.len(), "correction:s1:".len() + 16);
        assert_ne!(a, signal_identity(&corr("s2", "no, wrong file")));
        assert_ne!(a, signal_identity(&corr("s1", "no, wrong dir")));
        // Pinned value: FNV-1a must not drift across Rust versions.
        assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
    }

    fn pattern(session: &str, kind: &str, description: &str) -> Signal {
        Signal::Pattern(crate::signal::PatternSignal {
            session_id: session.into(),
            pattern_kind: kind.into(),
            description: description.into(),
            ..Default::default()
        })
    }

    #[test]
    fn pattern_identity_survives_growing_counts() {
        // jilog#1xmf chunk B pass 3: descriptions carry counts that grow as
        // the session runs; a rescan must not make the pattern new.
        assert_eq!(
            signal_identity(&pattern("s1", "compaction_storm", "compaction storm: 3 compactions within 7 minutes")),
            signal_identity(&pattern("s1", "compaction_storm", "compaction storm: 4 compactions within 9 minutes"))
        );
        assert_eq!(
            signal_identity(&pattern("s1", "stuck_loop", "stuck loop: `bash` called 4 times with identical arguments")),
            signal_identity(&pattern("s1", "stuck_loop", "stuck loop: `bash` called 6 times with identical arguments"))
        );
        assert_ne!(
            signal_identity(&pattern("s1", "stuck_loop", "stuck loop: `bash` called 4 times")),
            signal_identity(&pattern("s1", "stuck_loop", "stuck loop: `grep` called 4 times"))
        );
        assert_ne!(
            signal_identity(&pattern("s1", "compaction_storm", "x")),
            signal_identity(&pattern("s1", "resume_storm", "x"))
        );
        // Two loops on one tool in one session stay two signals.
        let two = vec![
            pattern("s1", "stuck_loop", "stuck loop: `bash` called 4 times"),
            pattern("s1", "stuck_loop", "stuck loop: `bash` called 5 times"),
        ];
        let ids = assign_ids(&two);
        assert_eq!(ids[1], format!("{}#2", ids[0]));
    }

    #[test]
    fn identity_fields_are_length_prefixed() {
        assert_ne!(
            signal_identity(&err("s1", "ab", "c")),
            signal_identity(&err("s1", "a", "bc"))
        );
    }

    #[test]
    fn repeats_get_suffixes() {
        let sigs = vec![corr("s1", "no, again"), corr("s1", "no, again"), corr("s1", "no, again")];
        let ids = assign_ids(&sigs);
        assert_eq!(ids[1], format!("{}#2", ids[0]));
        assert_eq!(ids[2], format!("{}#3", ids[0]));
    }

    #[test]
    fn round_trip_keeps_both_flags() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.txt.signals");
        let mut l = SignalLedger::empty();
        l.mark_counted("a", d("2026-10-10"), None);
        l.mark_filed("b", d("2026-10-11"), None);
        l.mark_counted("c", d("2026-10-12"), None);
        l.mark_filed("c", d("2026-10-12"), None);
        l.save(&path, d("2026-10-13")).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert_eq!(body, "2026-10-10\tc\t-\ta\n2026-10-11\tf\t-\tb\n2026-10-12\tcf\t-\tc\n");
        let back = SignalLedger::load(&path).unwrap();
        assert!(back.is_counted("a") && !back.is_filed("a"));
        assert!(!back.is_counted("b") && back.is_filed("b"));
        assert!(back.is_counted("c") && back.is_filed("c"));
        assert!(!back.is_counted("zzz"));
    }

    #[test]
    fn first_seen_date_keeps_earliest() {
        let mut l = SignalLedger::empty();
        l.mark_filed("a", d("2026-10-12"), None);
        l.mark_counted("a", d("2026-10-10"), None);
        l.mark_filed("a", d("2026-10-13"), None);
        assert_eq!(l.entries["a"].date, d("2026-10-10"));
    }

    #[test]
    fn retention_drops_entries_past_90_days() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.signals");
        let run = d("2026-10-13");
        let mut l = SignalLedger::empty();
        l.mark_counted("keep", run - chrono::Duration::days(90), None);
        l.mark_counted("drop", run - chrono::Duration::days(91), None);
        l.save(&path, run).unwrap();
        let back = SignalLedger::load(&path).unwrap();
        assert!(back.is_counted("keep"));
        assert!(!back.is_counted("drop"));
        assert_eq!(back.len(), 1);
    }

    #[test]
    fn malformed_lines_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.signals");
        std::fs::write(
            &path,
            "garbage\n2026-10-12\tx\tid1\nnot-a-date\tc\tid2\n2026-10-12\tc\t\n2026-10-12\tc\tgood\n",
        )
        .unwrap();
        let l = SignalLedger::load(&path).unwrap();
        assert_eq!(l.len(), 1);
        assert!(l.is_counted("good"));
    }

    #[test]
    fn missing_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let l = SignalLedger::load(&dir.path().join("none")).unwrap();
        assert_eq!(l.len(), 0);
    }

    fn t(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn window_counts_by_session_modified_time() {
        let mut l = SignalLedger::empty();
        l.mark_counted("correction:s1:aaaa", d("2026-10-12"), Some(t("2026-10-11T10:00:00Z")));
        l.mark_counted("correction:s2:bbbb", d("2026-10-12"), Some(t("2026-10-12T10:00:00Z")));
        l.mark_counted("error:s2:cccc", d("2026-10-12"), Some(t("2026-10-12T10:00:00Z")));
        l.mark_filed("correction:s3:dddd", d("2026-10-12"), Some(t("2026-10-12T10:00:00Z"))); // never counted
        l.mark_counted("correction:s4:eeee", d("2026-10-12"), None); // no session time
        let since = t("2026-10-12T00:00:00Z");
        assert_eq!(l.counted_in_window(since), KindCounts { corrections: 1, errors: 1, ..Default::default() });
        // A rescan that sees s1 modified later brings it into the window.
        l.note_session("correction:s1:aaaa", t("2026-10-12T20:00:00Z"));
        l.note_session("correction:zzz:none", t("2026-10-12T20:00:00Z")); // absent: no-op
        assert_eq!(l.counted_in_window(since).corrections, 2);
        let new = KindCounts { corrections: 2, ..Default::default() };
        let lc = LedgerCounts::new(&l, since, KindCounts::default(), &new);
        assert_eq!(lc.in_window.corrections, 4);
        assert_eq!(lc.in_window.total(), 5);
    }

    #[test]
    fn session_time_round_trips_and_old_lines_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.signals");
        let mut l = SignalLedger::empty();
        l.mark_counted("a", d("2026-10-12"), Some(t("2026-10-12T10:00:00Z")));
        l.save(&path, d("2026-10-13")).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "2026-10-12\tc\t2026-10-12T10:00:00Z\ta\n");
        let back = SignalLedger::load(&path).unwrap();
        assert_eq!(back.counted_in_window(t("2026-10-12T09:00:00Z")).total(), 0, "unknown kind 'a' counts nowhere");
        assert_eq!(back.entries["a"].session_modified, Some(t("2026-10-12T10:00:00Z")));
        // A three-field line from the first 0.9.0 builds still loads.
        std::fs::write(&path, "2026-10-12\tcf\tcorrection:s1:aaaa\n").unwrap();
        let old = SignalLedger::load(&path).unwrap();
        assert!(old.is_counted("correction:s1:aaaa") && old.is_filed("correction:s1:aaaa"));
        assert_eq!(old.entries["correction:s1:aaaa"].session_modified, None);
    }

    #[test]
    fn sidecar_paths_append() {
        let p = Path::new("/x/processed-sessions.txt");
        assert_eq!(ledger_path(p), Path::new("/x/processed-sessions.txt.signals"));
        assert_eq!(render_path(p), Path::new("/x/processed-sessions.txt.render"));
    }
}
