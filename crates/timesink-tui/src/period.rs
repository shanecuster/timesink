//! Periods of days: which stretch of the calendar a screen or report covers.
//!
//! Everything here works in *local day numbers* (days since 1970-01-01 in the local time
//! zone, the same numbers `timesink_core::stats` uses), so none of it needs a database or
//! a clock; the caller passes today's day number in.

use timesink_core::stats;

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

    /// The UTC time span `[from, to)` to read from the database: from the start of the first
    /// day to the end of the last, but never past `now` (so a range that includes today stops
    /// at this moment).
    pub fn bounds(&self, now: i64, tz: &dyn Fn(i64) -> i64) -> (i64, i64) {
        let from = stats::day_start_utc(self.first, tz);
        let to = stats::day_start_utc(self.last + 1, tz).min(now + 1);
        (from, to.max(from))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(_: i64) -> i64 {
        0
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
