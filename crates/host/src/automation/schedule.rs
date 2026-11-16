//! When a schedule fires: a cron pattern read the way croner reads it, or one ISO 8601 moment.

use std::str::FromStr;

use chrono::{DateTime, Local, LocalResult, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;
use croner::Cron;

#[derive(Debug, Clone, Copy)]
pub enum Zone {
    Local,
    Named(Tz),
}

impl Zone {
    pub fn parse(timezone: Option<&str>) -> Result<Self, String> {
        match timezone {
            None => Ok(Zone::Local),
            Some(name) => Tz::from_str(name).map(Zone::Named).map_err(|_| format!("Invalid time zone specified: {name}")),
        }
    }

    /// A wall-clock time in this zone. A time the clocks skip lands after the gap; a repeated one takes its first.
    fn resolve(self, wall: NaiveDateTime) -> Option<DateTime<Utc>> {
        fn pick<T: TimeZone>(result: LocalResult<DateTime<T>>, zone: &T, wall: NaiveDateTime) -> Option<DateTime<Utc>> {
            match result {
                LocalResult::Single(time) | LocalResult::Ambiguous(time, _) => Some(time.with_timezone(&Utc)),
                LocalResult::None => (1..=3 * 60).find_map(|minutes| zone.from_local_datetime(&(wall + chrono::Duration::minutes(minutes))).earliest()).map(|time| time.with_timezone(&Utc)),
            }
        }
        match self {
            Zone::Local => pick(Local.from_local_datetime(&wall), &Local, wall),
            Zone::Named(tz) => pick(tz.from_local_datetime(&wall), &tz, wall),
        }
    }
}

#[derive(Debug, Clone)]
pub enum Schedule {
    Cron { cron: Box<Cron>, zone: Zone },
    Once(DateTime<Utc>),
}

impl Schedule {
    /// What croner would build from the pattern: an ISO timestamp is one moment, anything else a pattern.
    pub fn parse(schedule: &str, timezone: Option<&str>) -> Result<Self, String> {
        let zone = Zone::parse(timezone)?;
        if let Some(moment) = parse_iso_instant(schedule, Some(zone)) {
            if schedule.contains('-') {
                return Ok(Schedule::Once(moment));
            }
        }
        let cron = Cron::from_str(schedule.trim()).map_err(|error| error.to_string())?;
        Ok(Schedule::Cron { cron: Box::new(cron), zone })
    }

    /// The first firing strictly after `after`. A one-shot within a second past still counts, as croner's does.
    pub fn next_after(&self, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
        match self {
            Schedule::Once(moment) => (after.timestamp_millis() - moment.timestamp_millis() <= 1_000).then_some(*moment),
            Schedule::Cron { cron, zone } => match zone {
                Zone::Local => cron.find_next_occurrence(&after.with_timezone(&Local), false).ok().map(|time| time.with_timezone(&Utc)),
                Zone::Named(tz) => cron.find_next_occurrence(&after.with_timezone(tz), false).ok().map(|time| time.with_timezone(&Utc)),
            },
        }
    }

    pub fn is_once(&self) -> bool {
        matches!(self, Schedule::Once(_))
    }
}

/// The moment an ISO 8601 timestamp names, read the way croner reads one: with an offset it is
/// absolute, without one it is wall-clock time in the schedule's zone. A date written in the strict
/// `YYYY-MM-DD` form is UTC midnight, as `Date.parse` takes it. `zone` None checks the shape alone.
pub fn parse_iso_instant(text: &str, zone: Option<Zone>) -> Option<DateTime<Utc>> {
    let text = text.trim();
    let (date_part, rest) = match text.find(['T', 't', ' ']) {
        Some(index) => (&text[..index], Some(&text[index + 1..])),
        None => (text, None),
    };
    let mut fields = date_part.split('-');
    let year_text = fields.next()?;
    let month_text = fields.next()?;
    let day_text = fields.next()?;
    if fields.next().is_some() || year_text.len() != 4 || !(1..=2).contains(&month_text.len()) || !(1..=2).contains(&day_text.len()) {
        return None;
    }
    let date = NaiveDate::from_ymd_opt(number(year_text)?, number(month_text)? as u32, number(day_text)? as u32)?;
    let strict_date = month_text.len() == 2 && day_text.len() == 2;
    let zone = zone.unwrap_or(Zone::Local);

    let Some(rest) = rest else {
        if strict_date {
            let utc = Utc.from_utc_datetime(&date.and_time(NaiveTime::MIN));
            return match zone {
                Zone::Local => Some(utc),
                // Croner reads the local wall clock of that instant in the schedule's zone.
                Zone::Named(_) => zone.resolve(utc.with_timezone(&Local).naive_local()),
            };
        }
        return zone.resolve(date.and_time(NaiveTime::MIN));
    };

    let (clock, offset) = split_offset(rest)?;
    let time = parse_clock(clock)?;
    let wall = date.and_time(time);
    match offset {
        Some(seconds) => Some(Utc.from_utc_datetime(&(wall - chrono::Duration::seconds(seconds)))),
        None => zone.resolve(wall),
    }
}

fn number(text: &str) -> Option<i32> {
    (!text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit())).then(|| text.parse().ok()).flatten()
}

/// The clock and its offset from UTC in seconds, if one is written.
fn split_offset(rest: &str) -> Option<(&str, Option<i64>)> {
    if let Some(clock) = rest.strip_suffix(['Z', 'z']) {
        return Some((clock, Some(0)));
    }
    if let Some(index) = rest.rfind(['+', '-']) {
        let (clock, offset) = rest.split_at(index);
        let sign = if offset.starts_with('-') { -1 } else { 1 };
        let digits: String = offset[1..].chars().filter(|character| *character != ':').collect();
        if (digits.len() != 4 && digits.len() != 2) || !digits.is_ascii() {
            return None;
        }
        let hours = number(&digits[..2])? as i64;
        let minutes = if digits.len() == 4 { number(&digits[2..])? as i64 } else { 0 };
        return Some((clock, Some(sign * (hours * 3600 + minutes * 60))));
    }
    Some((rest, None))
}

fn parse_clock(clock: &str) -> Option<NaiveTime> {
    let (main, fraction) = match clock.split_once('.') {
        Some((main, fraction)) => (main, Some(fraction)),
        None => (clock, None),
    };
    let parts: Vec<&str> = main.split(':').collect();
    if !(2..=3).contains(&parts.len()) || parts.iter().any(|part| part.len() != 2) {
        return None;
    }
    let hour = number(parts[0])? as u32;
    let minute = number(parts[1])? as u32;
    let second = parts.get(2).map(|part| number(part)).unwrap_or(Some(0))? as u32;
    let millis = match fraction {
        Some(fraction) if !fraction.is_empty() && fraction.bytes().all(|byte| byte.is_ascii_digit()) => {
            let padded: String = fraction.chars().chain("000".chars()).take(3).collect();
            padded.parse().ok()?
        }
        Some(_) => return None,
        None => 0,
    };
    NaiveTime::from_hms_milli_opt(hour, minute, second, millis)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_iso_timestamps_with_and_without_offsets() {
        let utc = parse_iso_instant("2026-11-13T08:00:00.000Z", None).unwrap();
        assert_eq!(utc.to_rfc3339(), "2026-11-13T08:00:00+00:00");
        let offset = parse_iso_instant("2026-11-13T08:00+02:00", None).unwrap();
        assert_eq!(offset.to_rfc3339(), "2026-11-13T06:00:00+00:00");
        let tokyo = Zone::parse(Some("Asia/Tokyo")).unwrap();
        let wall = parse_iso_instant("2026-11-13 08:00", Some(tokyo)).unwrap();
        assert_eq!(wall.to_rfc3339(), "2026-11-12T23:00:00+00:00");
        assert!(parse_iso_instant("0 9 * * 1-5", None).is_none());
        assert!(parse_iso_instant("2026-13-01", None).is_none());
        assert!(parse_iso_instant("2026-11-13T08:00+a€", None).is_none());
    }

    #[test]
    fn a_pattern_fires_in_its_own_zone() {
        let schedule = Schedule::parse("0 8 * * *", Some("America/New_York")).unwrap();
        let after = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        assert_eq!(schedule.next_after(after).unwrap().to_rfc3339(), "2026-01-01T13:00:00+00:00");
    }

    #[test]
    fn a_one_shot_fires_once_and_tolerates_a_second_of_lateness() {
        let schedule = Schedule::parse("2026-01-01T00:00:00Z", None).unwrap();
        assert!(schedule.is_once());
        let moment = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        assert_eq!(schedule.next_after(moment - chrono::Duration::hours(1)), Some(moment));
        assert_eq!(schedule.next_after(moment + chrono::Duration::milliseconds(900)), Some(moment));
        assert_eq!(schedule.next_after(moment + chrono::Duration::seconds(2)), None);
    }

    #[test]
    fn refuses_a_bad_pattern_or_zone() {
        assert!(Schedule::parse("not a pattern", None).is_err());
        assert!(Schedule::parse("* * * * *", Some("Mars/Base")).is_err());
    }
}
