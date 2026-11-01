//! Standing environment-workaround registers (jibot-code#x9k9).
//!
//! A register is a markdown file whose `## Standing` table lists each
//! environment defect worked around instead of fixed, with a tracking issue,
//! an owner and an expiry date. The format is the one jibot-code's
//! `docs/operations/environment-workarounds.md` defines and its
//! `scripts/env-workarounds.py` checks; this module reads the same table so
//! the nightly digest can list rows past expiry under P0 Alerts.
//!
//! The digest date decides expiry: a row whose `Expires` date is before it
//! has expired, as in the script. A register that cannot be read or does not
//! parse is reported as a problem, never skipped: a broken register would
//! otherwise hide every expiry in it.

use std::path::{Path, PathBuf};

use chrono::NaiveDate;

/// The Standing table's header, in order.
pub const COLUMNS: [&str; 6] = ["Workaround", "Hosts", "Tracking", "Owner", "Added", "Expires"];

/// One row of a Standing table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StandingRow {
    pub workaround: String,
    pub hosts: String,
    pub tracking: String,
    pub owner: String,
    pub added: NaiveDate,
    pub expires: NaiveDate,
}

/// A row past its expiry date, with the register it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpiredWorkaround {
    pub register: PathBuf,
    pub row: StandingRow,
}

/// A register that could not be read or parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisterProblem {
    pub register: PathBuf,
    pub message: String,
}

/// The result of checking every configured register against one date.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkaroundCheck {
    /// Registers checked, readable or not.
    pub registers: usize,
    /// Standing rows across the registers that parsed.
    pub standing: usize,
    pub expired: Vec<ExpiredWorkaround>,
    pub problems: Vec<RegisterProblem>,
}

impl WorkaroundCheck {
    /// Lines this check adds to the digest's P0 Alerts section.
    pub fn alert_count(&self) -> usize {
        self.expired.len() + self.problems.len()
    }
}

fn cells(line: &str) -> Vec<String> {
    let t = line.trim();
    let t = t.strip_prefix('|').unwrap_or(t);
    let t = t.strip_suffix('|').unwrap_or(t);
    t.split('|').map(|c| c.trim().to_string()).collect()
}

fn is_separator_cell(c: &str) -> bool {
    let c = c.strip_prefix(':').unwrap_or(c);
    let c = c.strip_suffix(':').unwrap_or(c);
    c.len() >= 3 && c.chars().all(|ch| ch == '-')
}

fn is_tracking_ref(s: &str) -> bool {
    let Some((project, id)) = s.split_once('#') else {
        return false;
    };
    let lower_alnum = |ch: char| ch.is_ascii_lowercase() || ch.is_ascii_digit();
    let mut p = project.chars();
    p.next().is_some_and(lower_alnum)
        && p.all(|ch| lower_alnum(ch) || ch == '-')
        && id.len() == 4
        && id.chars().all(lower_alnum)
}

fn date(value: &str, what: &str, lineno: usize) -> Result<NaiveDate, String> {
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .ok()
        .filter(|_| value.len() == 10)
        .ok_or_else(|| format!("line {lineno}: {what} {value:?} is not a YYYY-MM-DD date"))
}

/// Parse the `## Standing` table. Errors name the line, as the Python
/// checker's do; the 30-day limit is the register's rule, and a row that
/// breaks it is reported, not trusted.
pub fn parse_standing(text: &str) -> Result<Vec<StandingRow>, String> {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines
        .iter()
        .position(|l| l.trim() == "## Standing")
        .ok_or_else(|| "no '## Standing' section".to_string())?;
    let mut table: Vec<(usize, &str)> = Vec::new();
    for (i, line) in lines.iter().enumerate().skip(start + 1) {
        if line.starts_with("## ") {
            break;
        }
        if line.trim_start().starts_with('|') {
            table.push((i + 1, line));
        } else if !table.is_empty() && !line.trim().is_empty() {
            break;
        }
    }
    if table.len() < 2 || cells(table[0].1) != COLUMNS {
        return Err(format!(
            "the Standing table must have the columns: {}",
            COLUMNS.join(" | ")
        ));
    }
    let (sep_lineno, sep) = table[1];
    let sep_cells = cells(sep);
    if sep_cells.len() != COLUMNS.len() || !sep_cells.iter().all(|c| is_separator_cell(c)) {
        return Err(format!(
            "line {sep_lineno}: the header must be followed by a |---| separator row of {} columns",
            COLUMNS.len()
        ));
    }
    let mut rows = Vec::new();
    for &(lineno, line) in &table[2..] {
        let c = cells(line);
        if c.len() != COLUMNS.len() {
            return Err(format!(
                "line {lineno}: {} columns, expected {}",
                c.len(),
                COLUMNS.len()
            ));
        }
        if c[0].is_empty() {
            return Err(format!("line {lineno}: empty Workaround"));
        }
        if !is_tracking_ref(&c[2]) {
            return Err(format!("line {lineno}: tracking {:?} is not <project>#<id>", c[2]));
        }
        if c[3].is_empty() {
            return Err(format!("line {lineno}: empty owner"));
        }
        let added = date(&c[4], "Added date", lineno)?;
        let expires = date(&c[5], "Expires date", lineno)?;
        if expires < added {
            return Err(format!("line {lineno}: Expires {expires} is before Added {added}"));
        }
        if (expires - added).num_days() > 30 {
            return Err(format!("line {lineno}: Expires is more than 30 days after Added"));
        }
        rows.push(StandingRow {
            workaround: c[0].clone(),
            hosts: c[1].clone(),
            tracking: c[2].clone(),
            owner: c[3].clone(),
            added,
            expires,
        });
    }
    Ok(rows)
}

/// Check every register against `today`. Order follows `registers`, then
/// the table.
pub fn check_registers(registers: &[PathBuf], today: NaiveDate) -> WorkaroundCheck {
    let mut check = WorkaroundCheck {
        registers: registers.len(),
        ..WorkaroundCheck::default()
    };
    for path in registers {
        match read_register(path) {
            Ok(rows) => {
                check.standing += rows.len();
                for row in rows {
                    if row.expires < today {
                        check.expired.push(ExpiredWorkaround {
                            register: path.clone(),
                            row,
                        });
                    }
                }
            }
            Err(message) => check.problems.push(RegisterProblem {
                register: path.clone(),
                message,
            }),
        }
    }
    check
}

/// Read and parse one register; the error says why it was not checked.
pub fn read_register(path: &Path) -> Result<Vec<StandingRow>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read: {e}"))?;
    parse_standing(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    const REGISTER: &str = "\
# Standing environment workarounds

## Rules

| not | the | table |

## Standing

Seeded on 2026-10-27.

| Workaround | Hosts | Tracking | Owner | Added | Expires |
|------------|-------|----------|-------|-------|---------|
| Codex reviews run on joimba | macct | jibot-code#3x7y | macct host | 2026-10-01 | 2026-10-31 |
| Fonts lost; export via PowerPoint | macazbd | jibot-code#5db9 | macazbd host | 2026-10-27 | 2026-11-26 |

## Retired

| Workaround | Fixed by | Date |
|------------|----------|------|
| old | jibot-code#abej | 2026-10-18 |
";

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn parses_the_standing_table_only() {
        let rows = parse_standing(REGISTER).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].tracking, "jibot-code#3x7y");
        assert_eq!(rows[0].owner, "macct host");
        assert_eq!(rows[1].expires, d("2026-11-26"));
    }

    #[test]
    fn expiry_is_strictly_before_the_digest_date() {
        let dir = std::env::temp_dir().join(format!("jilog-wr-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("register.md");
        std::fs::write(&path, REGISTER).unwrap();

        let on_the_day = check_registers(std::slice::from_ref(&path), d("2026-10-31"));
        assert!(on_the_day.expired.is_empty(), "a row is live on its Expires date");
        assert_eq!(on_the_day.standing, 2);

        let after = check_registers(std::slice::from_ref(&path), d("2026-11-01"));
        assert_eq!(after.expired.len(), 1);
        assert_eq!(after.expired[0].row.tracking, "jibot-code#3x7y");
        assert_eq!(after.expired[0].register, path);
        assert_eq!(after.alert_count(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_or_malformed_register_is_a_problem_not_silence() {
        let missing = PathBuf::from("/nonexistent/jilog/register.md");
        let check = check_registers(std::slice::from_ref(&missing), d("2026-11-01"));
        assert_eq!(check.problems.len(), 1);
        assert_eq!(check.problems[0].register, missing);
        assert!(check.problems[0].message.starts_with("cannot read"));
        assert_eq!(check.alert_count(), 1);
    }

    #[test]
    fn malformed_rows_are_rejected_with_the_line() {
        let bad = |row: &str| {
            parse_standing(&format!(
                "## Standing\n\n| Workaround | Hosts | Tracking | Owner | Added | Expires |\n|---|---|---|---|---|---|\n{row}\n"
            ))
            .unwrap_err()
        };
        assert!(bad("| w | h | JIBOT#3x7y | o | 2026-10-01 | 2026-10-02 |").contains("tracking"));
        assert!(bad("| w | h | jibot-code#3x7y | | 2026-10-01 | 2026-10-02 |").contains("owner"));
        assert!(bad("| w | h | jibot-code#3x7y | o | 2026-10-01 | 2026-12-02 |").contains("30 days"));
        assert!(bad("| w | h | jibot-code#3x7y | o | 2026-10-05 | 2026-10-02 |").contains("before"));
        assert!(bad("| w | h | jibot-code#3x7y | o | 2026-1-5 | 2026-10-02 |").contains("line 5"));
        assert!(bad("| w | h | jibot-code#3x7y | o |").contains("columns"));
        assert!(parse_standing("# nothing\n").unwrap_err().contains("Standing"));
        assert!(parse_standing("## Standing\n\n| A | B |\n|---|---|\n").unwrap_err().contains("columns"));
    }

    #[test]
    fn an_empty_table_is_well_formed() {
        let rows = parse_standing(
            "## Standing\n\n| Workaround | Hosts | Tracking | Owner | Added | Expires |\n|---|---|---|---|---|---|\n",
        )
        .unwrap();
        assert!(rows.is_empty());
    }
}
