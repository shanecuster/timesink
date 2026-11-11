//! Periods of days: which stretch of the calendar a screen or report covers.
//!
//! Everything here works in *local day numbers* (days since 1970-01-01 in the local time
//! zone, the same numbers `timesink_core::stats` uses), so none of it needs a database or
//! a clock; the caller passes today's day number in.

use crate::stats;
use chrono::{Datelike, NaiveDate};

/// Days from 0001-01-01 (the "common era" count chrono uses) to 1970-01-01.
const UNIX_DAY_FROM_CE: i64 = 719_163;

/// A run of whole local days, both ends included.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    /// The first local day.
    pub first: i64,
    /// The last local day (never before `first`).
    pub last: i64,
}

impl Range {
    /// A range from `first` to `last`; the two are swapped if given the wrong way round.
    pub fn new(first: i64, last: i64) -> Range {
        Range {
            first: first.min(last),
            last: first.max(last),
        }
    }

    /// The `days` days ending on `last` (at least one day).
    pub fn ending(last: i64, days: i64) -> Range {
        Range {
            first: last - (days.max(1) - 1),
            last,
        }
    }

    // A range always holds at least one day, so an `is_empty` method would be pointless.
    #[allow(clippy::len_without_is_empty)]
    /// How many days it covers.
    pub fn len(&self) -> i64 {
        self.last - self.first + 1
    }

    /// The UTC time span `[from, to)` to read from the database: from the start of the first
    /// day to the end of the last, but never past `now` (so a range that includes today stops
    /// at this moment).
    pub fn bounds(&self, now: i64, tz: &dyn Fn(i64) -> i64) -> (i64, i64) {
        let from = stats::day_start_utc(self.first, tz);
        let to = stats::day_start_utc(self.last + 1, tz).min(now + 1);
        (from, to.max(from))
    }
}

/// The calendar date of a local day number.
pub fn date_of(day: i64) -> Option<NaiveDate> {
    NaiveDate::from_num_days_from_ce_opt((day + UNIX_DAY_FROM_CE) as i32)
}

/// The local day number of a calendar date.
pub fn day_of(date: NaiveDate) -> i64 {
    i64::from(date.num_days_from_ce()) - UNIX_DAY_FROM_CE
}

/// Read a date written as `YYYY-MM-DD` (surrounding spaces are ignored) as a day number.
pub fn parse_date(text: &str) -> Option<i64> {
    NaiveDate::parse_from_str(text.trim(), "%Y-%m-%d")
        .ok()
        .map(day_of)
}

/// The calendar week (Monday to Sunday) `offset` weeks from the one containing `today`
/// (0 = this week, -1 = last week). The current week stops at today, so it never lists
/// days that have not happened yet.
pub fn calendar_week(today: i64, offset: i64) -> Range {
    // 1970-01-01 was a Thursday, so day 0 is 3 days after a Monday.
    let monday = today - (today + 3).rem_euclid(7) + 7 * offset;
    Range::new(monday, (monday + 6).min(today))
}

/// The calendar quarter (Jan-Mar, Apr-Jun, Jul-Sep, Oct-Dec) `offset` quarters from the one
/// containing `today`. The current quarter stops at today.
pub fn calendar_quarter(today: i64, offset: i64) -> Range {
    let Some(date) = date_of(today) else {
        return Range::new(today, today);
    };
    // Number the quarters in a row (year * 4 + quarter) so stepping crosses year ends.
    let number = i64::from(date.year()) * 4 + i64::from((date.month() - 1) / 3) + offset;
    let first = quarter_start(number);
    let last = quarter_start(number + 1) - 1;
    Range::new(first, last.min(today))
}

/// The first day of quarter `number` (see `calendar_quarter`).
fn quarter_start(number: i64) -> i64 {
    let (year, quarter) = (number.div_euclid(4), number.rem_euclid(4));
    NaiveDate::from_ymd_opt(year as i32, (quarter * 3 + 1) as u32, 1)
        .map(day_of)
        .unwrap_or(0)
}

/// The quarter a day falls in, written like `Q4 2026`.
pub fn quarter_name(day: i64) -> String {
    match date_of(day) {
        Some(d) => format!("Q{} {}", (d.month() - 1) / 3 + 1, d.year()),
        None => String::new(),
    }
}

/// A short description of a run of days: `Tue Nov 10, 2026`, `Nov 3-9, 2026`,
/// `Oct 30 - Nov 5, 2026` or `Dec 28, 2025 - Jan 3, 2026`.
pub fn label(range: Range) -> String {
    let (Some(a), Some(b)) = (date_of(range.first), date_of(range.last)) else {
        return String::new();
    };
    if a == b {
        a.format("%a %b %-d, %Y").to_string()
    } else if a.year() == b.year() && a.month() == b.month() {
        format!("{}-{}, {}", a.format("%b %-d"), b.day(), b.year())
    } else if a.year() == b.year() {
        format!(
            "{} - {}, {}",
            a.format("%b %-d"),
            b.format("%b %-d"),
            b.year()
        )
    } else {
        format!("{} - {}", a.format("%b %-d, %Y"), b.format("%b %-d, %Y"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(_: i64) -> i64 {
        0
    }

    #[test]
    fn day_numbers_and_dates_round_trip() {
        let date = NaiveDate::from_ymd_opt(2026, 11, 11).unwrap();
        assert_eq!(date_of(day_of(date)), Some(date));
        assert_eq!(day_of(NaiveDate::from_ymd_opt(1970, 1, 1).unwrap()), 0);
        assert_eq!(parse_date("2026-11-11"), Some(day_of(date)));
        assert_eq!(parse_date(" 2026-11-11 "), Some(day_of(date)));
        assert_eq!(parse_date("2026-13-01"), None);
        assert_eq!(parse_date("11/11/2026"), None);
    }

    /// The local day number of a date, for the tests below.
    fn d(y: i32, m: u32, day: u32) -> i64 {
        day_of(NaiveDate::from_ymd_opt(y, m, day).unwrap())
    }

    #[test]
    fn calendar_weeks_run_monday_to_sunday_and_stop_at_today() {
        // Wednesday 2026-11-11: this week is Mon 9th to today.
        let today = d(2026, 11, 11);
        assert_eq!(calendar_week(today, 0), Range::new(d(2026, 11, 9), today));
        // Last week is the whole of Mon 2nd to Sun 8th.
        assert_eq!(
            calendar_week(today, -1),
            Range::new(d(2026, 11, 2), d(2026, 11, 8))
        );
        // On a Monday the week is just that day; on a Sunday it is complete.
        assert_eq!(calendar_week(d(2026, 11, 9), 0).len(), 1);
        assert_eq!(calendar_week(d(2026, 11, 15), 0).len(), 7);
        // Weeks can cross a month and a year.
        assert_eq!(
            calendar_week(d(2026, 1, 2), -1),
            Range::new(d(2025, 12, 22), d(2025, 12, 28))
        );
    }

    #[test]
    fn calendar_quarters_step_across_year_ends_and_stop_at_today() {
        let today = d(2026, 11, 11);
        assert_eq!(
            calendar_quarter(today, 0),
            Range::new(d(2026, 10, 1), today)
        );
        assert_eq!(
            calendar_quarter(today, -1),
            Range::new(d(2026, 7, 1), d(2026, 9, 30))
        );
        assert_eq!(
            calendar_quarter(today, -2),
            Range::new(d(2026, 4, 1), d(2026, 6, 30))
        );
        // Four steps back from Q4 is Q4 of the year before, and a leap year has a 91-day Q1.
        assert_eq!(
            calendar_quarter(today, -4),
            Range::new(d(2025, 10, 1), d(2025, 12, 31))
        );
        assert_eq!(calendar_quarter(d(2028, 2, 10), 0).first, d(2028, 1, 1));
        assert_eq!(calendar_quarter(d(2028, 4, 1), -1).len(), 91);
        assert_eq!(quarter_name(today), "Q4 2026");
        assert_eq!(quarter_name(d(2026, 1, 1)), "Q1 2026");
    }

    #[test]
    fn labels_are_short_and_readable() {
        assert_eq!(
            label(Range::new(d(2026, 11, 10), d(2026, 11, 10))),
            "Tue Nov 10, 2026"
        );
        assert_eq!(
            label(Range::new(d(2026, 11, 3), d(2026, 11, 9))),
            "Nov 3-9, 2026"
        );
        assert_eq!(
            label(Range::new(d(2026, 10, 30), d(2026, 11, 5))),
            "Oct 30 - Nov 5, 2026"
        );
        assert_eq!(
            label(Range::new(d(2025, 12, 28), d(2026, 1, 3))),
            "Dec 28, 2025 - Jan 3, 2026"
        );
    }

    #[test]
    fn ranges_count_their_days_and_fix_their_order() {
        assert_eq!(Range::ending(10, 7), Range { first: 4, last: 10 });
        // At least one day, even for a nonsense count.
        assert_eq!(
            Range::ending(10, 0),
            Range {
                first: 10,
                last: 10
            }
        );
        assert_eq!(Range::new(9, 3), Range { first: 3, last: 9 });
    }

    #[test]
    fn bounds_stop_at_now_and_at_the_end_of_a_past_range() {
        let r = Range::new(10, 12);
        // A range in the past covers whole days.
        assert_eq!(r.bounds(100 * 86400, &utc), (10 * 86400, 13 * 86400));
        // One that includes "now" stops one second after it.
        let now = 12 * 86400 + 500;
        assert_eq!(r.bounds(now, &utc), (10 * 86400, now + 1));
        // A range wholly in the future is empty, never reversed.
        let (from, to) = Range::new(50, 52).bounds(86400, &utc);
        assert!(to >= from);
    }
}
