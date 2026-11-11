//! Everything the screens show, worked out from the stored rows.
//!
//! `Snapshot::build` is a pure function: rows in, numbers out. The database is
//! only touched by `Snapshot::load`, so the maths can be tested on its own.

use anyhow::Result;
use timesink_core::appmap::{AppMap, AppTotal};
use timesink_core::db::Db;
use timesink_core::domains::{self, DomainConfig};
use timesink_core::model::StoredInterval;
use timesink_core::stats::{self, Segment, Span};

use timesink_core::period::{self, Range};

/// Which period is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    /// Just today.
    Today,
    /// The last 7 days, ending today.
    Week,
    /// The last 30 days, ending today.
    Month,
    /// The last 365 days, ending today.
    Year,
    /// Everything recorded, from the first day with data until today.
    All,
    /// A calendar week, Monday to Sunday.
    CalWeek,
    /// A calendar quarter: Jan-Mar, Apr-Jun, Jul-Sep or Oct-Dec.
    Quarter,
    /// Days chosen by typing two dates (the `c` key).
    Custom,
}

impl View {
    /// All views, in tab order.
    pub const ALL: [View; 8] = [
        View::Today,
        View::Week,
        View::Month,
        View::Year,
        View::All,
        View::CalWeek,
        View::Quarter,
        View::Custom,
    ];

    /// Tab title.
    pub fn title(self) -> &'static str {
        match self {
            View::Today => "Today",
            View::Week => "7 days",
            View::Month => "30 days",
            View::Year => "Year",
            View::All => "All time",
            View::CalWeek => "Cal. week",
            View::Quarter => "Quarter",
            View::Custom => "Custom",
        }
    }

    /// How many days (including today) a rolling view covers. `None` for the views that are
    /// not a fixed-length window ending today: "All time" (it depends on when recording
    /// began) and the calendar views.
    pub fn days(self) -> Option<i64> {
        match self {
            View::Today => Some(1),
            View::Week => Some(7),
            View::Month => Some(30),
            View::Year => Some(365),
            View::All | View::CalWeek | View::Quarter | View::Custom => None,
        }
    }

    /// Does this view show months instead of a day-by-hour grid? (Too many days to list.)
    pub fn by_month(self) -> bool {
        matches!(self, View::Year | View::All | View::Quarter)
    }

    /// The days this view covers, `offset` periods back from the current one (0 = the
    /// current period, -1 = the one before, ...). `earliest_day` is the first recorded day,
    /// if any, which "All time" starts from. "All time" does not step. `custom` is the range
    /// typed for the Custom view (the last 30 days until one is chosen); stepping moves it
    /// by its own length.
    pub fn range(
        self,
        offset: i64,
        today: i64,
        earliest_day: Option<i64>,
        custom: Option<Range>,
    ) -> Range {
        match self {
            View::Today | View::Week | View::Month | View::Year => {
                let n = self.days().unwrap_or(1);
                Range::ending(today + offset * n, n)
            }
            View::All => Range::new(earliest_day.unwrap_or(today), today),
            View::CalWeek => period::calendar_week(today, offset),
            View::Quarter => period::calendar_quarter(today, offset),
            View::Custom => {
                let base = custom.unwrap_or_else(|| Range::ending(today, 30));
                let shift = offset * base.len();
                Range::new(base.first + shift, base.last + shift)
            }
        }
    }

    /// The offset after moving `delta` periods (-1 = back, 1 = forward) from `offset`, or
    /// `None` when there is nowhere to go: forward past the current period, back to a
    /// period that ends before the first recorded day, or any step in "All time".
    pub fn step(
        self,
        offset: i64,
        delta: i64,
        today: i64,
        earliest_day: Option<i64>,
        custom: Option<Range>,
    ) -> Option<i64> {
        let next = offset + delta;
        if self == View::All || next > 0 {
            return None;
        }
        let range = self.range(next, today, earliest_day, custom);
        match earliest_day {
            Some(first) if range.last < first => None,
            _ => Some(next),
        }
    }

    /// A short description of the days a view covers (`Q4 2026`, `Nov 3-9, 2026`).
    pub fn label(self, range: Range) -> String {
        match self {
            View::Quarter => period::quarter_name(range.first),
            _ => period::label(range),
        }
    }

    /// The next tab, or `None` at the last one (the tabs do not wrap round).
    pub fn next(self) -> Option<View> {
        let i = View::ALL.iter().position(|v| *v == self).unwrap_or(0);
        View::ALL.get(i + 1).copied()
    }

    /// The previous tab, or `None` at the first one (the tabs do not wrap round).
    pub fn prev(self) -> Option<View> {
        let i = View::ALL.iter().position(|v| *v == self).unwrap_or(0);
        i.checked_sub(1).map(|j| View::ALL[j])
    }
}

/// One calendar month's total, for the Year and All time views.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonthRow {
    /// Calendar year.
    pub year: i32,
    /// Month number, 1 to 12.
    pub month: u32,
    /// Active seconds in the month (within the period shown).
    pub total: i64,
    /// Days of the month inside the period, for the daily average.
    pub days: i64,
    /// Days that had any activity at all.
    pub active_days: i64,
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

/// Time on one website (from the optional browser extension).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteTotal {
    /// The registered site, e.g. `github.com`.
    pub site: String,
    /// Friendly name from `domains.conf` (the site itself when unlisted).
    pub display: String,
    /// Category from `domains.conf` (`Other` when unlisted).
    pub category: String,
    /// Seconds.
    pub secs: i64,
}

/// The stretches of time each site was in front of you: the site stretches reported by the
/// extension, cut down to the times the browser was the focused, non-idle window.
/// `sites` are (site, start, end) rows; `intervals` are the ordinary focus rows.
pub fn site_spans(
    sites: &[(String, i64, i64)],
    intervals: &[StoredInterval],
    config: &DomainConfig,
) -> Vec<Span> {
    let domain: Vec<Span> = sites
        .iter()
        .map(|(site, start, end)| Span {
            start: *start,
            end: *end,
            key: site.clone(),
        })
        .collect();
    let focus: Vec<Span> = intervals
        .iter()
        .filter(|i| config.is_browser(&i.app_class))
        .map(|i| Span {
            start: i.start_ts,
            end: i.end_ts,
            key: i.app_class.clone(),
        })
        .collect();
    domains::intersect(&domain, &focus)
}

/// Per-site totals (names and categories applied) for the stretches in `spans`, clipped to
/// `[from, to)`, biggest first.
pub fn site_totals(spans: &[Span], config: &DomainConfig, from: i64, to: i64) -> Vec<SiteTotal> {
    let clipped: Vec<Span> = spans
        .iter()
        .filter_map(|s| {
            let (start, end) = (s.start.max(from), s.end.min(to));
            (end > start).then(|| Span {
                start,
                end,
                key: s.key.clone(),
            })
        })
        .collect();
    domains::totals(&clipped)
        .into_iter()
        .map(|(site, secs)| {
            let info = config.sites.lookup(&site);
            SiteTotal {
                site,
                display: info.display,
                category: info.category,
                secs,
            }
        })
        .collect()
}

/// All numbers for one screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// The period shown.
    pub view: View,
    /// Local day number of today.
    pub today: i64,
    /// The days the numbers cover (the last one is usually today).
    pub range: Range,
    /// Total active seconds in the period.
    pub total: i64,
    /// Seconds per category, biggest first.
    pub categories: Vec<(String, i64)>,
    /// Seconds per application (display names), biggest first.
    pub apps: Vec<AppTotal>,
    /// One row per day, oldest first.
    pub days: Vec<DayRow>,
    /// One row per calendar month, oldest first (used by the Year and All time views).
    pub months: Vec<MonthRow>,
    /// Blocks of today (only filled for the Today view).
    pub timeline: Vec<TimelineBlock>,
    /// Seconds of listening per player, biggest first.
    pub listening: Vec<(String, i64)>,
    /// Total listening seconds in the period.
    pub listening_total: i64,
    /// Seconds since local midnight right now (for the "now" marker).
    pub now_secs: i64,
    /// Time on websites, biggest first. Empty unless the browser extension is in use.
    pub sites: Vec<SiteTotal>,
}

impl Snapshot {
    /// Does this screen sum up by month instead of listing days? The Year, All time and
    /// Quarter views always do; a custom range does once it is too long to list.
    pub fn by_month(&self) -> bool {
        self.view.by_month() || (self.view == View::Custom && self.range.len() > 31)
    }

    /// Does the period include today, so "now" markers and today's chart make sense?
    pub fn is_live(&self) -> bool {
        self.range.last == self.today
    }

    /// Fill in the website time for this snapshot's period (a no-op when the extension has
    /// recorded nothing). Kept apart from `load` so the app maps stay the only thing that
    /// function needs.
    pub fn attach_sites(
        &mut self,
        db: &Db,
        config: &DomainConfig,
        device: Option<&str>,
        now: i64,
        tz: &dyn Fn(i64) -> i64,
    ) -> Result<()> {
        let (from, to) = self.range.bounds(now, tz);
        let rows = db.domains_between(from, to, device)?;
        if rows.is_empty() {
            self.sites.clear();
            return Ok(());
        }
        let intervals = db.intervals_between(from, to, device)?;
        let spans = site_spans(&rows, &intervals, config);
        self.sites = site_totals(&spans, config, from, to);
        Ok(())
    }

    /// Read the rows for `view` over `range` from the database and work out the numbers.
    pub fn load(
        db: &Db,
        map: &AppMap,
        view: View,
        range: Range,
        device: Option<&str>,
        now: i64,
        tz: &dyn Fn(i64) -> i64,
    ) -> Result<Snapshot> {
        // Read from the start of the first day to the end of the last (or to now).
        let (from, to) = range.bounds(now, tz);
        let spans: Vec<Span> = db
            .intervals_between(from, to, device)?
            .into_iter()
            .map(|i| Span {
                start: i.start_ts,
                end: i.end_ts,
                key: i.app_class,
            })
            .collect();
        let listening: Vec<Span> = db
            .listening_between(from, to, device)?
            .into_iter()
            .map(|(player, start, end)| Span {
                start,
                end,
                key: player,
            })
            .collect();
        Ok(Snapshot::build_days(
            &spans, &listening, map, view, range, now, tz,
        ))
    }

    /// `load` for the view's usual period, the one that ends today ("All time" starts at
    /// the first recorded interval, or is just today if there is none).
    pub fn load_default(
        db: &Db,
        map: &AppMap,
        view: View,
        device: Option<&str>,
        now: i64,
        tz: &dyn Fn(i64) -> i64,
    ) -> Result<Snapshot> {
        let today = stats::local_day(now, tz);
        let earliest = db.earliest_start()?.map(|t| stats::local_day(t, tz));
        Snapshot::load(
            db,
            map,
            view,
            view.range(0, today, earliest, None),
            device,
            now,
            tz,
        )
    }

    /// Work out every number from the spans for the period of `view`.
    /// No database or clock involved.
    #[cfg(test)]
    pub fn build(
        spans: &[Span],
        listening: &[Span],
        map: &AppMap,
        view: View,
        now: i64,
        tz: &dyn Fn(i64) -> i64,
    ) -> Snapshot {
        let today = stats::local_day(now, tz);
        let range = match view {
            // The calendar views are their own stretch of days, not "N days ending today".
            View::CalWeek | View::Quarter => view.range(0, today, None, None),
            _ => Range::ending(today, view.days().unwrap_or(365)),
        };
        Snapshot::build_days(spans, listening, map, view, range, now, tz)
    }

    /// Like `build`, but for any run of days (used by exports and the screens).
    /// `view` only decides whether the day timeline is filled in.
    pub fn build_days(
        spans: &[Span],
        listening: &[Span],
        map: &AppMap,
        view: View,
        range: Range,
        now: i64,
        tz: &dyn Fn(i64) -> i64,
    ) -> Snapshot {
        let today = stats::local_day(now, tz);
        let (first, last) = (range.first, range.last);

        // Per-day raw app totals, the hour grid and the listening per day.
        let per_day = stats::per_day(spans, first, last, tz);
        let grid = stats::hour_grid(spans, first, last, tz);
        let listen_days = stats::per_day(listening, first, last, tz);

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

        let months = month_rows(&days);

        let timeline = if view == View::Today {
            stats::day_segments(spans, last, tz)
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
            range,
            total: days.iter().map(|d| d.total).sum(),
            categories,
            apps,
            days,
            months,
            timeline,
            listening: listening_by_player,
            listening_total,
            now_secs: (now + tz(now)).rem_euclid(86400),
            sites: Vec::new(),
        }
    }
}

/// Group day rows into calendar months (oldest first).
fn month_rows(days: &[DayRow]) -> Vec<MonthRow> {
    use chrono::Datelike;
    let mut out: Vec<MonthRow> = Vec::new();
    for d in days {
        // `day` is a local day number, so reading it as UTC gives the local calendar date.
        let Some(date) = chrono::DateTime::from_timestamp(d.day * 86400, 0) else {
            continue;
        };
        let (year, month) = (date.year(), date.month());
        match out.last_mut() {
            Some(m) if m.year == year && m.month == month => {
                m.total += d.total;
                m.days += 1;
                m.active_days += i64::from(d.total > 0);
            }
            _ => out.push(MonthRow {
                year,
                month,
                total: d.total,
                days: 1,
                active_days: i64::from(d.total > 0),
            }),
        }
    }
    out
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
        let snap = Snapshot::build_days(
            &old,
            &[],
            &map(),
            View::Month,
            Range::ending(now / 86400, 14),
            now,
            &utc,
        );
        assert_eq!(snap.days.len(), 14);
        assert_eq!(snap.total, 500);
        let snap = Snapshot::build_days(
            &old,
            &[],
            &map(),
            View::Month,
            Range::ending(now / 86400, 7),
            now,
            &utc,
        );
        assert_eq!(snap.total, 0);
    }

    #[test]
    fn tabs_step_one_at_a_time() {
        assert_eq!(View::Today.next(), Some(View::Week));
        assert_eq!(View::Month.next(), Some(View::Year));
        assert_eq!(View::All.next(), Some(View::CalWeek));
        assert_eq!(View::Quarter.next(), Some(View::Custom));
        assert_eq!(View::Custom.prev(), Some(View::Quarter));
    }

    #[test]
    fn tabs_stop_at_both_ends() {
        // Neither end wraps round to the other.
        assert_eq!(View::Custom.next(), None);
        assert_eq!(View::Today.prev(), None);
    }

    #[test]
    fn a_snapshot_remembers_the_days_it_covers() {
        let now = 100 * 86400 + 5000;
        let snap = Snapshot::build(&[], &[], &map(), View::Week, now, &utc);
        assert_eq!(
            snap.range,
            Range {
                first: 94,
                last: 100
            }
        );
        assert_eq!(snap.days.len(), 7);
        // A range that ended earlier is read as given, not as "ending today".
        let past =
            Snapshot::build_days(&[], &[], &map(), View::Week, Range::new(10, 12), now, &utc);
        assert_eq!(
            past.range,
            Range {
                first: 10,
                last: 12
            }
        );
        assert_eq!(
            past.days.iter().map(|d| d.day).collect::<Vec<_>>(),
            vec![10, 11, 12]
        );
        assert_eq!(past.today, 100);
        // The rolling windows end today; "All time" starts at the first recorded day.
        assert_eq!(
            View::Month.range(0, 100, None, None),
            Range {
                first: 71,
                last: 100
            }
        );
        assert_eq!(
            View::All.range(0, 100, Some(40), None),
            Range {
                first: 40,
                last: 100
            }
        );
        assert_eq!(
            View::All.range(0, 100, None, None),
            Range {
                first: 100,
                last: 100
            }
        );
    }

    #[test]
    fn tabs_walk_through_every_view_and_back() {
        let mut v = View::Today;
        let mut seen = vec![v];
        while let Some(n) = v.next() {
            seen.push(n);
            v = n;
        }
        assert_eq!(seen, View::ALL.to_vec());
        for view in View::ALL {
            if let Some(n) = view.next() {
                assert_eq!(n.prev(), Some(view));
            }
        }
    }

    #[test]
    fn views_step_back_a_period_at_a_time() {
        // Rolling windows move by their own length: the 7 days before the last 7.
        assert_eq!(
            View::Week.range(-1, 100, None, None),
            Range {
                first: 87,
                last: 93
            }
        );
        assert_eq!(
            View::Today.range(-1, 100, None, None),
            Range {
                first: 99,
                last: 99
            }
        );
        // Day 100 is a Saturday (1970-01-01 was a Thursday), so this calendar week is Mon 95 to today.
        assert_eq!(
            View::CalWeek.range(0, 100, None, None),
            Range {
                first: 95,
                last: 100
            }
        );
        assert_eq!(
            View::CalWeek.range(-1, 100, None, None),
            Range {
                first: 88,
                last: 94
            }
        );
        // Stepping goes back, never forward past the current period, and never into
        // periods that ended before the first recorded day.
        assert_eq!(View::Week.step(0, -1, 100, Some(50), None), Some(-1));
        assert_eq!(View::Week.step(-1, 1, 100, Some(50), None), Some(0));
        assert_eq!(View::Week.step(0, 1, 100, Some(50), None), None);
        assert_eq!(View::Week.step(-6, -1, 100, Some(60), None), None);
        assert_eq!(View::Week.step(-1, -1, 100, None, None), Some(-2));
        // "All time" is one fixed period.
        assert_eq!(View::All.step(0, -1, 100, Some(50), None), None);
    }

    #[test]
    fn the_custom_view_uses_the_chosen_days_and_steps_by_their_length() {
        let chosen = Range::new(60, 69);
        // Until a range is chosen it shows the last 30 days.
        assert_eq!(
            View::Custom.range(0, 100, None, None),
            Range {
                first: 71,
                last: 100
            }
        );
        assert_eq!(View::Custom.range(0, 100, None, Some(chosen)), chosen);
        // One step back moves a 10-day range by 10 days; forward is the range itself again.
        assert_eq!(
            View::Custom.range(-1, 100, None, Some(chosen)),
            Range::new(50, 59)
        );
        assert_eq!(
            View::Custom.step(0, -1, 100, Some(1), Some(chosen)),
            Some(-1)
        );
        // Not before the first recorded day.
        assert_eq!(View::Custom.step(0, -1, 100, Some(65), Some(chosen)), None);
        // Short custom ranges list their days, long ones sum by month.
        let snap = |r: Range| Snapshot::build_days(&[], &[], &map(), View::Custom, r, 0, &utc);
        assert!(!snap(Range::new(60, 90)).by_month());
        assert!(snap(Range::new(60, 91)).by_month());
        assert!(
            !Snapshot::build_days(&[], &[], &map(), View::Week, Range::new(1, 100), 0, &utc)
                .by_month()
        );
    }

    #[test]
    fn the_quarter_view_sums_by_month_and_is_named_by_quarter() {
        assert!(View::Quarter.by_month());
        assert!(!View::CalWeek.by_month());
        let today = period::day_of(chrono::NaiveDate::from_ymd_opt(2026, 11, 11).unwrap());
        let range = View::Quarter.range(0, today, None, None);
        assert_eq!(View::Quarter.label(range), "Q4 2026");
        assert_eq!(
            View::CalWeek.label(View::CalWeek.range(0, today, None, None)),
            "Nov 9-11, 2026"
        );
        // A past quarter reads as a whole quarter, and its snapshot lists those days only.
        let past = View::Quarter.range(-1, today, None, None);
        let snap = Snapshot::build_days(&[], &[], &map(), View::Quarter, past, 0, &utc);
        assert_eq!(snap.days.len() as i64, past.len());
        assert_eq!(past.len(), 92);
        assert!(!snap.is_live());
    }

    #[test]
    fn year_covers_365_days_and_sums_by_month() {
        // 2026-11-09 12:00 UTC; spans on 2026-11-01 (1h), 2026-10-31 (30m), 2025-12-15 (2h)
        // and one just outside the 365-day window.
        let now = 1_794_225_600;
        let day = |y: i32, m: u32, d: u32| {
            chrono::NaiveDate::from_ymd_opt(y, m, d)
                .unwrap()
                .and_hms_opt(10, 0, 0)
                .unwrap()
                .and_utc()
                .timestamp()
        };
        let spans = vec![
            span(day(2026, 11, 1), day(2026, 11, 1) + 3600, "zen"),
            span(day(2026, 10, 31), day(2026, 10, 31) + 1800, "zen"),
            span(day(2025, 12, 15), day(2025, 12, 15) + 7200, "zen"),
            span(day(2025, 11, 1), day(2025, 11, 1) + 600, "zen"),
        ];
        let snap = Snapshot::build_days(
            &spans,
            &[],
            &map(),
            View::Year,
            Range::ending(now / 86400, 365),
            now,
            &utc,
        );
        assert_eq!(snap.days.len(), 365);
        assert_eq!(snap.total, 3600 + 1800 + 7200);
        let months: Vec<(i32, u32, i64)> = snap
            .months
            .iter()
            .map(|m| (m.year, m.month, m.total))
            .collect();
        // Oldest first; Nov 2025 is outside the window apart from days in it, which have no data.
        assert_eq!(months.last(), Some(&(2026, 11, 3600)));
        assert!(months.contains(&(2026, 10, 1800)));
        assert!(months.contains(&(2025, 12, 7200)));
        // Months are in time order and the days add up to the total.
        assert!(months
            .windows(2)
            .all(|w| (w[0].0, w[0].1) < (w[1].0, w[1].1)));
        assert_eq!(snap.months.iter().map(|m| m.days).sum::<i64>(), 365);
        let dec = snap.months.iter().find(|m| m.month == 12 && m.year == 2025);
        assert_eq!(dec.map(|m| m.active_days), Some(1));
    }

    #[test]
    fn all_time_starts_at_the_first_recorded_day() {
        let db = Db::open_in_memory("box").unwrap();
        let now = 1_794_225_600; // 2026-11-09 12:00 UTC
                                 // Nothing recorded: just today, nothing to show.
        let empty = Snapshot::load_default(&db, &map(), View::All, None, now, &utc).unwrap();
        assert_eq!(empty.days.len(), 1);
        assert_eq!(empty.total, 0);
        // Data from 10 days ago: the view reaches back exactly that far.
        let start = now - 10 * 86400;
        db.insert_open(
            start,
            &timesink_core::model::WindowSnapshot {
                app_class: "zen".into(),
                title: String::new(),
                workspace: "1".into(),
            },
        )
        .unwrap();
        let snap = Snapshot::load_default(&db, &map(), View::All, None, now, &utc).unwrap();
        assert_eq!(snap.days.len(), 11);
        assert!(View::All.by_month() && View::Year.by_month() && !View::Month.by_month());
        assert_eq!(View::All.days(), None);
    }

    #[test]
    fn site_time_counts_only_while_the_browser_has_focus() {
        let config = DomainConfig::from_conf("browser = zen\ngithub.com github Research\n");
        let focus = |start, end, class: &str| StoredInterval {
            id: 1,
            device_id: "d".into(),
            start_ts: start,
            end_ts: end,
            app_class: class.into(),
            title: String::new(),
            workspace: "1".into(),
        };
        // github.com was the active tab for 0..100; zen was focused 10..40, the terminal
        // 40..60 (so those 20s must not count), zen again 60..70.
        let sites = vec![("github.com".to_string(), 0, 100)];
        let intervals = vec![
            focus(10, 40, "zen"),
            focus(40, 60, "wezterm"),
            focus(60, 70, "zen"),
        ];
        let spans = site_spans(&sites, &intervals, &config);
        let totals = site_totals(&spans, &config, 0, 1000);
        assert_eq!(totals.len(), 1);
        assert_eq!(totals[0].secs, 40);
        assert_eq!(totals[0].display, "github");
        assert_eq!(totals[0].category, "Research");
        // Clipping to a later window removes the earlier time.
        assert_eq!(site_totals(&spans, &config, 60, 1000)[0].secs, 10);
    }
}
