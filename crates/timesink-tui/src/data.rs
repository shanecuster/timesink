//! Everything the screens show, worked out from the stored rows.
//!
//! `Snapshot::build` is a pure function: rows in, numbers out. The database is
//! only touched by `Snapshot::load`, so the maths can be tested on its own.

use anyhow::Result;
use timesink_core::appmap::{AppMap, AppTotal};
use timesink_core::db::Db;
use timesink_core::stats::{self, Segment, Span};

/// Which period is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    /// Just today.
    Today,
    /// The last 7 days, ending today.
    Week,
    /// The last 30 days, ending today.
    Month,
}

impl View {
    /// All views, in tab order.
    pub const ALL: [View; 3] = [View::Today, View::Week, View::Month];

    /// Tab title.
    pub fn title(self) -> &'static str {
        match self {
            View::Today => "Today",
            View::Week => "7 days",
            View::Month => "30 days",
        }
    }

    /// How many days (including today) the view covers.
    pub fn days(self) -> i64 {
        match self {
            View::Today => 1,
            View::Week => 7,
            View::Month => 30,
        }
    }

    /// The next tab, wrapping round.
    pub fn next(self) -> View {
        match self {
            View::Today => View::Week,
            View::Week => View::Month,
            View::Month => View::Today,
        }
    }

    /// The previous tab, wrapping round.
    pub fn prev(self) -> View {
        match self {
            View::Today => View::Month,
            View::Week => View::Today,
            View::Month => View::Week,
        }
    }
}

/// One row of the heatmap: a day with its hourly activity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DayRow {
    /// Local day number (days since 1970-01-01).
    pub day: i64,
    /// Total active seconds that day.
    pub total: i64,
    /// Seconds of media listening that day.
    pub listening: i64,
    /// Active seconds in each hour of the day.
    pub hours: [i64; 24],
}

/// A block of the day timeline with its category.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineBlock {
    /// Seconds after local midnight.
    pub start: i64,
    /// Seconds after local midnight.
    pub end: i64,
    /// Category the time belongs to.
    pub category: String,
}

/// All numbers for one screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// The period shown.
    pub view: View,
    /// Local day number of today.
    pub today: i64,
    /// Total active seconds in the period.
    pub total: i64,
    /// Seconds per category, biggest first.
    pub categories: Vec<(String, i64)>,
    /// Seconds per application (display names), biggest first.
    pub apps: Vec<AppTotal>,
    /// One row per day, oldest first.
    pub days: Vec<DayRow>,
    /// Blocks of today (only filled for the Today view).
    pub timeline: Vec<TimelineBlock>,
    /// Seconds of listening per player, biggest first.
    pub listening: Vec<(String, i64)>,
    /// Total listening seconds in the period.
    pub listening_total: i64,
    /// Seconds since local midnight right now (for the "now" marker).
    pub now_secs: i64,
}

impl Snapshot {
    /// Read the rows for `view` from the database and work out the numbers.
    pub fn load(
        db: &Db,
        map: &AppMap,
        view: View,
        now: i64,
        tz: &dyn Fn(i64) -> i64,
    ) -> Result<Snapshot> {
        let today = stats::local_day(now, tz);
        let first = today - (view.days() - 1);
        // Read from the start of the first day to now.
        let from = stats::day_start_utc(first, tz);
        let spans: Vec<Span> = db
            .intervals_between(from, now + 1)?
            .into_iter()
            .map(|i| Span {
                start: i.start_ts,
                end: i.end_ts,
                key: i.app_class,
            })
            .collect();
        let listening: Vec<Span> = db
            .listening_between(from, now + 1)?
            .into_iter()
            .map(|(player, start, end)| Span {
                start,
                end,
                key: player,
            })
            .collect();
        Ok(Snapshot::build(&spans, &listening, map, view, now, tz))
    }

    /// Work out every number from the spans for the period of `view`.
    /// No database or clock involved.
    pub fn build(
        spans: &[Span],
        listening: &[Span],
        map: &AppMap,
        view: View,
        now: i64,
        tz: &dyn Fn(i64) -> i64,
    ) -> Snapshot {
        Snapshot::build_days(spans, listening, map, view, view.days(), now, tz)
    }

    /// Like `build`, but for any number of days ending today (used by exports).
    /// `view` only decides whether the day timeline is filled in.
    pub fn build_days(
        spans: &[Span],
        listening: &[Span],
        map: &AppMap,
        view: View,
        days: i64,
        now: i64,
        tz: &dyn Fn(i64) -> i64,
    ) -> Snapshot {
        let today = stats::local_day(now, tz);
        let first = today - (days.max(1) - 1);

        // Per-day raw app totals, the hour grid and the listening per day.
        let per_day = stats::per_day(spans, first, today, tz);
        let grid = stats::hour_grid(spans, first, today, tz);
        let listen_days = stats::per_day(listening, first, today, tz);

        // Whole-period totals per raw app class, then names and categories.
        let mut raw: std::collections::HashMap<&str, i64> = std::collections::HashMap::new();
        for day in &per_day {
            for (key, secs) in &day.by_key {
                *raw.entry(key.as_str()).or_insert(0) += secs;
            }
        }
        let raw: Vec<(String, i64)> = raw.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
        let (apps, categories) = map.group(&raw);

        let days: Vec<DayRow> = per_day
            .iter()
            .zip(grid.iter())
            .zip(listen_days.iter())
            .map(|((d, hours), l)| DayRow {
                day: d.day,
                total: d.total(),
                listening: l.total(),
                hours: *hours,
            })
            .collect();

        // Listening per player over the whole period.
        let mut players: std::collections::HashMap<&str, i64> = std::collections::HashMap::new();
        for day in &listen_days {
            for (player, secs) in &day.by_key {
                *players.entry(player.as_str()).or_insert(0) += secs;
            }
        }
        let mut listening_by_player: Vec<(String, i64)> = players
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
        listening_by_player.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        let listening_total = listening_by_player.iter().map(|(_, s)| s).sum();

        let timeline = if view == View::Today {
            stats::day_segments(spans, today, tz)
                .into_iter()
                .map(|Segment { start, end, key }| TimelineBlock {
                    start,
                    end,
                    category: map.lookup(&key).category,
                })
                .collect()
        } else {
            Vec::new()
        };

        Snapshot {
            view,
            today,
            total: days.iter().map(|d| d.total).sum(),
            categories,
            apps,
            days,
            timeline,
            listening: listening_by_player,
            listening_total,
            now_secs: (now + tz(now)).rem_euclid(86400),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// UTC for the tests.
    fn utc(_: i64) -> i64 {
        0
    }

    /// A map like a real config.
    fn map() -> AppMap {
        AppMap::from_conf("zen zen Browser\nwezterm wezterm Terminal\n")
    }

    fn span(start: i64, end: i64, key: &str) -> Span {
        Span {
            start,
            end,
            key: key.into(),
        }
    }

    #[test]
    fn totals_names_and_categories_are_worked_out() {
        // "Now" is 10:00 on day 3.
        let now = 3 * 86400 + 10 * 3600;
        let spans = [
            span(3 * 86400 + 3600, 3 * 86400 + 7200, "zen"),
            span(3 * 86400 + 7200, 3 * 86400 + 7800, "wezterm"),
            span(3 * 86400 + 8000, 3 * 86400 + 8100, "mystery"),
        ];
        let snap = Snapshot::build(&spans, &[], &map(), View::Today, now, &utc);
        assert_eq!(snap.total, 3600 + 600 + 100);
        assert_eq!(snap.categories[0], ("Browser".to_string(), 3600));
        assert_eq!(snap.categories.last().unwrap().0, "Other");
        assert_eq!(snap.apps[0].display, "zen");
        assert_eq!(snap.days.len(), 1);
        assert_eq!(snap.now_secs, 10 * 3600);
        assert_eq!(snap.timeline.len(), 3);
        assert_eq!(snap.timeline[1].category, "Terminal");
    }

    #[test]
    fn week_has_seven_days_oldest_first_and_no_timeline() {
        let now = 20 * 86400 + 100;
        let snap = Snapshot::build(&[], &[], &map(), View::Week, now, &utc);
        assert_eq!(snap.days.len(), 7);
        assert_eq!(snap.days[0].day, 14);
        assert_eq!(snap.days[6].day, 20);
        assert!(snap.timeline.is_empty());
        assert_eq!(snap.total, 0);
    }

    #[test]
    fn time_before_the_period_is_ignored() {
        let now = 20 * 86400 + 100;
        // A row ten days ago does not count in the 7-day view...
        let old = [span(10 * 86400, 10 * 86400 + 500, "zen")];
        assert_eq!(
            Snapshot::build(&old, &[], &map(), View::Week, now, &utc).total,
            0
        );
        // ...but does in the 30-day view.
        assert_eq!(
            Snapshot::build(&old, &[], &map(), View::Month, now, &utc).total,
            500
        );
    }

    #[test]
    fn listening_is_kept_apart_from_the_totals() {
        let now = 5 * 86400 + 12 * 3600;
        let focus = [span(5 * 86400, 5 * 86400 + 100, "zen")];
        let music = [
            span(5 * 86400, 5 * 86400 + 600, "cider"),
            span(5 * 86400 + 700, 5 * 86400 + 760, "mpv"),
        ];
        let snap = Snapshot::build(&focus, &music, &map(), View::Today, now, &utc);
        assert_eq!(snap.total, 100);
        assert_eq!(snap.listening_total, 660);
        assert_eq!(snap.listening[0], ("cider".to_string(), 600));
        assert_eq!(snap.days[0].listening, 660);
    }

    #[test]
    fn any_number_of_days_can_be_built() {
        let now = 50 * 86400 + 100;
        let old = [span(40 * 86400, 40 * 86400 + 500, "zen")];
        // 10 days back is 11 days ago at most: inside 14 days, outside 7.
        let snap = Snapshot::build_days(&old, &[], &map(), View::Month, 14, now, &utc);
        assert_eq!(snap.days.len(), 14);
        assert_eq!(snap.total, 500);
        let snap = Snapshot::build_days(&old, &[], &map(), View::Month, 7, now, &utc);
        assert_eq!(snap.total, 0);
    }

    #[test]
    fn tabs_cycle_both_ways() {
        assert_eq!(View::Today.next(), View::Week);
        assert_eq!(View::Month.next(), View::Today);
        assert_eq!(View::Today.prev(), View::Month);
    }
}
