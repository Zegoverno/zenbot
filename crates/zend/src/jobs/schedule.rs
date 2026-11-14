//! When a job runs: parsing a schedule (cron in the job's timezone, `every`, `at`, `in`), its next
//! run, its period, and how late a run may start (`grace`).

use std::str::FromStr;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;

/// When a job runs.
#[derive(Debug, Clone)]
pub enum Schedule {
    Cron(Box<croner::Cron>),
    Every(chrono::Duration),
    At(DateTime<Utc>),
}

/// A duration like `30m`, `2h`, `1d`, `90s`. None for a number too large to be a duration.
fn duration(s: &str) -> Option<chrono::Duration> {
    let s = s.trim();
    let split = s.find(|c: char| !c.is_ascii_digit())?;
    let (n, unit) = s.split_at(split);
    let n: i64 = n.parse().ok()?;
    match unit.trim() {
        "s" | "sec" | "secs" | "second" | "seconds" => chrono::Duration::try_seconds(n),
        "m" | "min" | "mins" | "minute" | "minutes" => chrono::Duration::try_minutes(n),
        "h" | "hour" | "hours" => chrono::Duration::try_hours(n),
        "d" | "day" | "days" => chrono::Duration::try_days(n),
        _ => None,
    }
}

pub fn parse_tz(tz: &str) -> Result<Tz> {
    Tz::from_str(tz.trim()).map_err(|_| anyhow::anyhow!("unknown timezone `{tz}` (use an IANA name such as Europe/Lisbon or UTC)"))
}

/// Parse a schedule: a 5-field cron expression (or `@daily`, `@hourly`, …), `every <duration>`,
/// `at <date and time>` (read in `tz` unless it has an offset) or `in <duration>` (made into an `at`).
/// Returns the schedule and its canonical text (what's stored).
pub fn parse_schedule(text: &str, tz: Tz, now: DateTime<Utc>) -> Result<(Schedule, String)> {
    let t = text.trim();
    let lower = t.to_lowercase();
    if let Some(d) = lower.strip_prefix("every ") {
        let d = duration(d).context("`every` takes a duration such as 30m, 2h or 1d")?;
        anyhow::ensure!(d > chrono::Duration::zero(), "the interval must be positive");
        anyhow::ensure!(now.checked_add_signed(d).is_some(), "that interval is too long");
        return Ok((Schedule::Every(d), format!("every {}", d_text(d))));
    }
    if let Some(d) = lower.strip_prefix("in ") {
        let d = duration(d).context("`in` takes a duration such as 30m, 2h or 1d")?;
        let at = now.checked_add_signed(d).context("that is too far ahead")?;
        return Ok((Schedule::At(at), format!("at {}", at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))));
    }
    if let Some(when) = t.strip_prefix("at ").or_else(|| t.strip_prefix("At ")) {
        let when = when.trim();
        let at = if let Ok(d) = DateTime::parse_from_rfc3339(when) {
            d.with_timezone(&Utc)
        } else {
            let naive = ["%Y-%m-%dT%H:%M:%S", "%Y-%m-%dT%H:%M", "%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M"]
                .iter()
                .find_map(|f| NaiveDateTime::parse_from_str(when, f).ok())
                .context("`at` takes a date and time such as 2026-10-12 07:00 (in the job's timezone) or an RFC 3339 time")?;
            tz.from_local_datetime(&naive).earliest().context("that time doesn't exist in the job's timezone (a clock change)")?.with_timezone(&Utc)
        };
        return Ok((Schedule::At(at), format!("at {}", at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))));
    }
    // Minute granularity: no seconds or years field.
    anyhow::ensure!(t.starts_with('@') || t.split_whitespace().count() == 5, "a cron schedule has 5 fields: minute hour day-of-month month day-of-week (e.g. `0 7 * * 1-5`)");
    let cron = croner::Cron::from_str(t).map_err(|e| anyhow::anyhow!("bad cron expression `{t}`: {e}"))?;
    Ok((Schedule::Cron(Box::new(cron)), t.to_string()))
}

fn d_text(d: chrono::Duration) -> String {
    let s = d.num_seconds();
    if s % 86400 == 0 {
        format!("{}d", s / 86400)
    } else if s % 3600 == 0 {
        format!("{}h", s / 3600)
    } else if s % 60 == 0 {
        format!("{}m", s / 60)
    } else {
        format!("{s}s")
    }
}

/// The first run strictly after `after`, None when there's none (a one-shot in the past).
pub fn next_after(s: &Schedule, tz: Tz, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
    match s {
        Schedule::Cron(c) => c.find_next_occurrence(&after.with_timezone(&tz), false).ok().map(|t| t.with_timezone(&Utc)),
        Schedule::Every(d) => after.checked_add_signed(*d),
        Schedule::At(t) => (*t > after).then_some(*t),
    }
}

/// The run after the one that was due at `due`, strictly after `now`. An interval keeps its rhythm
/// from the due time (`due + k·d`), not from when the tick saw it, so it doesn't drift; runs it
/// passed while late are skipped (the tick already ran or recorded the one due).
pub fn next_from(s: &Schedule, tz: Tz, due: DateTime<Utc>, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    match s {
        Schedule::Every(d) if due <= now && *d > chrono::Duration::zero() => {
            let (late, step) = ((now - due).num_milliseconds(), d.num_milliseconds().max(1));
            due.checked_add_signed(chrono::Duration::try_milliseconds((late / step + 1).checked_mul(step)?)?)
        }
        _ => next_after(s, tz, now),
    }
}

/// How many runs ahead `period` looks at: a cron expression's gaps can differ (`0,30-35 * * * *`).
const PERIOD_SAMPLES: usize = 50;

/// The shortest time between two runs (of the next PERIOD_SAMPLES after `from`); None for a one-shot.
pub fn period(s: &Schedule, tz: Tz, from: DateTime<Utc>) -> Option<chrono::Duration> {
    match s {
        Schedule::At(_) => None,
        Schedule::Every(d) => Some(*d),
        Schedule::Cron(_) => {
            let mut prev = next_after(s, tz, from)?;
            let mut min: Option<chrono::Duration> = None;
            for _ in 0..PERIOD_SAMPLES {
                let Some(next) = next_after(s, tz, prev) else { break };
                min = Some(min.map_or(next - prev, |m| m.min(next - prev)));
                prev = next;
            }
            min
        }
    }
}

/// How late a run may start and still run: half its period, between 2 minutes and 2 hours (2 hours
/// for a one-shot).
pub fn grace(period: Option<chrono::Duration>) -> chrono::Duration {
    let p = period.unwrap_or(chrono::Duration::hours(4));
    (p / 2).clamp(chrono::Duration::minutes(2), chrono::Duration::hours(2))
}

/// Shortest period of an agent job.
const MIN_PERIOD: Duration = Duration::from_secs(300);
/// The schedule checked for an agent job: parsed, not more often than every 5 minutes, and with a run
/// ahead. Returns the canonical text and the first run.
pub(super) fn checked_schedule(text: &str, tz: Tz, now: DateTime<Utc>) -> Result<(String, DateTime<Utc>)> {
    let (s, canon) = parse_schedule(text, tz, now)?;
    if let Some(p) = period(&s, tz, now) {
        anyhow::ensure!(p >= chrono::Duration::from_std(MIN_PERIOD).unwrap_or_default(), "an agent job runs at most every 5 minutes");
    }
    let next = next_after(&s, tz, now).context("that schedule has no run ahead (a time in the past?)")?;
    Ok((canon, next))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn cron_runs_in_the_jobs_timezone() {
        let tz = parse_tz("America/Argentina/Buenos_Aires").unwrap();
        let (s, canon) = parse_schedule("0 7 * * 1-5", tz, Utc::now()).unwrap();
        assert_eq!(canon, "0 7 * * 1-5");
        // Friday 2026-10-09 12:00 UTC (09:00 in Buenos Aires): next is Monday 07:00 there, 10:00 UTC.
        assert_eq!(next_after(&s, tz, t("2026-10-09T12:00:00Z")), Some(t("2026-10-12T10:00:00Z")));
        assert_eq!(period(&s, tz, t("2026-10-12T11:00:00Z")), Some(chrono::Duration::days(1)));
    }

    #[test]
    fn every_in_and_at_parse() {
        let now = t("2026-10-09T12:00:00Z");
        let tz = parse_tz("America/Argentina/Buenos_Aires").unwrap();
        let (s, canon) = parse_schedule("every 2h", tz, now).unwrap();
        assert_eq!(canon, "every 2h");
        assert_eq!(next_after(&s, tz, now), Some(t("2026-10-09T14:00:00Z")));
        let (s, canon) = parse_schedule("in 30m", tz, now).unwrap();
        assert_eq!(canon, "at 2026-10-09T12:30:00Z");
        assert_eq!(next_after(&s, tz, now), Some(t("2026-10-09T12:30:00Z")));
        assert_eq!(next_after(&s, tz, t("2026-10-09T13:00:00Z")), None, "a one-shot has nothing after it");
        let (_, canon) = parse_schedule("at 2026-10-12 07:00", tz, now).unwrap();
        assert_eq!(canon, "at 2026-10-12T10:00:00Z", "read in the job's timezone");
        assert!(parse_schedule("* * * * * *", tz, now).is_err(), "no seconds field");
        assert!(parse_schedule("every soon", tz, now).is_err());
        assert!(parse_tz("Mars/Olympus").is_err());
    }

    #[test]
    fn huge_durations_are_errors_not_panics() {
        let now = t("2026-10-09T12:00:00Z");
        for text in ["every 9999999999999d", "in 9999999999999d", "every 9223372036854775807s", "in 99999999999h", "every 300000000d"] {
            assert!(parse_schedule(text, Tz::UTC, now).is_err(), "{text}");
        }
        // A valid duration that takes the date past what a date can hold.
        assert!(parse_schedule("in 100000000d", Tz::UTC, now).is_err());
        assert!(parse_schedule("every 100000000d", Tz::UTC, now).is_err());
    }

    #[test]
    fn the_shortest_gap_of_a_cron_counts_toward_the_minimum() {
        let now = t("2026-10-09T12:00:00Z");
        let (s, _) = parse_schedule("0,30-35 * * * *", Tz::UTC, now).unwrap();
        assert_eq!(period(&s, Tz::UTC, now), Some(chrono::Duration::minutes(1)), "not the 30 minutes between the first two");
        assert!(checked_schedule("0,30-35 * * * *", Tz::UTC, now).is_err());
        assert!(checked_schedule("0,30 * * * *", Tz::UTC, now).is_ok());
    }

    #[test]
    fn an_interval_keeps_its_rhythm_from_the_due_time() {
        let due = t("2026-10-09T12:00:00Z");
        let (s, _) = parse_schedule("every 1h", Tz::UTC, due).unwrap();
        // Seen 20 seconds late: the next run is still on the hour.
        assert_eq!(next_from(&s, Tz::UTC, due, t("2026-10-09T12:00:20Z")), Some(t("2026-10-09T13:00:00Z")));
        // Seen three and a half hours late: the runs it passed are skipped, the rhythm kept.
        assert_eq!(next_from(&s, Tz::UTC, due, t("2026-10-09T15:30:00Z")), Some(t("2026-10-09T16:00:00Z")));
        // Exactly on a later beat: strictly after now.
        assert_eq!(next_from(&s, Tz::UTC, due, t("2026-10-09T14:00:00Z")), Some(t("2026-10-09T15:00:00Z")));
        let (c, _) = parse_schedule("0 * * * *", Tz::UTC, due).unwrap();
        assert_eq!(next_from(&c, Tz::UTC, due, t("2026-10-09T12:00:20Z")), Some(t("2026-10-09T13:00:00Z")));
    }

    #[test]
    fn agent_jobs_run_at_most_every_five_minutes_and_have_a_run_ahead() {
        let now = t("2026-10-09T12:00:00Z");
        assert!(checked_schedule("* * * * *", Tz::UTC, now).is_err());
        assert!(checked_schedule("every 1m", Tz::UTC, now).is_err());
        assert!(checked_schedule("*/5 * * * *", Tz::UTC, now).is_ok());
        assert!(checked_schedule("at 2026-10-01 07:00", Tz::UTC, now).is_err(), "in the past");
    }

    #[test]
    fn grace_is_half_the_period_within_bounds() {
        assert_eq!(grace(Some(chrono::Duration::days(1))), chrono::Duration::hours(2));
        assert_eq!(grace(Some(chrono::Duration::minutes(30))), chrono::Duration::minutes(15));
        assert_eq!(grace(Some(chrono::Duration::minutes(2))), chrono::Duration::minutes(2));
        assert_eq!(grace(None), chrono::Duration::hours(2));
    }
}
