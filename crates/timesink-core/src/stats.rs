//! Numbers for charts: totals per day, an hour-by-day grid and a day timeline.
//!
//! Everything here is a pure function of the recorded spans and a timezone, so
//! it can be tested without a database or a terminal. The timezone is passed
//! in as a function from a UTC timestamp to its offset in seconds east of UTC
//! (negative in America/New_York), which keeps this crate free of timezone
//! databases and lets daylight-saving changes work.
//!
//! Days are identified by a *local day number*: whole days since 1970-01-01 on
//! the local calendar. Convert it to a date with `day * 86400` seconds.

use std::collections::HashMap;

/// A stretch of time attributed to one key (an app class, a player, ...).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    /// Start, UTC seconds.
    pub start: i64,
    /// End, UTC seconds.
    pub end: i64,
    /// What the time was spent on.
    pub key: String,
}

/// One slice of a span that falls inside a single local unit (hour or day).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Piece {
    /// Which unit it falls in (local hours or local days since 1970).
    unit: i64,
    /// Seconds from the start of that unit to the start of the piece.
    offset: i64,
    /// Length of the piece in seconds.
    len: i64,
}

/// Cut `[start, end)` at every local unit boundary (`unit_secs` = 3600 for
/// hours, 86400 for days). The offset is looked up at the start of each piece,
/// so a daylight-saving change is followed correctly.
fn pieces(start: i64, end: i64, unit_secs: i64, tz: &dyn Fn(i64) -> i64) -> Vec<Piece> {
    let mut out = Vec::new();
    let mut cursor = start;
    while cursor < end {
        let local = cursor + tz(cursor);
        let offset = local.rem_euclid(unit_secs);
        let len = (end - cursor).min(unit_secs - offset);
        out.push(Piece {
            unit: local.div_euclid(unit_secs),
            offset,
            len,
        });
        cursor += len;
    }
    out
}

/// The local day number that a UTC timestamp falls on.
pub fn local_day(ts: i64, tz: &dyn Fn(i64) -> i64) -> i64 {
    (ts + tz(ts)).div_euclid(86400)
}

/// UTC timestamp of the start (local midnight) of a local day.
pub fn day_start_utc(day: i64, tz: &dyn Fn(i64) -> i64) -> i64 {
    // Guess with the offset near midnight, then correct with the offset at the
    // guess, which fixes days where the clocks change.
    let rough = day * 86400 - tz(day * 86400);
    day * 86400 - tz(rough)
}

/// Total seconds per key for one local day.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DayTotals {
    /// Local day number.
    pub day: i64,
    /// (key, seconds), most first.
    pub by_key: Vec<(String, i64)>,
}

impl DayTotals {
    /// Sum of all keys.
    pub fn total(&self) -> i64 {
        self.by_key.iter().map(|(_, s)| s).sum()
    }
}

/// Totals per key for each local day from `first_day` to `last_day` inclusive.
/// Days without any time are still returned (empty), so charts have no holes.
pub fn per_day(
    spans: &[Span],
    first_day: i64,
    last_day: i64,
    tz: &dyn Fn(i64) -> i64,
) -> Vec<DayTotals> {
    let mut maps: Vec<HashMap<&str, i64>> =
        vec![HashMap::new(); (last_day - first_day + 1).max(0) as usize];
    for span in spans {
        for piece in pieces(span.start, span.end, 86400, tz) {
            if piece.unit < first_day || piece.unit > last_day {
                continue;
            }
            *maps[(piece.unit - first_day) as usize]
                .entry(span.key.as_str())
                .or_insert(0) += piece.len;
        }
    }
    maps.into_iter()
        .enumerate()
        .map(|(i, map)| {
            let mut by_key: Vec<(String, i64)> =
                map.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
            // Biggest first; ties by name so the order is stable.
            by_key.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            DayTotals {
                day: first_day + i as i64,
                by_key,
            }
        })
        .collect()
}

/// Seconds of activity in each local hour (0..24) for each day from
/// `first_day` to `last_day` inclusive. This is the data behind the heatmap.
pub fn hour_grid(
    spans: &[Span],
    first_day: i64,
    last_day: i64,
    tz: &dyn Fn(i64) -> i64,
) -> Vec<[i64; 24]> {
    let mut grid = vec![[0i64; 24]; (last_day - first_day + 1).max(0) as usize];
    for span in spans {
        for piece in pieces(span.start, span.end, 3600, tz) {
            let day = piece.unit.div_euclid(24);
            if day < first_day || day > last_day {
                continue;
            }
            grid[(day - first_day) as usize][piece.unit.rem_euclid(24) as usize] += piece.len;
        }
    }
    grid
}

/// One block on the timeline of a single day.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    /// Seconds after local midnight.
    pub start: i64,
    /// Seconds after local midnight (at most 86400).
    pub end: i64,
    /// What it was.
    pub key: String,
}

/// The blocks of one local day in time order. A span that crosses midnight is
/// cut at the day's edges.
pub fn day_segments(spans: &[Span], day: i64, tz: &dyn Fn(i64) -> i64) -> Vec<Segment> {
    let mut out = Vec::new();
    for span in spans {
        for piece in pieces(span.start, span.end, 86400, tz) {
            if piece.unit == day {
                out.push(Segment {
                    start: piece.offset,
                    end: piece.offset + piece.len,
                    key: span.key.clone(),
                });
            }
        }
    }
    out.sort_by_key(|s| s.start);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// UTC: no offset.
    fn utc(_: i64) -> i64 {
        0
    }

    /// A fixed zone five hours behind UTC (like New York in winter).
    fn minus5(_: i64) -> i64 {
        -5 * 3600
    }

    /// Shorthand span builder.
    fn span(start: i64, end: i64, key: &str) -> Span {
        Span {
            start,
            end,
            key: key.into(),
        }
    }

    #[test]
    fn a_span_inside_one_day_stays_whole() {
        let days = per_day(&[span(3600, 7200, "zen")], 0, 0, &utc);
        assert_eq!(days.len(), 1);
        assert_eq!(days[0].by_key, vec![("zen".to_string(), 3600)]);
        assert_eq!(days[0].total(), 3600);
    }

    #[test]
    fn a_span_over_midnight_is_split_between_days() {
        // 23:00 on day 0 to 01:00 on day 1, in UTC.
        let days = per_day(&[span(82800, 90000, "zen")], 0, 1, &utc);
        assert_eq!(days[0].total(), 3600);
        assert_eq!(days[1].total(), 3600);
    }

    #[test]
    fn midnight_follows_the_timezone() {
        // 03:00 UTC on day 1 is 22:00 on day 0 in a -5h zone.
        let ts = 86400 + 3 * 3600;
        assert_eq!(local_day(ts, &minus5), 0);
        assert_eq!(local_day(ts, &utc), 1);
        // Local midnight of day 1 is 05:00 UTC.
        assert_eq!(day_start_utc(1, &minus5), 86400 + 5 * 3600);
    }

    #[test]
    fn empty_days_are_kept_and_outside_days_ignored() {
        let days = per_day(
            &[span(100, 200, "a"), span(5 * 86400, 5 * 86400 + 50, "b")],
            0,
            2,
            &utc,
        );
        assert_eq!(days.len(), 3);
        assert_eq!(days[0].total(), 100);
        assert!(days[1].by_key.is_empty());
        assert!(days[2].by_key.is_empty());
    }

    #[test]
    fn keys_sort_biggest_first() {
        let days = per_day(&[span(0, 10, "a"), span(10, 60, "b")], 0, 0, &utc);
        assert_eq!(days[0].by_key[0].0, "b");
    }

    #[test]
    fn hour_grid_places_time_in_the_right_hours() {
        // 09:30 to 10:15 UTC on day 0 -> 30 min in hour 9, 15 min in hour 10.
        let grid = hour_grid(&[span(9 * 3600 + 1800, 10 * 3600 + 900, "x")], 0, 0, &utc);
        assert_eq!(grid[0][9], 1800);
        assert_eq!(grid[0][10], 900);
        assert_eq!(grid[0].iter().sum::<i64>(), 2700);
    }

    #[test]
    fn hour_grid_uses_local_hours() {
        // 14:00 UTC is 09:00 in a -5h zone.
        let grid = hour_grid(&[span(14 * 3600, 15 * 3600, "x")], 0, 0, &minus5);
        assert_eq!(grid[0][9], 3600);
    }

    #[test]
    fn segments_are_cut_at_the_day_edges_and_ordered() {
        // Spans given out of order; one crosses midnight into day 1.
        let spans = [span(80000, 90000, "late"), span(100, 200, "early")];
        let day0 = day_segments(&spans, 0, &utc);
        assert_eq!(day0.len(), 2);
        assert_eq!(day0[0].key, "early");
        assert_eq!(day0[1].end, 86400);
        let day1 = day_segments(&spans, 1, &utc);
        assert_eq!(day1.len(), 1);
        assert_eq!((day1[0].start, day1[0].end), (0, 3600));
    }

    #[test]
    fn totals_always_equal_input_time() {
        // Many odd spans: nothing may be lost or invented by the splitting.
        let mut spans = Vec::new();
        let mut t = 12_345;
        let mut expected = 0;
        for i in 0..200 {
            let len = 1 + (i * 7919) % 20_000;
            spans.push(span(t, t + len, "k"));
            expected += len;
            t += len + (i * 31) % 500;
        }
        let first = local_day(12_345, &minus5);
        let last = local_day(t, &minus5);
        let by_day: i64 = per_day(&spans, first, last, &minus5)
            .iter()
            .map(|d| d.total())
            .sum();
        let by_hour: i64 = hour_grid(&spans, first, last, &minus5)
            .iter()
            .map(|d| d.iter().sum::<i64>())
            .sum();
        assert_eq!(by_day, expected);
        assert_eq!(by_hour, expected);
    }
}
