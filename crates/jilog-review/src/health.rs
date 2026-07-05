//! Health-pattern detectors — mechanical session-health signals over
//! [`SessionEvent`] streams.
//!
//! Four pure-Rust detectors, each emitting [`PatternSignal`] with a stable
//! snake_case `pattern_kind`. They run only for readers that implement
//! [`crate::reader::Reader::load_events`]; message-only readers produce no
//! health signals.
//!
//! Thresholds follow the amplifier context-intelligence signals-reference,
//! tuned conservatively. All constants live here so the digest, README, and
//! tests reference one place.
//!
//! | pattern_kind        | fires when |
//! |---------------------|------------|
//! | `compaction_storm`  | ≥3 compaction events within a 10-minute window |
//! | `stuck_loop`        | same tool called with identical arguments ≥4 times consecutively |
//! | `resume_storm`      | ≥3 resumes of one session within 30 minutes |
//! | `iteration_runaway` | ≥25 tool calls with no intervening user message |

use chrono::{DateTime, Duration, Utc};

use crate::reader::{SessionEvent, SessionEventKind};
use crate::signal::PatternSignal;

// ---------------------------------------------------------------------------
// Thresholds
// ---------------------------------------------------------------------------

/// `compaction_storm`: minimum compaction events inside the window.
pub const COMPACTION_STORM_MIN_EVENTS: usize = 3;
/// `compaction_storm`: window size in minutes (inclusive span).
pub const COMPACTION_STORM_WINDOW_MINUTES: i64 = 10;

/// `stuck_loop`: minimum consecutive identical tool calls.
pub const STUCK_LOOP_MIN_REPEATS: usize = 4;

/// `resume_storm`: minimum resume events inside the window.
pub const RESUME_STORM_MIN_EVENTS: usize = 3;
/// `resume_storm`: window size in minutes (inclusive span).
pub const RESUME_STORM_WINDOW_MINUTES: i64 = 30;

/// `iteration_runaway`: minimum tool calls with no intervening user message.
pub const ITERATION_RUNAWAY_MIN_TOOL_CALLS: usize = 25;

// ---------------------------------------------------------------------------
// Aggregator
// ---------------------------------------------------------------------------

/// Run all four health detectors over one session's event stream.
///
/// Events are assumed to be in file (chronological) order; the sequence
/// detectors (`stuck_loop`, `iteration_runaway`) depend on that order, the
/// window detectors sort timestamps internally.
pub fn detect_health_patterns(events: &[SessionEvent], session_id: &str) -> Vec<PatternSignal> {
    let mut out = Vec::new();
    out.extend(detect_compaction_storm(events, session_id));
    out.extend(detect_stuck_loops(events, session_id));
    out.extend(detect_resume_storm(events, session_id));
    out.extend(detect_iteration_runaway(events, session_id));
    out
}

// ---------------------------------------------------------------------------
// compaction_storm / resume_storm — windowed clusters
// ---------------------------------------------------------------------------

/// Fire when >= [`COMPACTION_STORM_MIN_EVENTS`] compactions land within a
/// [`COMPACTION_STORM_WINDOW_MINUTES`]-minute window. At most one signal per
/// session, describing the densest cluster.
pub fn detect_compaction_storm(
    events: &[SessionEvent],
    session_id: &str,
) -> Option<PatternSignal> {
    let times = times_of_kind(events, SessionEventKind::Compaction);
    let (count, start, end) = densest_window(
        &times,
        Duration::minutes(COMPACTION_STORM_WINDOW_MINUTES),
        COMPACTION_STORM_MIN_EVENTS,
    )?;
    Some(PatternSignal {
        session_id: session_id.to_string(),
        description: format!(
            "compaction storm: {} compactions within {} minutes",
            count, COMPACTION_STORM_WINDOW_MINUTES
        ),
        pattern_kind: "compaction_storm".to_string(),
        evidence: format!("{} compactions {}", count, format_range(start, end)),
    })
}

/// Fire when >= [`RESUME_STORM_MIN_EVENTS`] resumes land within a
/// [`RESUME_STORM_WINDOW_MINUTES`]-minute window. At most one signal per
/// session, describing the densest cluster.
pub fn detect_resume_storm(events: &[SessionEvent], session_id: &str) -> Option<PatternSignal> {
    let times = times_of_kind(events, SessionEventKind::Resume);
    let (count, start, end) = densest_window(
        &times,
        Duration::minutes(RESUME_STORM_WINDOW_MINUTES),
        RESUME_STORM_MIN_EVENTS,
    )?;
    Some(PatternSignal {
        session_id: session_id.to_string(),
        description: format!(
            "resume storm: {} resumes within {} minutes",
            count, RESUME_STORM_WINDOW_MINUTES
        ),
        pattern_kind: "resume_storm".to_string(),
        evidence: format!("{} resumes {}", count, format_range(start, end)),
    })
}

/// Timestamps of all events of `kind`, sorted ascending.
fn times_of_kind(events: &[SessionEvent], kind: SessionEventKind) -> Vec<DateTime<Utc>> {
    let mut times: Vec<DateTime<Utc>> = events
        .iter()
        .filter(|e| e.kind == kind)
        .map(|e| e.timestamp)
        .collect();
    times.sort();
    times
}

/// Find the densest cluster of `times` whose span fits inside `window`
/// (inclusive: a span of exactly `window` still counts). Returns
/// `(count, first, last)` for the largest such cluster with
/// `count >= min_events`, or None.
fn densest_window(
    times: &[DateTime<Utc>],
    window: Duration,
    min_events: usize,
) -> Option<(usize, DateTime<Utc>, DateTime<Utc>)> {
    let mut best: Option<(usize, DateTime<Utc>, DateTime<Utc>)> = None;
    let mut lo = 0;
    for hi in 0..times.len() {
        while times[hi] - times[lo] > window {
            lo += 1;
        }
        let count = hi - lo + 1;
        if count >= min_events && best.map(|(c, _, _)| count > c).unwrap_or(true) {
            best = Some((count, times[lo], times[hi]));
        }
    }
    best
}

/// "09:01-09:08" (UTC, minute precision).
fn format_range(start: DateTime<Utc>, end: DateTime<Utc>) -> String {
    format!("{}-{}", start.format("%H:%M"), end.format("%H:%M"))
}

// ---------------------------------------------------------------------------
// stuck_loop — consecutive identical tool calls
// ---------------------------------------------------------------------------

/// Fire once per maximal run of >= [`STUCK_LOOP_MIN_REPEATS`] consecutive
/// calls to the same tool with identical arguments. "Consecutive" is over
/// the `ToolCall` subsequence: interleaved LLM responses do not break a run
/// (the agent retrying after each response is exactly the stuck shape).
pub fn detect_stuck_loops(events: &[SessionEvent], session_id: &str) -> Vec<PatternSignal> {
    let calls: Vec<&SessionEvent> = events
        .iter()
        .filter(|e| e.kind == SessionEventKind::ToolCall)
        .collect();

    let mut out = Vec::new();
    let mut run_start = 0;
    for i in 1..=calls.len() {
        let same = i < calls.len()
            && calls[i].tool_name == calls[run_start].tool_name
            && calls[i].detail == calls[run_start].detail;
        if same {
            continue;
        }
        let run_len = i - run_start;
        if run_len >= STUCK_LOOP_MIN_REPEATS {
            let tool = calls[run_start]
                .tool_name
                .as_deref()
                .unwrap_or("unknown")
                .to_string();
            out.push(PatternSignal {
                session_id: session_id.to_string(),
                description: format!(
                    "stuck loop: `{}` called {} times with identical arguments",
                    tool, run_len
                ),
                pattern_kind: "stuck_loop".to_string(),
                evidence: format!(
                    "`{}` x{} identical arguments {}",
                    tool,
                    run_len,
                    format_range(calls[run_start].timestamp, calls[i - 1].timestamp)
                ),
            });
        }
        run_start = i;
    }
    out
}

// ---------------------------------------------------------------------------
// iteration_runaway — tool calls with no intervening user message
// ---------------------------------------------------------------------------

/// Fire when >= [`ITERATION_RUNAWAY_MIN_TOOL_CALLS`] tool calls happen with
/// no intervening user message. At most one signal per session, describing
/// the longest stretch.
pub fn detect_iteration_runaway(
    events: &[SessionEvent],
    session_id: &str,
) -> Option<PatternSignal> {
    let mut best: Option<(usize, DateTime<Utc>, DateTime<Utc>)> = None;
    let mut count = 0;
    let mut stretch_start: Option<DateTime<Utc>> = None;
    let mut stretch_end: Option<DateTime<Utc>> = None;

    let flush = |count: usize,
                     start: Option<DateTime<Utc>>,
                     end: Option<DateTime<Utc>>,
                     best: &mut Option<(usize, DateTime<Utc>, DateTime<Utc>)>| {
        if count >= ITERATION_RUNAWAY_MIN_TOOL_CALLS {
            if let (Some(s), Some(e)) = (start, end) {
                if best.map(|(c, _, _)| count > c).unwrap_or(true) {
                    *best = Some((count, s, e));
                }
            }
        }
    };

    for event in events {
        match event.kind {
            SessionEventKind::UserMessage => {
                flush(count, stretch_start, stretch_end, &mut best);
                count = 0;
                stretch_start = None;
                stretch_end = None;
            }
            SessionEventKind::ToolCall => {
                count += 1;
                stretch_start.get_or_insert(event.timestamp);
                stretch_end = Some(event.timestamp);
            }
            _ => {}
        }
    }
    flush(count, stretch_start, stretch_end, &mut best);

    let (count, start, end) = best?;
    Some(PatternSignal {
        session_id: session_id.to_string(),
        description: format!(
            "iteration runaway: {} tool calls with no intervening user message",
            count
        ),
        pattern_kind: "iteration_runaway".to_string(),
        evidence: format!(
            "{} tool calls without a user message {}",
            count,
            format_range(start, end)
        ),
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    /// Event at `minute_offset` minutes (and `sec` seconds) past 2026-01-01 09:00 UTC.
    fn at(kind: SessionEventKind, minute_offset: i64, sec: i64) -> SessionEvent {
        SessionEvent {
            kind,
            timestamp: Utc.with_ymd_and_hms(2026, 1, 1, 9, 0, 0).unwrap()
                + Duration::minutes(minute_offset)
                + Duration::seconds(sec),
            tool_name: None,
            detail: None,
        }
    }

    fn tool_call(name: &str, args: &str, minute_offset: i64) -> SessionEvent {
        SessionEvent {
            tool_name: Some(name.to_string()),
            detail: Some(args.to_string()),
            ..at(SessionEventKind::ToolCall, minute_offset, 0)
        }
    }

    // ---------- compaction_storm ----------

    #[test]
    fn compaction_storm_fires_at_threshold() {
        let events = vec![
            at(SessionEventKind::Compaction, 1, 0),
            at(SessionEventKind::Compaction, 4, 0),
            at(SessionEventKind::Compaction, 8, 0),
        ];
        let sig = detect_compaction_storm(&events, "s1").expect("must fire at 3-in-10m");
        assert_eq!(sig.pattern_kind, "compaction_storm");
        assert_eq!(sig.session_id, "s1");
        assert_eq!(sig.evidence, "3 compactions 09:01-09:08");
        assert!(sig.description.contains("3 compactions"));
    }

    #[test]
    fn compaction_storm_silent_below_threshold() {
        let events = vec![
            at(SessionEventKind::Compaction, 1, 0),
            at(SessionEventKind::Compaction, 8, 0),
        ];
        assert!(detect_compaction_storm(&events, "s1").is_none());
    }

    #[test]
    fn compaction_storm_window_edges() {
        // Span of exactly 10 minutes fires (inclusive window)...
        let exact = vec![
            at(SessionEventKind::Compaction, 0, 0),
            at(SessionEventKind::Compaction, 5, 0),
            at(SessionEventKind::Compaction, 10, 0),
        ];
        assert!(detect_compaction_storm(&exact, "s1").is_some());

        // ...one second past 10 minutes does not.
        let past = vec![
            at(SessionEventKind::Compaction, 0, 0),
            at(SessionEventKind::Compaction, 5, 0),
            at(SessionEventKind::Compaction, 10, 1),
        ];
        assert!(detect_compaction_storm(&past, "s1").is_none());
    }

    #[test]
    fn compaction_storm_spread_out_events_silent() {
        // 4 compactions over 45 minutes, never 3 within any 10-minute window.
        let events = vec![
            at(SessionEventKind::Compaction, 0, 0),
            at(SessionEventKind::Compaction, 15, 0),
            at(SessionEventKind::Compaction, 30, 0),
            at(SessionEventKind::Compaction, 45, 0),
        ];
        assert!(detect_compaction_storm(&events, "s1").is_none());
    }

    #[test]
    fn compaction_storm_reports_densest_cluster() {
        // A 3-cluster early and a 4-cluster later: evidence must cite the 4.
        let events = vec![
            at(SessionEventKind::Compaction, 0, 0),
            at(SessionEventKind::Compaction, 2, 0),
            at(SessionEventKind::Compaction, 4, 0),
            at(SessionEventKind::Compaction, 30, 0),
            at(SessionEventKind::Compaction, 32, 0),
            at(SessionEventKind::Compaction, 34, 0),
            at(SessionEventKind::Compaction, 36, 0),
        ];
        let sig = detect_compaction_storm(&events, "s1").unwrap();
        assert_eq!(sig.evidence, "4 compactions 09:30-09:36");
    }

    // ---------- resume_storm ----------

    #[test]
    fn resume_storm_fires_at_threshold() {
        let events = vec![
            at(SessionEventKind::Resume, 0, 0),
            at(SessionEventKind::Resume, 12, 0),
            at(SessionEventKind::Resume, 25, 0),
        ];
        let sig = detect_resume_storm(&events, "s1").expect("must fire at 3-in-30m");
        assert_eq!(sig.pattern_kind, "resume_storm");
        assert_eq!(sig.evidence, "3 resumes 09:00-09:25");
    }

    #[test]
    fn resume_storm_silent_below_threshold() {
        let events = vec![
            at(SessionEventKind::Resume, 0, 0),
            at(SessionEventKind::Resume, 12, 0),
        ];
        assert!(detect_resume_storm(&events, "s1").is_none());
    }

    #[test]
    fn resume_storm_window_edges() {
        let exact = vec![
            at(SessionEventKind::Resume, 0, 0),
            at(SessionEventKind::Resume, 15, 0),
            at(SessionEventKind::Resume, 30, 0),
        ];
        assert!(detect_resume_storm(&exact, "s1").is_some());

        let past = vec![
            at(SessionEventKind::Resume, 0, 0),
            at(SessionEventKind::Resume, 15, 0),
            at(SessionEventKind::Resume, 30, 1),
        ];
        assert!(detect_resume_storm(&past, "s1").is_none());
    }

    #[test]
    fn storms_ignore_other_event_kinds() {
        // Compactions must not count toward a resume storm and vice versa.
        let events = vec![
            at(SessionEventKind::Compaction, 0, 0),
            at(SessionEventKind::Compaction, 1, 0),
            at(SessionEventKind::Resume, 2, 0),
        ];
        assert!(detect_compaction_storm(&events, "s1").is_none());
        assert!(detect_resume_storm(&events, "s1").is_none());
    }

    // ---------- stuck_loop ----------

    #[test]
    fn stuck_loop_fires_at_threshold() {
        let events = vec![
            tool_call("bash", r#"{"command":"cargo test"}"#, 0),
            tool_call("bash", r#"{"command":"cargo test"}"#, 1),
            tool_call("bash", r#"{"command":"cargo test"}"#, 2),
            tool_call("bash", r#"{"command":"cargo test"}"#, 3),
        ];
        let sigs = detect_stuck_loops(&events, "s1");
        assert_eq!(sigs.len(), 1);
        assert_eq!(sigs[0].pattern_kind, "stuck_loop");
        assert_eq!(sigs[0].evidence, "`bash` x4 identical arguments 09:00-09:03");
    }

    #[test]
    fn stuck_loop_silent_below_threshold() {
        let events = vec![
            tool_call("bash", "{}", 0),
            tool_call("bash", "{}", 1),
            tool_call("bash", "{}", 2),
        ];
        assert!(detect_stuck_loops(&events, "s1").is_empty());
    }

    #[test]
    fn stuck_loop_different_arguments_break_run() {
        let events = vec![
            tool_call("bash", r#"{"command":"a"}"#, 0),
            tool_call("bash", r#"{"command":"a"}"#, 1),
            tool_call("bash", r#"{"command":"b"}"#, 2),
            tool_call("bash", r#"{"command":"a"}"#, 3),
            tool_call("bash", r#"{"command":"a"}"#, 4),
        ];
        assert!(detect_stuck_loops(&events, "s1").is_empty());
    }

    #[test]
    fn stuck_loop_different_tool_breaks_run() {
        let events = vec![
            tool_call("bash", "{}", 0),
            tool_call("bash", "{}", 1),
            tool_call("read_file", "{}", 2),
            tool_call("bash", "{}", 3),
            tool_call("bash", "{}", 4),
        ];
        assert!(detect_stuck_loops(&events, "s1").is_empty());
    }

    #[test]
    fn stuck_loop_survives_interleaved_llm_responses() {
        // Call → response → same call, four times: still a stuck loop.
        let mut events = Vec::new();
        for i in 0..4 {
            events.push(tool_call("bash", "{}", i * 2));
            events.push(at(SessionEventKind::LlmResponse, i * 2 + 1, 0));
        }
        let sigs = detect_stuck_loops(&events, "s1");
        assert_eq!(sigs.len(), 1);
        assert!(sigs[0].description.contains("4 times"));
    }

    #[test]
    fn stuck_loop_two_separate_runs_two_signals() {
        let mut events = Vec::new();
        for i in 0..4 {
            events.push(tool_call("bash", r#"{"c":"x"}"#, i));
        }
        for i in 0..5 {
            events.push(tool_call("grep", r#"{"q":"y"}"#, 10 + i));
        }
        let sigs = detect_stuck_loops(&events, "s1");
        assert_eq!(sigs.len(), 2);
        assert!(sigs[0].evidence.contains("`bash` x4"));
        assert!(sigs[1].evidence.contains("`grep` x5"));
    }

    // ---------- iteration_runaway ----------

    fn n_tool_calls(n: usize, start_minute: i64) -> Vec<SessionEvent> {
        (0..n)
            .map(|i| tool_call("bash", &format!(r#"{{"step":{}}}"#, i), start_minute + i as i64))
            .collect()
    }

    #[test]
    fn iteration_runaway_fires_at_threshold() {
        let events = n_tool_calls(25, 0);
        let sig = detect_iteration_runaway(&events, "s1").expect("must fire at 25");
        assert_eq!(sig.pattern_kind, "iteration_runaway");
        assert_eq!(sig.evidence, "25 tool calls without a user message 09:00-09:24");
    }

    #[test]
    fn iteration_runaway_silent_below_threshold() {
        let events = n_tool_calls(24, 0);
        assert!(detect_iteration_runaway(&events, "s1").is_none());
    }

    #[test]
    fn iteration_runaway_user_message_resets_count() {
        // 24 calls, a user message, 24 more: never 25 uninterrupted.
        let mut events = n_tool_calls(24, 0);
        events.push(at(SessionEventKind::UserMessage, 24, 0));
        events.extend(n_tool_calls(24, 25));
        assert!(detect_iteration_runaway(&events, "s1").is_none());
    }

    #[test]
    fn iteration_runaway_llm_responses_do_not_reset() {
        let mut events = Vec::new();
        for i in 0..25 {
            events.push(tool_call("bash", &format!(r#"{{"i":{}}}"#, i), i as i64));
            events.push(at(SessionEventKind::LlmResponse, i as i64, 30));
        }
        assert!(detect_iteration_runaway(&events, "s1").is_some());
    }

    #[test]
    fn iteration_runaway_reports_longest_stretch() {
        // 26-call stretch, user message, 30-call stretch: report the 30.
        let mut events = n_tool_calls(26, 0);
        events.push(at(SessionEventKind::UserMessage, 26, 0));
        events.extend(n_tool_calls(30, 27));
        let sig = detect_iteration_runaway(&events, "s1").unwrap();
        assert!(sig.evidence.starts_with("30 tool calls"), "evidence: {}", sig.evidence);
    }

    // ---------- aggregator ----------

    #[test]
    fn detect_health_patterns_combines_all_kinds() {
        let mut events = Vec::new();
        events.extend(vec![
            at(SessionEventKind::Compaction, 0, 0),
            at(SessionEventKind::Compaction, 1, 0),
            at(SessionEventKind::Compaction, 2, 0),
        ]);
        for i in 0..4 {
            events.push(tool_call("bash", "{}", 3 + i));
        }
        let sigs = detect_health_patterns(&events, "s1");
        let kinds: Vec<&str> = sigs.iter().map(|s| s.pattern_kind.as_str()).collect();
        assert!(kinds.contains(&"compaction_storm"));
        assert!(kinds.contains(&"stuck_loop"));
        assert!(!kinds.contains(&"resume_storm"));
        assert!(!kinds.contains(&"iteration_runaway"));
        for s in &sigs {
            assert_eq!(s.session_id, "s1");
            assert!(!s.description.is_empty());
            assert!(!s.evidence.is_empty());
        }
    }

    #[test]
    fn detect_health_patterns_empty_events() {
        assert!(detect_health_patterns(&[], "s1").is_empty());
    }
}
