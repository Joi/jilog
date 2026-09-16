//! Archive spend — the digest's "Archive spend (agentsview)" block, built
//! from `agentsview usage daily --json` (yesterday + trailing 7 days, per
//! agent, top models). Money is `rust_decimal` from `microdollars`
//! (jilog#heyg).
//!
//! Nothing here may stall or fail the nightly: the CLI runs under a hard
//! timeout and every failure is returned as an error the caller turns into
//! a warning (the block is simply absent).

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use chrono::NaiveDate;
use rust_decimal::Decimal;

use crate::error::JilogReviewError;

/// One day of archive usage, as agentsview prices it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DailyUsage {
    pub date: NaiveDate,
    pub total_usd: Decimal,
    pub agents: BTreeMap<String, Decimal>,
    pub models: BTreeMap<String, Decimal>,
}

/// Spend over a period (one day, or the trailing week).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PeriodSpend {
    pub total_usd: Decimal,
    /// Days in the period that had an archive row.
    pub days: usize,
    pub agents: BTreeMap<String, Decimal>,
    pub models: BTreeMap<String, Decimal>,
}

impl PeriodSpend {
    fn add(&mut self, row: &DailyUsage) {
        self.days += 1;
        self.total_usd += row.total_usd;
        for (k, v) in &row.agents {
            *self.agents.entry(k.clone()).or_insert(Decimal::ZERO) += *v;
        }
        for (k, v) in &row.models {
            *self.models.entry(k.clone()).or_insert(Decimal::ZERO) += *v;
        }
    }

    /// Agents by cost, descending; ties by name.
    pub fn agents_by_cost(&self) -> Vec<(&String, &Decimal)> {
        let mut v: Vec<(&String, &Decimal)> = self.agents.iter().collect();
        v.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
        v
    }

    /// The `n` most expensive models, descending; ties by name.
    pub fn top_models(&self, n: usize) -> Vec<(&String, &Decimal)> {
        let mut v: Vec<(&String, &Decimal)> = self.models.iter().collect();
        v.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
        v.truncate(n);
        v
    }
}

/// Yesterday and the trailing seven days of archive spend, relative to
/// the digest date.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveSpend {
    /// The day before the digest date, when the archive had a row for it.
    pub yesterday: Option<PeriodSpend>,
    /// The seven days ending yesterday (rows present only).
    pub week: PeriodSpend,
    pub week_from: NaiveDate,
    pub week_to: NaiveDate,
}

impl ArchiveSpend {
    /// `(digest_date − 7, digest_date − 1)`.
    pub fn window(digest_date: NaiveDate) -> (NaiveDate, NaiveDate) {
        (
            digest_date - chrono::Duration::days(7),
            digest_date - chrono::Duration::days(1),
        )
    }

    /// Bucket the rows into yesterday and the trailing week; `None` when
    /// no row falls inside the window.
    pub fn summarize(rows: &[DailyUsage], digest_date: NaiveDate) -> Option<Self> {
        let (from, to) = Self::window(digest_date);
        let mut week = PeriodSpend::default();
        let mut yesterday: Option<PeriodSpend> = None;
        for row in rows.iter().filter(|r| r.date >= from && r.date <= to) {
            week.add(row);
            if row.date == to {
                let mut y = PeriodSpend::default();
                y.add(row);
                yesterday = Some(y);
            }
        }
        if week.days == 0 {
            return None;
        }
        Some(Self { yesterday, week, week_from: from, week_to: to })
    }
}

fn micro(v: &serde_json::Value, key: &str) -> Decimal {
    Decimal::new(
        v.get(key)
            .and_then(|c| c.get("microdollars"))
            .and_then(|m| m.as_i64())
            .unwrap_or(0),
        6,
    )
}

fn breakdown(v: &serde_json::Value, list: &str, name: &str) -> BTreeMap<String, Decimal> {
    let mut out = BTreeMap::new();
    for row in v.get(list).and_then(|b| b.as_array()).into_iter().flatten() {
        if let Some(k) = row.get(name).and_then(|n| n.as_str()) {
            *out.entry(k.to_string()).or_insert(Decimal::ZERO) += micro(row, "cost");
        }
    }
    out
}

/// Parse `agentsview usage daily --json` output (schema_version 6).
pub fn parse_daily_usage(raw: &str) -> Result<Vec<DailyUsage>, JilogReviewError> {
    let v: serde_json::Value = serde_json::from_str(raw)
        .map_err(|e| JilogReviewError::Reader(format!("agentsview usage daily: bad JSON: {}", e)))?;
    let daily = v
        .get("daily")
        .and_then(|d| d.as_array())
        .ok_or_else(|| JilogReviewError::Reader("agentsview usage daily: no `daily` array".into()))?;
    let mut rows = Vec::with_capacity(daily.len());
    for row in daily {
        let date = match row
            .get("date")
            .and_then(|d| d.as_str())
            .and_then(|s| s.parse::<NaiveDate>().ok())
        {
            Some(d) => d,
            None => {
                tracing::warn!("agentsview usage daily: row without a date skipped");
                continue;
            }
        };
        rows.push(DailyUsage {
            date,
            total_usd: micro(row, "totalCost"),
            agents: breakdown(row, "agentBreakdowns", "agent"),
            models: breakdown(row, "modelBreakdowns", "modelName"),
        });
    }
    Ok(rows)
}

/// Run `<bin> usage daily --json --since <from> --until <to> --no-sync`
/// under `timeout` and parse it. `--no-sync` keeps the CLI from spawning
/// or waiting on a daemon sync of its own.
pub fn fetch_daily_usage(
    bin: &Path,
    from: NaiveDate,
    to: NaiveDate,
    timeout: Duration,
) -> Result<Vec<DailyUsage>, JilogReviewError> {
    let mut cmd = std::process::Command::new(bin);
    cmd.args([
        "usage",
        "daily",
        "--json",
        "--since",
        &from.to_string(),
        "--until",
        &to.to_string(),
        "--no-sync",
    ]);
    let out = crate::util::run_with_timeout(&mut cmd, timeout).map_err(|e| {
        JilogReviewError::Reader(format!("agentsview usage daily ({}): {}", bin.display(), e))
    })?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(JilogReviewError::Reader(format!(
            "agentsview usage daily exit {}: {}",
            out.status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "signal".into()),
            stderr.lines().next().unwrap_or("").trim()
        )));
    }
    parse_daily_usage(&String::from_utf8_lossy(&out.stdout))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const DAILY: &str = r#"{"schema_version":6,"pricing":{"source":"fetched"},"projects":{},"daily":[
      {"date":"2026-09-14","inputTokens":1,"outputTokens":2,"cacheCreationTokens":0,"cacheReadTokens":0,"totalCost":{"microdollars":1500000},"modelsUsed":["gpt-5.6-sol"],
       "modelBreakdowns":[{"modelName":"gpt-5.6-sol","cost":{"microdollars":1500000}}],
       "agentBreakdowns":[{"agent":"codex","cost":{"microdollars":1500000}}],"machineBreakdowns":[],"projectBreakdowns":[]},
      {"date":"2026-09-15","inputTokens":12675971,"outputTokens":1834511,"cacheCreationTokens":5395288,"cacheReadTokens":261345608,"totalCost":{"microdollars":332138392},"modelsUsed":["gpt-6-astra","claude-opus-5","gpt-5.6-sol"],
       "modelBreakdowns":[{"modelName":"gpt-6-astra","cost":{"microdollars":125784488}},{"modelName":"claude-opus-5","cost":{"microdollars":107584332}},{"modelName":"gpt-5.6-sol","cost":{"microdollars":98769572}}],
       "agentBreakdowns":[{"agent":"codex","cost":{"microdollars":224554060}},{"agent":"claude","cost":{"microdollars":104943456}},{"agent":"cowork","cost":{"microdollars":2640876}}],
       "machineBreakdowns":[{"machineName":"4a70","cost":{"microdollars":332138392}}],"projectBreakdowns":[]},
      {"date":"2026-09-16","totalCost":{"microdollars":999},"modelBreakdowns":[],"agentBreakdowns":[]}
    ],"totals":{"totalCost":{"microdollars":333638392}},"sessionCounts":{"total":10}}"#;

    fn day(d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 9, d).unwrap()
    }

    #[test]
    fn parses_daily_rows_and_summarizes_yesterday_and_week() {
        let rows = parse_daily_usage(DAILY).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[1].total_usd.to_string(), "332.138392");
        assert_eq!(rows[1].agents["claude"].to_string(), "104.943456");
        assert_eq!(rows[1].models["gpt-6-astra"].to_string(), "125.784488");

        let digest = day(16);
        assert_eq!(ArchiveSpend::window(digest), (day(9), day(15)));
        let spend = ArchiveSpend::summarize(&rows, digest).expect("rows in window");
        let y = spend.yesterday.as_ref().expect("2026-09-15 present");
        assert_eq!(y.total_usd.to_string(), "332.138392");
        assert_eq!(spend.week.days, 2, "the 16th is today and excluded");
        assert_eq!(spend.week.total_usd.to_string(), "333.638392");
        assert_eq!(spend.week.agents["codex"].to_string(), "226.054060");
        let agents: Vec<&str> = spend.week.agents_by_cost().iter().map(|(a, _)| a.as_str()).collect();
        assert_eq!(agents, ["codex", "claude", "cowork"]);
        let models: Vec<&str> = spend.week.top_models(2).iter().map(|(m, _)| m.as_str()).collect();
        assert_eq!(models, ["gpt-6-astra", "claude-opus-5"]);

        // Yesterday absent → None for yesterday, week still summarized.
        let spend = ArchiveSpend::summarize(&rows, day(18)).unwrap();
        assert!(spend.yesterday.is_none());
        assert_eq!(spend.week.days, 3);
        // Nothing in window → None; an empty daily array → no rows → None.
        assert!(ArchiveSpend::summarize(&rows, NaiveDate::from_ymd_opt(2027, 1, 1).unwrap()).is_none());
        assert!(ArchiveSpend::summarize(&[], digest).is_none());
        assert_eq!(parse_daily_usage("{\"daily\": []}").unwrap().len(), 0);
        // Window edges: a row dated exactly digest − 7 is in, digest − 8 is out.
        let edge = |d: u32| DailyUsage {
            date: day(d),
            total_usd: Decimal::ONE,
            agents: Default::default(),
            models: Default::default(),
        };
        let s = ArchiveSpend::summarize(&[edge(8), edge(9)], digest).unwrap();
        assert_eq!(s.week.days, 1, "2026-09-08 is outside the 7-day window ending 2026-09-15");
        // Top-5 truncation, descending.
        let mut many = edge(15);
        for i in 0..8 {
            many.models.insert(format!("m{i}"), Decimal::from(i));
        }
        let s = ArchiveSpend::summarize(&[many], digest).unwrap();
        let top: Vec<&str> = s.week.top_models(5).iter().map(|(m, _)| m.as_str()).collect();
        assert_eq!(top, ["m7", "m6", "m5", "m4", "m3"]);
        // Garbage → Err, not a panic.
        assert!(parse_daily_usage("not json").is_err());
        assert!(parse_daily_usage("{\"daily\": 5}").is_err());
    }

    #[cfg(unix)]
    fn script(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join(name);
        std::fs::write(&p, body).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[cfg(unix)]
    #[test]
    fn fetch_uses_the_binary_with_window_flags_and_times_out() {
        let dir = tempfile::tempdir().unwrap();
        let calls = dir.path().join("calls");
        let stub = script(
            dir.path(),
            "agentsview",
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\ncat <<'EOF'\n{}\nEOF\n",
                calls.display(),
                DAILY
            ),
        );
        let (from, to) = (day(9), day(15));
        let rows = fetch_daily_usage(&stub, from, to, Duration::from_secs(10)).unwrap();
        assert_eq!(rows.len(), 3);
        let argv = std::fs::read_to_string(&calls).unwrap();
        assert_eq!(
            argv.trim(),
            "usage daily --json --since 2026-09-09 --until 2026-09-15 --no-sync"
        );

        // A hung binary is killed at the deadline.
        let slow = script(dir.path(), "slow", "#!/bin/sh\nsleep 30\n");
        let started = std::time::Instant::now();
        let err = fetch_daily_usage(&slow, from, to, Duration::from_millis(300))
            .unwrap_err()
            .to_string();
        assert!(started.elapsed() < Duration::from_secs(5), "must not wait for the child");
        assert!(err.contains("timed out"), "{err}");
        // Missing binary → Err.
        assert!(fetch_daily_usage(&dir.path().join("missing"), from, to, Duration::from_secs(1)).is_err());
        // Non-zero exit → Err naming the status and the first stderr line.
        let bad = script(dir.path(), "bad", "#!/bin/sh\necho 'fatal: no archive' >&2\nexit 3\n");
        let err = fetch_daily_usage(&bad, from, to, Duration::from_secs(5)).unwrap_err().to_string();
        assert!(err.contains("exit 3") && err.contains("no archive"), "{err}");
    }
}
