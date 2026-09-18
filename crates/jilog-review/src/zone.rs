//! The digest's calendar zone (jilog#0qpq).
//!
//! The nightly runs late in the evening, local time. Taking the digest
//! date from `Utc::now()` made "yesterday" a UTC date while the morning
//! brief takes yesterday in Joi's zone and passes `--timezone` to
//! agentsview; the two reports disagreed every morning
//! (amplifier-bundle-joi#rvbm). One zone now decides the digest date, the
//! archive-spend window and agentsview's day bucketing.
//!
//! Resolution order, first hit wins:
//!
//! 1. `JILOG_TZ` — an explicit per-run override; invalid is an error.
//! 2. `timezone` in jilog.toml — validated at config load; invalid is an
//!    error here too.
//! 3. `TZ` — honoured when it names an IANA zone; a POSIX rule string
//!    (`JST-9`) is not one, and falls through.
//! 4. The system zone (`iana_time_zone::get_timezone`).
//! 5. UTC, with a warning: the digest still runs, on the old clock.

use chrono::{DateTime, NaiveDate, Utc};
use chrono_tz::Tz;

use crate::error::JilogReviewError;

/// Environment variable that overrides every other zone source.
pub const ENV_OVERRIDE: &str = "JILOG_TZ";

/// Where the zone came from, for the run log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZoneSource {
    EnvOverride,
    Config,
    EnvTz,
    System,
    Fallback,
}

impl std::fmt::Display for ZoneSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            ZoneSource::EnvOverride => ENV_OVERRIDE,
            ZoneSource::Config => "jilog.toml timezone",
            ZoneSource::EnvTz => "TZ",
            ZoneSource::System => "system zone",
            ZoneSource::Fallback => "fallback",
        })
    }
}

/// Parse an IANA zone name; the error names the source so a typo in
/// jilog.toml reads differently from one in `JILOG_TZ`.
pub fn parse_zone(name: &str, source: &str) -> Result<Tz, JilogReviewError> {
    name.trim()
        .parse::<Tz>()
        .map_err(|_| JilogReviewError::Config(format!("{}: not an IANA time zone: {:?}", source, name)))
}

/// Resolve the zone. `env_override` is the caller's reading of
/// [`ENV_OVERRIDE`] (passed in, not read here, so a test never has to
/// touch the process environment); `TZ` and the system zone are read
/// here, and only when neither override nor config decides — a run or a
/// test with either set never consults the process or the system.
pub fn resolve_zone(
    env_override: Option<&str>,
    configured: Option<&str>,
) -> Result<(Tz, ZoneSource), JilogReviewError> {
    let env_override = env_override.filter(|v| !v.trim().is_empty());
    if env_override.is_some() || configured.is_some() {
        return resolve_zone_from(env_override, configured, None, None);
    }
    let env_tz = std::env::var("TZ").ok().filter(|v| !v.trim().is_empty());
    resolve_zone_from(None, None, env_tz.as_deref(), iana_time_zone::get_timezone().ok().as_deref())
}

/// The pure resolution behind [`resolve_zone`], with every input passed
/// in so the order is testable.
pub fn resolve_zone_from(
    env_override: Option<&str>,
    configured: Option<&str>,
    env_tz: Option<&str>,
    system: Option<&str>,
) -> Result<(Tz, ZoneSource), JilogReviewError> {
    if let Some(v) = env_override {
        return parse_zone(v, ENV_OVERRIDE).map(|tz| (tz, ZoneSource::EnvOverride));
    }
    if let Some(v) = configured {
        return parse_zone(v, "jilog.toml timezone").map(|tz| (tz, ZoneSource::Config));
    }
    if let Some(tz) = env_tz.and_then(|v| v.trim().parse::<Tz>().ok()) {
        return Ok((tz, ZoneSource::EnvTz));
    }
    if let Some(tz) = system.and_then(|v| v.trim().parse::<Tz>().ok()) {
        return Ok((tz, ZoneSource::System));
    }
    Ok((Tz::UTC, ZoneSource::Fallback))
}

/// The calendar date of `now` in `zone` — the digest date.
pub fn local_date(now: DateTime<Utc>, zone: Tz) -> NaiveDate {
    now.with_timezone(&zone).date_naive()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ArchiveSpend;
    use chrono::TimeZone;

    fn ymd(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    #[test]
    fn digest_date_and_window_follow_the_zone() {
        // 22:30Z on the 17th is already 04:30 on the 18th in Thimphu (+06).
        let now = Utc.with_ymd_and_hms(2026, 9, 17, 22, 30, 0).unwrap();
        let thimphu: Tz = "Asia/Thimphu".parse().unwrap();
        let date = local_date(now, thimphu);
        assert_eq!(date, ymd(2026, 9, 18));
        assert_eq!(ArchiveSpend::window(date), (ymd(2026, 9, 11), ymd(2026, 9, 17)));

        let date = local_date(now, Tz::UTC);
        assert_eq!(date, ymd(2026, 9, 17));
        assert_eq!(ArchiveSpend::window(date), (ymd(2026, 9, 10), ymd(2026, 9, 16)));

        // The 22:50 nightly on macazbd: 16:50Z the same day, still the 17th
        // in Thimphu — the digest is dated the day it ran, yesterday is the 16th.
        let run = Utc.with_ymd_and_hms(2026, 9, 17, 16, 50, 0).unwrap();
        assert_eq!(local_date(run, thimphu), ymd(2026, 9, 17));
        // Tokyo (+09) has already turned over.
        let tokyo: Tz = "Asia/Tokyo".parse().unwrap();
        assert_eq!(local_date(run, tokyo), ymd(2026, 9, 18));
    }

    #[test]
    fn resolution_order_is_override_config_tz_system_utc() {
        let r = |o, c, t, s| resolve_zone_from(o, c, t, s).unwrap();
        assert_eq!(
            r(Some("Asia/Tokyo"), Some("Asia/Thimphu"), Some("Europe/Paris"), Some("America/New_York")),
            (chrono_tz::Asia::Tokyo, ZoneSource::EnvOverride)
        );
        assert_eq!(
            r(None, Some("Asia/Thimphu"), Some("Europe/Paris"), Some("America/New_York")),
            (chrono_tz::Asia::Thimphu, ZoneSource::Config)
        );
        assert_eq!(
            r(None, None, Some("Europe/Paris"), Some("America/New_York")),
            (chrono_tz::Europe::Paris, ZoneSource::EnvTz)
        );
        // A POSIX TZ rule (not the legacy IANA names like EST5EDT) is not a
        // zone name: fall through to the system zone.
        assert_eq!(
            r(None, None, Some("JST-9"), Some("America/New_York")),
            (chrono_tz::America::New_York, ZoneSource::System)
        );
        assert_eq!(r(None, None, None, None), (Tz::UTC, ZoneSource::Fallback));
        assert_eq!(r(None, None, None, Some("garbage")), (Tz::UTC, ZoneSource::Fallback));
        // Explicit sources must be right: a typo is an error, not a silent UTC.
        let err = resolve_zone_from(Some("Asia/Thimpu"), None, None, None).unwrap_err().to_string();
        assert!(err.contains("JILOG_TZ") && err.contains("Asia/Thimpu"), "{err}");
        let err = resolve_zone_from(None, Some(""), None, None).unwrap_err().to_string();
        assert!(err.contains("jilog.toml timezone"), "{err}");
        assert_eq!(resolve_zone_from(None, Some(" Asia/Thimphu "), None, None).unwrap().0, chrono_tz::Asia::Thimphu);
    }
}
