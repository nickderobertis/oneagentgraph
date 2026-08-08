//! The one place an envelope's `ts` is rendered.
//!
//! `docs/contract.md` fixes the format — RFC 3339, millisecond precision, UTC —
//! and merge order across streams is `(ts, stream, seq)`, so every producer in a
//! run has to spell the same instant the same way. That is a small enough job to
//! own rather than take a date library for, and owning it keeps the rendering
//! total: a `SystemTime` before the epoch cannot arise from
//! [`SystemTime::now`] on a sane host, and is rendered as the epoch rather than
//! panicking a run that had nothing else wrong with it.

use std::time::{SystemTime, UNIX_EPOCH};

/// Days in each month of a non-leap year, indexed from January.
const MONTH_DAYS: [u32; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];

/// Now, as the contract spells it.
#[must_use]
pub fn now_rfc3339() -> String {
    rfc3339(SystemTime::now())
}

/// Milliseconds since the Unix epoch, as the run record and the schedules count
/// them. Saturates at zero for the same reason [`rfc3339`] does.
#[must_use]
pub fn unix_millis(at: SystemTime) -> u64 {
    at.duration_since(UNIX_EPOCH).map_or(0, |since| {
        u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
    })
}

/// Render one instant as RFC 3339 with millisecond precision, in UTC.
#[must_use]
pub fn rfc3339(at: SystemTime) -> String {
    let millis = unix_millis(at);
    let (days, time_of_day) = (millis / 86_400_000, millis % 86_400_000);
    let (year, month, day) = civil_from_days(days);
    let (hour, minute, second, milli) = (
        time_of_day / 3_600_000,
        (time_of_day / 60_000) % 60,
        (time_of_day / 1_000) % 60,
        time_of_day % 1_000,
    );
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{milli:03}Z")
}

/// The civil date `days` after 1970-01-01, by the proleptic Gregorian calendar.
///
/// Walking years forward from the epoch rather than applying a closed-form
/// era formula: the inputs this crate renders are wall-clock instants a few
/// decades past the epoch, and a loop that is obviously right beats a constant
/// nobody on the team can check.
fn civil_from_days(days: u64) -> (u64, u32, u32) {
    let mut remaining = days;
    let mut year = 1970;
    loop {
        let in_year = if is_leap(year) { 366 } else { 365 };
        if remaining < in_year {
            break;
        }
        remaining -= in_year;
        year += 1;
    }
    let mut month = 0;
    loop {
        let in_month = u64::from(MONTH_DAYS[month]) + u64::from(month == 1 && is_leap(year));
        if remaining < in_month {
            break;
        }
        remaining -= in_month;
        month += 1;
    }
    let month = u32::try_from(month).unwrap_or(0) + 1;
    let day = u32::try_from(remaining).unwrap_or(0) + 1;
    (year, month, day)
}

/// Whether `year` is a Gregorian leap year.
fn is_leap(year: u64) -> bool {
    year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// One instant per boundary the rendering can get wrong: the epoch itself, a
    /// millisecond that has to keep its leading zeros, a leap day, the day after
    /// one, a year boundary, and an ordinary recent instant.
    #[test]
    fn renders_the_contract_s_timestamp_shape() {
        let at = |secs: u64, millis: u64| {
            UNIX_EPOCH + Duration::from_secs(secs) + Duration::from_millis(millis)
        };
        assert_eq!(rfc3339(UNIX_EPOCH), "1970-01-01T00:00:00.000Z");
        assert_eq!(rfc3339(at(0, 7)), "1970-01-01T00:00:00.007Z");
        assert_eq!(rfc3339(at(1_709_210_096, 789)), "2024-02-29T12:34:56.789Z");
        assert_eq!(rfc3339(at(1_709_251_200, 0)), "2024-03-01T00:00:00.000Z");
        assert_eq!(rfc3339(at(1_704_067_199, 999)), "2023-12-31T23:59:59.999Z");
        assert_eq!(rfc3339(at(1_786_169_542, 847)), "2026-08-08T06:12:22.847Z");
    }

    /// An instant before the epoch cannot come from `SystemTime::now`, but it can
    /// be constructed — and rendering it must degrade rather than panic a run
    /// that had nothing else wrong with it.
    #[test]
    fn a_pre_epoch_instant_renders_as_the_epoch() {
        let before = UNIX_EPOCH - Duration::from_secs(1);
        assert_eq!(unix_millis(before), 0);
        assert_eq!(rfc3339(before), "1970-01-01T00:00:00.000Z");
    }

    /// `now` is the caller every envelope uses, so it has to render the same
    /// shape — checked structurally rather than against a frozen instant.
    #[test]
    fn now_renders_the_same_shape() {
        let now = now_rfc3339();
        assert_eq!(now.len(), 24, "{now}");
        assert!(now.ends_with('Z') && now.as_bytes()[10] == b'T', "{now}");
        assert!(now.as_str() > "2020-01-01T00:00:00.000Z", "{now}");
    }
}
