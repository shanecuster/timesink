//! The title banner: the program name, today's date and week number, and a small chart of
//! how busy each hour of today was.
//!
//! This file only works out *what* to show (text and levels); `ui.rs` does the painting
//! with theme colours. Keeping them apart means the date and chart maths can be tested
//! without a screen.
//!
//! ```text
//!  t i m e s i n k                          ▁▁▂▅▇█▇▆▅▇█▆▃▂▁▁▁
//!  Saturday November 07 2026 · Week 45      00   06   12   18   24
//! ```

use chrono::{DateTime, Datelike};

/// The banner is two rows tall.
pub const HEIGHT: u16 = 2;

/// Below this many terminal rows the banner hides itself so the charts keep their room.
pub const MIN_TERMINAL_HEIGHT: u16 = 24;

/// Width reserved on the left for the name and the date line. The longest date
/// ("Wednesday September 30 2026 · Week 40") fits with a space either side.
pub const LEFT_WIDTH: u16 = 40;

/// The program name, spaced out so it reads as a title rather than a word.
pub const NAME: &str = "t i m e s i n k";

/// `Saturday November 07 2026 · Week 45` for a local day number (days since 1970-01-01).
///
/// The week is the ISO week (weeks start on Monday and week 1 holds the first Thursday),
/// which is what `%V` gives and what most calendars call "week number".
pub fn date_line(day: i64) -> String {
    match DateTime::from_timestamp(day * 86400, 0) {
        Some(d) => format!("{} · Week {}", d.format("%A %B %d %Y"), d.iso_week().week()),
        None => String::new(),
    }
}

/// How many terminal columns each hour gets in the chart for a given terminal width:
/// 2 when there is plenty of room, 1 when it is tight, 0 when the chart does not fit.
pub fn columns_per_hour(width: u16) -> usize {
    let free = width.saturating_sub(LEFT_WIDTH) as usize;
    if free > 24 * 2 {
        2
    } else if free > 24 {
        1
    } else {
        0
    }
}

/// The eight bar heights, shortest to tallest.
const LEVELS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// The bar glyph for an hour with `secs` of activity when the busiest hour had `max`.
///
/// An hour with no activity gets the shortest bar (the caller colours it as "empty");
/// any activity at all gets at least the second-shortest, so a quiet hour is still visible.
pub fn level_glyph(secs: i64, max: i64) -> char {
    if secs <= 0 || max <= 0 {
        return LEVELS[0];
    }
    // ceil(secs / max * 7) is 1..=7 for any non-zero amount: index 1 ('▂') up to 7 ('█').
    let steps = ((secs as f64 / max as f64) * 7.0).ceil() as usize;
    LEVELS[steps.clamp(1, 7)]
}

/// The row of hour labels under the chart: `00`, `06`, `12`, `18` at their hours and `24`
/// at the right-hand end. `per_hour` is the chart's columns per hour (1 or 2).
pub fn axis_line(per_hour: usize) -> String {
    let width = 24 * per_hour;
    let mut cells = vec![' '; width];
    // Write a two-digit label starting at a column, if it fits.
    let mut put = |col: usize, text: &str| {
        for (i, ch) in text.chars().enumerate() {
            if col + i < width {
                cells[col + i] = ch;
            }
        }
    };
    for h in [0usize, 6, 12, 18] {
        put(h * per_hour, &format!("{h:02}"));
    }
    // Right-align "24" so it ends where the chart ends.
    put(width.saturating_sub(2), "24");
    cells.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn date_line_has_weekday_month_and_iso_week() {
        // 2026-11-07 is a Saturday in ISO week 45. Day number = days since 1970-01-01.
        let day = chrono::NaiveDate::from_ymd_opt(2026, 11, 7)
            .unwrap()
            .signed_duration_since(chrono::NaiveDate::from_ymd_opt(1970, 1, 1).unwrap())
            .num_days();
        assert_eq!(date_line(day), "Saturday November 07 2026 · Week 45");
    }

    #[test]
    fn date_line_fits_the_reserved_width_even_for_the_longest_names() {
        // Scan a couple of years: no date line may overflow the left column.
        for day in 20000..20800 {
            let n = date_line(day).chars().count() as u16;
            assert!(n + 2 <= LEFT_WIDTH, "{} is {n} wide", date_line(day));
        }
    }

    #[test]
    fn iso_week_boundaries() {
        // 2026-12-31 (Thursday) is ISO week 53; 2027-01-01 (Friday) is still week 53.
        let d = |y, m, dd| {
            chrono::NaiveDate::from_ymd_opt(y, m, dd)
                .unwrap()
                .signed_duration_since(chrono::NaiveDate::from_ymd_opt(1970, 1, 1).unwrap())
                .num_days()
        };
        assert!(date_line(d(2026, 12, 31)).ends_with("Week 53"));
        assert!(date_line(d(2027, 1, 1)).ends_with("Week 53"));
        assert!(date_line(d(2027, 1, 4)).ends_with("Week 1"));
    }

    #[test]
    fn columns_depend_on_width() {
        assert_eq!(columns_per_hour(120), 2);
        assert_eq!(columns_per_hour(80), 1);
        assert_eq!(columns_per_hour(60), 0);
    }

    #[test]
    fn glyph_levels() {
        assert_eq!(level_glyph(0, 100), '▁');
        // Any activity is visible: never the empty bar.
        assert_eq!(level_glyph(1, 10_000), '▂');
        assert_eq!(level_glyph(100, 100), '█');
        // Half of the busiest hour sits in the middle of the scale.
        assert_eq!(level_glyph(50, 100), '▅');
        // Nothing recorded anywhere does not divide by zero.
        assert_eq!(level_glyph(0, 0), '▁');
    }

    #[test]
    fn axis_labels_line_up() {
        let two = axis_line(2);
        assert_eq!(two.chars().count(), 48);
        assert!(two.starts_with("00"));
        assert_eq!(&two[12..14], "06");
        assert_eq!(&two[24..26], "12");
        assert!(two.ends_with("24"));
        let one = axis_line(1);
        assert_eq!(one.chars().count(), 24);
        assert_eq!(&one[6..8], "06");
        assert!(one.ends_with("24"));
    }
}
