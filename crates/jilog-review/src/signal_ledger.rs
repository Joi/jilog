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
//! File format: one line per signal, `<first-seen date>\t<c|f|cf>\t<id>`,
//! next to the processed file as `<processed-file>.signals`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::NaiveDate;
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
#[derive(Debug, Default)]
pub struct SignalLedger {
    entries: HashMap<String, (NaiveDate, Flags)>,
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
            let mut parts = line.splitn(3, '\t');
            let parsed = match (parts.next(), parts.next(), parts.next()) {
                (Some(d), Some(f), Some(id)) if !id.is_empty() => {
                    match (NaiveDate::parse_from_str(d, "%Y-%m-%d"), Flags::parse(f)) {
                        (Ok(date), Some(flags)) => Some((id.to_string(), date, flags)),
                        _ => None,
                    }
                }
                _ => None,
            };
            match parsed {
                Some((id, date, flags)) => {
                    ledger.entries.insert(id, (date, flags));
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
        self.entries.get(id).is_some_and(|(_, f)| f.counted)
    }

    pub fn is_filed(&self, id: &str) -> bool {
        self.entries.get(id).is_some_and(|(_, f)| f.filed)
    }

    /// Per-kind counts of signals first counted on a run dated `from` or
    /// later — the window total, together with this run's new signals
    /// (jilog#1xmf). The kind is the identity's prefix.
    pub fn counted_since(&self, from: NaiveDate) -> KindCounts {
        let mut out = KindCounts::default();
        for (id, (date, flags)) in &self.entries {
            if flags.counted && *date >= from {
                out.add(id.split(':').next().unwrap_or(""));
            }
        }
        out
    }

    pub fn mark_counted(&mut self, id: &str, date: NaiveDate) {
        self.entry(id, date).counted = true;
    }

    pub fn mark_filed(&mut self, id: &str, date: NaiveDate) {
        self.entry(id, date).filed = true;
    }

    fn entry(&mut self, id: &str, date: NaiveDate) -> &mut Flags {
        let slot = self
            .entries
            .entry(id.to_string())
            .or_insert((date, Flags::default()));
        if date < slot.0 {
            slot.0 = date;
        }
        &mut slot.1
    }

    /// Write atomically, dropping entries first seen more than
    /// [`LEDGER_RETENTION_DAYS`] before `run_date`.
    pub fn save(&self, path: &Path, run_date: NaiveDate) -> Result<(), JilogReviewError> {
        let floor = run_date - chrono::Duration::days(LEDGER_RETENTION_DAYS);
        let mut lines: Vec<String> = self
            .entries
            .iter()
            .filter(|(_, (date, flags))| *date >= floor && (flags.counted || flags.filed))
            .map(|(id, (date, flags))| format!("{}\t{}\t{}", date.format("%Y-%m-%d"), flags.as_str(), id))
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
    /// Every signal first counted on a run dated within the window, plus
    /// this run's new signals: the window total, including sessions an
    /// earlier run already processed and this run skipped.
    pub in_window: KindCounts,
}

impl LedgerCounts {
    /// `in_window` = the ledger's counted entries dated `from` or later,
    /// plus `new` (this run's signals, not yet in the ledger as counted).
    pub fn new(ledger: &SignalLedger, from: NaiveDate, detected: KindCounts, new: &KindCounts) -> Self {
        Self { detected, in_window: ledger.counted_since(from).plus(new) }
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
        l.mark_counted("a", d("2026-10-10"));
        l.mark_filed("b", d("2026-10-11"));
        l.mark_counted("c", d("2026-10-12"));
        l.mark_filed("c", d("2026-10-12"));
        l.save(&path, d("2026-10-13")).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert_eq!(body, "2026-10-10\tc\ta\n2026-10-11\tf\tb\n2026-10-12\tcf\tc\n");
        let back = SignalLedger::load(&path).unwrap();
        assert!(back.is_counted("a") && !back.is_filed("a"));
        assert!(!back.is_counted("b") && back.is_filed("b"));
        assert!(back.is_counted("c") && back.is_filed("c"));
        assert!(!back.is_counted("zzz"));
    }

    #[test]
    fn first_seen_date_keeps_earliest() {
        let mut l = SignalLedger::empty();
        l.mark_filed("a", d("2026-10-12"));
        l.mark_counted("a", d("2026-10-10"));
        l.mark_filed("a", d("2026-10-13"));
        assert_eq!(l.entries["a"].0, d("2026-10-10"));
    }

    #[test]
    fn retention_drops_entries_past_90_days() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.signals");
        let run = d("2026-10-13");
        let mut l = SignalLedger::empty();
        l.mark_counted("keep", run - chrono::Duration::days(90));
        l.mark_counted("drop", run - chrono::Duration::days(91));
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

    #[test]
    fn counted_since_counts_window_by_kind() {
        let mut l = SignalLedger::empty();
        l.mark_counted("correction:s1:aaaa", d("2026-10-10"));
        l.mark_counted("correction:s2:bbbb", d("2026-10-12"));
        l.mark_counted("error:s2:cccc", d("2026-10-12"));
        l.mark_filed("correction:s3:dddd", d("2026-10-12")); // filed, never counted
        let w = l.counted_since(d("2026-10-11"));
        assert_eq!(w, KindCounts { corrections: 1, errors: 1, ..Default::default() });
        let new = KindCounts { corrections: 2, ..Default::default() };
        let lc = LedgerCounts::new(&l, d("2026-10-11"), KindCounts::default(), &new);
        assert_eq!(lc.in_window.corrections, 3);
        assert_eq!(lc.in_window.total(), 4);
    }

    #[test]
    fn sidecar_paths_append() {
        let p = Path::new("/x/processed-sessions.txt");
        assert_eq!(ledger_path(p), Path::new("/x/processed-sessions.txt.signals"));
        assert_eq!(render_path(p), Path::new("/x/processed-sessions.txt.render"));
    }
}
