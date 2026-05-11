//! `jilog query` — filter and display ledger events.
//!
//! Ported from opsctl/crates/opsctl/src/query.rs with opsctl::Config replaced
//! by a --zone (from JilogConfig) or direct --ledger-path flag.

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};

use ledger_core::{Event, EventClass};
use ledger_sqlite::LedgerDb;

use crate::config::JilogConfig;
use jilog_review::util::expand_tilde;

// ---------------------------------------------------------------------------
// CLI types
// ---------------------------------------------------------------------------

#[derive(clap::Args, Debug)]
pub struct QueryArgs {
    /// Time window (e.g., "7d", "4w", "2026-04-01"). Default: 7d.
    #[arg(long, default_value = "7d")]
    pub since: String,

    /// Filter by subsystem name (may repeat, OR logic). Supports trailing-* glob.
    #[arg(long)]
    pub subsystem: Vec<String>,

    /// Filter by event class (ingest, route, decision, state-change, ...).
    #[arg(long)]
    pub class: Option<String>,

    /// Zone id from config (conflicts with --ledger-path).
    #[arg(long, conflicts_with = "ledger_path")]
    pub zone: Option<String>,

    /// Direct path to ledger directory (conflicts with --zone).
    #[arg(long)]
    pub ledger_path: Option<std::path::PathBuf>,

    /// Max events to return per zone after filtering.
    #[arg(long, default_value_t = 100)]
    pub limit: u32,

    /// Output format: text (default) or json.
    #[arg(long, default_value = "text")]
    pub format: String,
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

pub fn run(cfg: &JilogConfig, args: &QueryArgs) -> anyhow::Result<()> {
    let cutoff = parse_since(&args.since)
        .with_context(|| format!("invalid --since value: {}", args.since))?;
    let class_filter = args
        .class
        .as_deref()
        .map(parse_class)
        .transpose()
        .with_context(|| format!("invalid --class value: {:?}", args.class))?;

    // Determine which ledger paths to query.
    let ledger_paths: Vec<(String, std::path::PathBuf)> = if let Some(ref direct) = args.ledger_path {
        vec![("direct".to_string(), direct.clone())]
    } else {
        let zones: Vec<&crate::config::ZoneConfig> = match &args.zone {
            Some(z) => cfg.zones.iter().filter(|zc| &zc.id == z).collect(),
            None => cfg.zones.iter().collect(),
        };
        if zones.is_empty() && args.zone.is_some() {
            anyhow::bail!(
                "no zones matched (--zone {:?}, configured: {:?})",
                args.zone,
                cfg.zones.iter().map(|z| &z.id).collect::<Vec<_>>(),
            );
        }
        zones
            .iter()
            .map(|zc| (zc.id.clone(), expand_tilde(&zc.ledger_path)))
            .collect()
    };

    if ledger_paths.is_empty() {
        anyhow::bail!("no ledger paths found (configure a [[zone]] or use --ledger-path)");
    }

    let mut all_results: Vec<(String, Vec<Event>)> = Vec::new();

    for (zone_id, ledger_path) in ledger_paths {
        let db_path = ledger_path.join("index.sqlite");
        if !db_path.exists() {
            continue;
        }
        let db = LedgerDb::open(&db_path)?;
        let events = query_events(
            &db,
            cutoff,
            &args.subsystem,
            class_filter.as_ref(),
            args.limit,
        )?;
        if !events.is_empty() {
            all_results.push((zone_id, events));
        }
    }

    match args.format.as_str() {
        "json" => emit_json(&all_results)?,
        _ => emit_text(&all_results, &args.subsystem, &args.since),
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Pure query logic
// ---------------------------------------------------------------------------

/// Run a filtered query against a single ledger.
pub(crate) fn query_events(
    db: &LedgerDb,
    cutoff: DateTime<Utc>,
    subsystem_patterns: &[String],
    class: Option<&EventClass>,
    limit: u32,
) -> Result<Vec<Event>> {
    let raw = db.recent_events(limit.saturating_mul(5).max(limit))?;

    let filtered: Vec<Event> = raw
        .into_iter()
        .filter(|e| e.timestamp >= cutoff)
        .filter(|e| match class {
            Some(c) => &e.event_class == c,
            None => true,
        })
        .filter(|e| subsystem_matches(subsystem_patterns, e))
        .take(limit as usize)
        .collect();

    Ok(filtered)
}

/// Returns true if the event's subsystem matches any pattern, or if no patterns given.
pub(crate) fn subsystem_matches(patterns: &[String], event: &Event) -> bool {
    if patterns.is_empty() {
        return true;
    }
    match extract_subsystem(event) {
        Some(name) => patterns.iter().any(|p| glob_match(p, name)),
        None => false,
    }
}

/// Extract the subsystem name from an event's payload or object_ref.
fn extract_subsystem(event: &Event) -> Option<&str> {
    if let Some(p) = event.payload.as_ref() {
        if let Some(s) = p.get("subsystem").and_then(|v| v.as_str()) {
            return Some(s);
        }
    }
    event
        .object_ref
        .as_deref()
        .and_then(|r| r.strip_prefix("subsystem:"))
}

/// Minimal glob: trailing `*` is a prefix wildcard, otherwise exact match.
pub(crate) fn glob_match(pattern: &str, value: &str) -> bool {
    if let Some(prefix) = pattern.strip_suffix('*') {
        value.starts_with(prefix)
    } else {
        pattern == value
    }
}

/// Parse a `--class` arg into an EventClass enum.
fn parse_class(s: &str) -> Result<EventClass> {
    match s.to_lowercase().as_str() {
        "ingest" => Ok(EventClass::Ingest),
        "route" => Ok(EventClass::Route),
        "decision" => Ok(EventClass::Decision),
        "statechange" | "state-change" | "state_change" => Ok(EventClass::StateChange),
        "claim" => Ok(EventClass::Claim),
        "delivery" => Ok(EventClass::Delivery),
        "projection" => Ok(EventClass::Projection),
        "health" => Ok(EventClass::Health),
        "approval" => Ok(EventClass::Approval),
        "notemeta" | "note-meta" | "note_meta" => Ok(EventClass::NoteMeta),
        other => anyhow::bail!(
            "unknown event class '{}' (valid: ingest, route, decision, state-change, claim, delivery, projection, health, approval, note-meta)",
            other
        ),
    }
}

/// Parse a time-window string: "24h", "7d", "4w", or "YYYY-MM-DD".
pub(crate) fn parse_since(since: &str) -> Result<DateTime<Utc>> {
    if let Some(hours_str) = since.strip_suffix('h') {
        let hours: i64 = hours_str
            .parse()
            .with_context(|| format!("invalid hour count: {}", since))?;
        return Ok(Utc::now() - Duration::hours(hours));
    }
    if let Some(days_str) = since.strip_suffix('d') {
        let days: i64 = days_str
            .parse()
            .with_context(|| format!("invalid day count: {}", since))?;
        return Ok(Utc::now() - Duration::days(days));
    }
    if let Some(weeks_str) = since.strip_suffix('w') {
        let weeks: i64 = weeks_str
            .parse()
            .with_context(|| format!("invalid week count: {}", since))?;
        return Ok(Utc::now() - Duration::weeks(weeks));
    }
    // Try ISO date.
    let date = chrono::NaiveDate::parse_from_str(since, "%Y-%m-%d")
        .with_context(|| format!("invalid date: {} (expected Nh, Nd, Nw, or YYYY-MM-DD)", since))?;
    Ok(date.and_hms_opt(0, 0, 0).unwrap().and_utc())
}

// ---------------------------------------------------------------------------
// Output formatting
// ---------------------------------------------------------------------------

fn emit_json(results: &[(String, Vec<Event>)]) -> Result<()> {
    let flat: Vec<serde_json::Value> = results
        .iter()
        .flat_map(|(zone, events)| {
            events.iter().map(move |e| {
                serde_json::json!({
                    "zone": zone,
                    "event": e,
                })
            })
        })
        .collect();
    let out =
        serde_json::to_string_pretty(&flat).with_context(|| "failed to serialize results")?;
    println!("{}", out);
    Ok(())
}

fn emit_text(results: &[(String, Vec<Event>)], patterns: &[String], since: &str) {
    let total: usize = results.iter().map(|(_, e)| e.len()).sum();

    if total == 0 {
        let scope = if patterns.is_empty() {
            String::new()
        } else {
            format!(" matching {:?}", patterns)
        };
        println!("No events found since {}{}.", since, scope);
        return;
    }

    println!("Found {} event(s) since {}", total, since);
    if !patterns.is_empty() {
        println!("Subsystem filter: {:?}", patterns);
    }
    println!();

    for (zone, events) in results {
        println!("Zone: {} ({} events)", zone, events.len());
        for event in events {
            let ts = event.timestamp.format("%Y-%m-%d %H:%M:%S");
            let subsystem = extract_subsystem(event).unwrap_or("?");
            let summary = event
                .payload
                .as_ref()
                .and_then(|p| p.get("summary"))
                .and_then(|s| s.as_str())
                .unwrap_or("");
            let class = format!("{:?}", event.event_class).to_lowercase();
            println!("  [{ts}] {class:14} {subsystem:30} {summary}");
        }
        println!();
    }
}

// ---------------------------------------------------------------------------
// Tests — ported from opsctl/crates/opsctl/src/query.rs
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use ledger_core::{PayloadTier, Segment, SegmentStore, ZoneId};
    use serde_json::json;
    use uuid::Uuid;

    fn write_dispatch_event(
        store: &SegmentStore,
        db: &mut LedgerDb,
        source: &str,
        seq: u64,
        subsystem: &str,
        summary: &str,
        timestamp: DateTime<Utc>,
    ) {
        let event = Event {
            event_id: Uuid::now_v7(),
            zone: "test-zone".to_string(),
            source: source.to_string(),
            source_seq: seq,
            timestamp,
            correlation_id: None,
            causation_id: None,
            actor_ref: Some(format!("machine:{}", source)),
            object_ref: Some(format!("subsystem:{}", subsystem)),
            event_class: EventClass::StateChange,
            payload_tier: PayloadTier::Structured,
            payload: Some(json!({
                "kind": "machine-dispatch",
                "machine": source,
                "subsystem": subsystem,
                "summary": summary,
            })),
        };
        let mut segment = Segment::new(source, seq);
        segment.append(event);
        segment.seal().unwrap();
        store.write_segment(&segment).unwrap();
        db.ingest_segment(&segment).unwrap();
    }

    fn test_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir()
            .join("jilog-test-query")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn build_test_ledger(name: &str) -> (std::path::PathBuf, LedgerDb) {
        let dir = test_dir(name);
        let ledger_path = dir.join("ledger");
        std::fs::create_dir_all(&ledger_path).unwrap();
        let store = SegmentStore::new(ZoneId::new("test-zone"), &ledger_path);
        let db_path = ledger_path.join("index.sqlite");
        let mut db = LedgerDb::open(&db_path).unwrap();

        let now = Utc::now();
        write_dispatch_event(&store, &mut db, "src1", 1, "hook-beads-logger",
            "logged 3 issues", now - Duration::hours(1));
        write_dispatch_event(&store, &mut db, "src1", 2, "hook-feature-capture",
            "captured 2 features", now - Duration::hours(2));
        write_dispatch_event(&store, &mut db, "src1", 3, "nightly-extractor",
            "109 signals, 1 P0", now - Duration::hours(6));
        write_dispatch_event(&store, &mut db, "src1", 4, "opsctl",
            "phase 1 deployed", now - Duration::days(20));
        let db = LedgerDb::open(&db_path).unwrap();
        (dir, db)
    }

    #[test]
    fn glob_match_prefix_wildcard() {
        assert!(glob_match("hook-*", "hook-beads-logger"));
        assert!(glob_match("hook-*", "hook-feature-capture"));
        assert!(glob_match("hook-*", "hook-"));
        assert!(!glob_match("hook-*", "nightly-extractor"));
        assert!(!glob_match("hook-*", "other-hook"));
    }

    #[test]
    fn glob_match_exact() {
        assert!(glob_match("nightly-extractor", "nightly-extractor"));
        assert!(!glob_match("nightly-extractor", "nightly-extractor-v2"));
        assert!(!glob_match("opsctl", "opsctl-something"));
    }

    #[test]
    fn subsystem_matches_empty_patterns_passes() {
        let event = Event {
            event_id: Uuid::now_v7(),
            zone: "z".into(),
            source: "s".into(),
            source_seq: 1,
            timestamp: Utc::now(),
            correlation_id: None,
            causation_id: None,
            actor_ref: None,
            object_ref: Some("subsystem:hook-test".into()),
            event_class: EventClass::StateChange,
            payload_tier: PayloadTier::Structured,
            payload: None,
        };
        assert!(subsystem_matches(&[], &event));
    }

    #[test]
    fn subsystem_matches_falls_back_to_object_ref() {
        let event = Event {
            event_id: Uuid::now_v7(),
            zone: "z".into(),
            source: "s".into(),
            source_seq: 1,
            timestamp: Utc::now(),
            correlation_id: None,
            causation_id: None,
            actor_ref: None,
            object_ref: Some("subsystem:opsctl".into()),
            event_class: EventClass::StateChange,
            payload_tier: PayloadTier::Structured,
            payload: None,
        };
        assert!(subsystem_matches(&["opsctl".into()], &event));
        assert!(!subsystem_matches(&["hook-*".into()], &event));
    }

    #[test]
    fn query_events_filters_by_subsystem_glob() {
        let (dir, db) = build_test_ledger("subsystem_glob");
        let cutoff = Utc::now() - Duration::days(7);

        let hook_results = query_events(
            &db,
            cutoff,
            &["hook-*".to_string()],
            None,
            100,
        ).unwrap();
        assert_eq!(hook_results.len(), 2, "hook-* should match 2 events");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn query_events_filters_by_exact_subsystem() {
        let (dir, db) = build_test_ledger("exact_subsystem");
        let cutoff = Utc::now() - Duration::days(7);

        let nightly = query_events(
            &db,
            cutoff,
            &["nightly-extractor".to_string()],
            None,
            100,
        ).unwrap();
        assert_eq!(nightly.len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn query_events_filters_by_time_cutoff() {
        let (dir, db) = build_test_ledger("time_cutoff");

        let recent = query_events(
            &db,
            Utc::now() - Duration::days(7),
            &[],
            None,
            100,
        ).unwrap();
        assert_eq!(recent.len(), 3, "expected 3 events within 7 days");

        let all = query_events(
            &db,
            Utc::now() - Duration::days(30),
            &[],
            None,
            100,
        ).unwrap();
        assert_eq!(all.len(), 4, "expected all 4 events within 30 days");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn query_events_multi_pattern_or_logic() {
        let (dir, db) = build_test_ledger("multi_pattern");
        let cutoff = Utc::now() - Duration::days(30);

        let results = query_events(
            &db,
            cutoff,
            &["hook-*".to_string(), "nightly-extractor".to_string()],
            None,
            100,
        ).unwrap();
        assert_eq!(results.len(), 3);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_class_known_values() {
        assert_eq!(parse_class("state-change").unwrap(), EventClass::StateChange);
        assert_eq!(parse_class("StateChange").unwrap(), EventClass::StateChange);
        assert_eq!(parse_class("approval").unwrap(), EventClass::Approval);
        assert!(parse_class("nonsense").is_err());
    }
}
