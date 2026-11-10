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
//!
//! On tall terminals the name is drawn large instead (the figlet font "big"), with the date
//! line under it and the chart beside the bottom of it.

use chrono::{DateTime, Datelike};

/// The compact banner is two rows tall.
pub const HEIGHT: u16 = 2;

/// Below this many terminal rows the banner hides itself so the charts keep their room.
pub const MIN_TERMINAL_HEIGHT: u16 = 24;

/// The large banner: six rows of title plus the date line.
pub const BIG_HEIGHT: u16 = 7;

/// Terminals at least this tall get the large banner; shorter ones get the compact one.
pub const BIG_MIN_TERMINAL_HEIGHT: u16 = 32;

/// The large title: "timesink" in the figlet font "big" (trailing blank rows removed).
/// Every row is at most 40 columns wide.
pub const BIG_TITLE: [&str; 6] = [
    r" _   _                     _       _    ",
    r"| | (_)                   (_)     | |   ",
    r"| |_ _ _ __ ___   ___  ___ _ _ __ | | __",
    r"| __| | '_ ` _ \ / _ \/ __| | '_ \| |/ /",
    r"| |_| | | | | | |  __/\__ \ | | | |   < ",
    r" \__|_|_| |_| |_|\___||___/_|_| |_|_|\_\",
];

/// Which banner to draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Size {
    /// Two rows: the spaced-out name and the date.
    Compact,
    /// Seven rows: the large title with the date under it.
    Big,
}

impl Size {
    /// The banner for a terminal this tall, or `None` when it is too short to spare the room.
    pub fn for_terminal(height: u16) -> Option<Size> {
        if height >= BIG_MIN_TERMINAL_HEIGHT {
            Some(Size::Big)
        } else if height >= MIN_TERMINAL_HEIGHT {
            Some(Size::Compact)
        } else {
            None
        }
    }

    /// Rows the banner takes.
    pub fn height(self) -> u16 {
        match self {
            Size::Compact => HEIGHT,
            Size::Big => BIG_HEIGHT,
        }
    }

    /// Width of the name as drawn (it is centred across the banner).
    pub fn title_width(self) -> u16 {
        match self {
            Size::Compact => NAME.chars().count() as u16,
            Size::Big => BIG_TITLE[0].chars().count() as u16,
        }
    }
}

/// Width kept clear on the left for the date line, which sits at the left edge of the
/// banner. The longest date ("Wednesday September 30 2026 · Week 40") fits with a space
/// either side.
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

/// The first column of a `content_width`-wide block centred in `width` columns.
pub fn centred_x(width: u16, content_width: u16) -> u16 {
    width.saturating_sub(content_width) / 2
}

/// The first column of the chart when it is `columns` wide and pushed against the right
/// edge, leaving a one-column margin.
pub fn chart_x(width: u16, columns: u16) -> u16 {
    width.saturating_sub(columns + 1)
}

/// How many terminal columns each hour gets in the chart: 2 when there is plenty of room,
/// 1 when it is tight, 0 when it does not fit. The chart sits at the right edge, so it
/// must stay clear of the centred name (with a two-column gap) and of the date line on
/// the left.
pub fn columns_per_hour(width: u16, title_width: u16) -> usize {
    let title_end = centred_x(width, title_width) + title_width;
    for per_hour in [2usize, 1] {
        let x = chart_x(width, 24 * per_hour as u16);
        if x >= title_end + 2 && x > LEFT_WIDTH {
            return per_hour;
        }
    }
    0
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
        let small = Size::Compact.title_width();
        let big = Size::Big.title_width();
        // The compact name is short, so the chart fits at two columns an hour on a wide
        // terminal, one on a medium one, and drops out on a narrow one.
        assert_eq!(columns_per_hour(160, small), 2);
        assert_eq!(columns_per_hour(100, small), 1);
        assert_eq!(columns_per_hour(60, small), 0);
        // The big title is centred and 40 wide, so the chart needs more room beside it.
        assert_eq!(columns_per_hour(160, big), 2);
        assert_eq!(columns_per_hour(120, big), 1);
        assert_eq!(columns_per_hour(90, big), 0);
        // Whatever it picks never overlaps the centred name or the date line.
        for width in 20..200u16 {
            for title in [small, big] {
                let per_hour = columns_per_hour(width, title) as u16;
                if per_hour > 0 {
                    let x = chart_x(width, 24 * per_hour);
                    assert!(x >= centred_x(width, title) + title + 2, "{width}");
                    assert!(x > LEFT_WIDTH, "{width}");
                    assert!(x + 24 * per_hour < width, "{width}");
                }
            }
        }
        assert_eq!(centred_x(120, 40), 40);
        assert_eq!(centred_x(10, 40), 0);
        assert_eq!(chart_x(120, 24), 95);
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

    #[test]
    fn banner_size_depends_on_terminal_height() {
        assert_eq!(Size::for_terminal(23), None);
        assert_eq!(Size::for_terminal(24), Some(Size::Compact));
        assert_eq!(Size::for_terminal(31), Some(Size::Compact));
        assert_eq!(Size::for_terminal(32), Some(Size::Big));
        assert_eq!(Size::Big.height(), BIG_HEIGHT);
        assert_eq!(Size::Big.title_width(), 40);
        assert_eq!(Size::Compact.title_width(), 15);
    }

    #[test]
    fn the_big_title_fits_its_reserved_width() {
        // Six rows plus the date line, every row the same width so the title stays aligned
        // when centred.
        assert_eq!(BIG_TITLE.len() as u16 + 1, BIG_HEIGHT);
        for row in BIG_TITLE {
            assert_eq!(row.chars().count(), 40, "{row}");
        }
        // The date line fits in the space kept clear for it.
        assert!(date_line(20_000).chars().count() < LEFT_WIDTH as usize);
    }
}
