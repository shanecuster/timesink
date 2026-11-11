//! Export reports as CSV, JSON, Markdown or HTML.
//!
//! `ReportData` holds everything an export needs, already worked out and
//! formatted. The four writers are pure functions from it to text, so they can
//! be tested without a database, a terminal or the clock.
//!
//! What is exported follows the same privacy rules as the screens: only app
//! names, categories and the cleaned detail the daemon stored (never page,
//! channel or note titles). CSV and JSON also carry every recorded row; the
//! Markdown and HTML reports are summaries.

use chrono::{DateTime, FixedOffset};
use serde_json::json;
use timesink_core::appmap::AppMap;
use timesink_core::domains::DomainConfig;
use timesink_core::model::StoredInterval;
use timesink_core::stats::{self, Span};

use crate::data::{self, Snapshot, View};
use crate::palette;
use crate::period::Range;
use crate::ui::fmt_duration;

/// The output formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// Comma-separated rows, opens in any spreadsheet.
    Csv,
    /// Everything in one JSON document.
    Json,
    /// A readable summary with tables.
    Markdown,
    /// A self-contained web page with bars.
    Html,
}

impl Format {
    /// File extension, without the dot.
    pub fn extension(self) -> &'static str {
        match self {
            Format::Csv => "csv",
            Format::Json => "json",
            Format::Markdown => "md",
            Format::Html => "html",
        }
    }

    /// Understand a name given on the command line (`csv`, `json`, `md`, ...).
    pub fn parse(text: &str) -> Option<Format> {
        match text.to_lowercase().as_str() {
            "csv" => Some(Format::Csv),
            "json" => Some(Format::Json),
            "md" | "markdown" => Some(Format::Markdown),
            "html" => Some(Format::Html),
            _ => None,
        }
    }

    /// Turn a report into text in this format.
    pub fn render(self, data: &ReportData) -> String {
        match self {
            Format::Csv => to_csv(data),
            Format::Json => to_json(data),
            Format::Markdown => to_markdown(data),
            Format::Html => to_html(data),
        }
    }
}

/// One recorded row, ready to write out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawRow {
    /// `focus` (a window had focus), `listening` (a player was playing) or `site` (a website
    /// was the active tab while the browser had focus).
    pub kind: &'static str,
    /// Start as local time, ISO 8601 with offset.
    pub start: String,
    /// End as local time, ISO 8601 with offset.
    pub end: String,
    /// Length in seconds.
    pub seconds: i64,
    /// Friendly name (or the player for listening rows).
    pub app: String,
    /// Category ("Listening" for listening rows).
    pub category: String,
    /// The raw window class as recorded.
    pub raw_class: String,
    /// The cleaned detail the daemon stored (program name, `ssh: label`, ...).
    pub detail: String,
}

/// One application line of the summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppLine {
    /// Friendly name.
    pub name: String,
    /// Its category.
    pub category: String,
    /// Seconds.
    pub secs: i64,
}

/// One website line of the summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteLine {
    /// The registered site, e.g. `github.com`.
    pub site: String,
    /// Friendly name from `domains.conf`.
    pub name: String,
    /// Category from `domains.conf`.
    pub category: String,
    /// Seconds.
    pub secs: i64,
}

/// One day of the summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DayLine {
    /// Local date, `YYYY-MM-DD`.
    pub date: String,
    /// Active seconds.
    pub active: i64,
    /// Listening seconds.
    pub listening: i64,
}

/// Everything an export needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportData {
    /// When the report was made, local ISO 8601.
    pub generated: String,
    /// `today`, `last 7 days`, ...
    pub label: String,
    /// First local date covered.
    pub first_date: String,
    /// Last local date covered.
    pub last_date: String,
    /// Total active seconds.
    pub total: i64,
    /// Seconds per category, biggest first.
    pub categories: Vec<(String, i64)>,
    /// Seconds per application, biggest first.
    pub apps: Vec<AppLine>,
    /// One entry per day, oldest first.
    pub days: Vec<DayLine>,
    /// Listening seconds per player, biggest first.
    pub listening: Vec<(String, i64)>,
    /// Total listening seconds.
    pub listening_total: i64,
    /// Every row in the period (clipped to it), oldest first.
    pub rows: Vec<RawRow>,
    /// Category names in config order, which decides their colours.
    pub category_names: Vec<String>,
    /// Seconds per website, biggest first (empty without the browser extension).
    pub sites: Vec<SiteLine>,
}

/// Name for a period of `days` days.
pub fn period_label(days: i64) -> String {
    match days {
        1 => "today".to_string(),
        n => format!("last {n} days"),
    }
}

/// Local ISO 8601 time (`2026-11-07T05:30:00-05:00`) for a UTC timestamp.
pub fn format_time(ts: i64, tz: &dyn Fn(i64) -> i64) -> String {
    let offset = FixedOffset::east_opt(tz(ts) as i32)
        .unwrap_or_else(|| FixedOffset::east_opt(0).expect("zero offset is valid"));
    DateTime::from_timestamp(ts, 0)
        .map(|d| {
            d.with_timezone(&offset)
                .format("%Y-%m-%dT%H:%M:%S%:z")
                .to_string()
        })
        .unwrap_or_default()
}

/// Calendar date (`YYYY-MM-DD`) of a local day number.
fn format_day(day: i64) -> String {
    DateTime::from_timestamp(day * 86400, 0)
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

impl ReportData {
    /// Work out a report for the last `days` days (including today) from the
    /// stored rows. Pure: rows and the time go in, a report comes out.
    pub fn build(
        intervals: &[StoredInterval],
        listening: &[(String, i64, i64)],
        map: &AppMap,
        days: i64,
        now: i64,
        tz: &dyn Fn(i64) -> i64,
    ) -> ReportData {
        let days = days.max(1);
        let today = stats::local_day(now, tz);
        let from = stats::day_start_utc(today - (days - 1), tz);

        // Spans for the totals (the maths clips them to the period).
        let spans: Vec<Span> = intervals
            .iter()
            .map(|i| Span {
                start: i.start_ts,
                end: i.end_ts,
                key: i.app_class.clone(),
            })
            .collect();
        let listen_spans: Vec<Span> = listening
            .iter()
            .map(|(player, start, end)| Span {
                start: *start,
                end: *end,
                key: player.clone(),
            })
            .collect();
        let range = Range::ending(today, days);
        let snap = Snapshot::build_days(&spans, &listen_spans, map, View::Month, range, now, tz);

        // Raw rows, clipped to the period, with their start time for sorting.
        let mut rows: Vec<(i64, RawRow)> = Vec::new();
        for i in intervals {
            let (start, end) = (i.start_ts.max(from), i.end_ts.min(now));
            if end <= start {
                continue;
            }
            let info = map.lookup(&i.app_class);
            rows.push((
                start,
                RawRow {
                    kind: "focus",
                    start: format_time(start, tz),
                    end: format_time(end, tz),
                    seconds: end - start,
                    app: info.display,
                    category: info.category,
                    raw_class: i.app_class.clone(),
                    detail: i.title.clone(),
                },
            ));
        }
        for (player, s, e) in listening {
            let (start, end) = (*s.max(&from), *e.min(&now));
            if end <= start {
                continue;
            }
            rows.push((
                start,
                RawRow {
                    kind: "listening",
                    start: format_time(start, tz),
                    end: format_time(end, tz),
                    seconds: end - start,
                    app: player.clone(),
                    category: "Listening".to_string(),
                    raw_class: player.clone(),
                    detail: String::new(),
                },
            ));
        }
        rows.sort_by_key(|a| a.0);

        ReportData {
            generated: format_time(now, tz),
            label: period_label(days),
            first_date: snap
                .days
                .first()
                .map(|d| format_day(d.day))
                .unwrap_or_default(),
            last_date: snap
                .days
                .last()
                .map(|d| format_day(d.day))
                .unwrap_or_default(),
            total: snap.total,
            categories: snap.categories,
            apps: snap
                .apps
                .into_iter()
                .map(|a| AppLine {
                    name: a.display,
                    category: a.category,
                    secs: a.secs,
                })
                .collect(),
            days: snap
                .days
                .iter()
                .map(|d| DayLine {
                    date: format_day(d.day),
                    active: d.total,
                    listening: d.listening,
                })
                .collect(),
            listening: snap.listening,
            listening_total: snap.listening_total,
            rows: rows.into_iter().map(|(_, r)| r).collect(),
            category_names: map.categories(),
            sites: Vec::new(),
        }
    }

    /// Add website time: the summary lines and one raw `site` row per stretch. `spans` are
    /// the site stretches already cut down to browser focus (see `data::site_spans`).
    pub fn with_sites(
        mut self,
        spans: &[Span],
        config: &DomainConfig,
        days: i64,
        now: i64,
        tz: &dyn Fn(i64) -> i64,
    ) -> ReportData {
        let today = stats::local_day(now, tz);
        let from = stats::day_start_utc(today - (days.max(1) - 1), tz);
        self.sites = data::site_totals(spans, config, from, now)
            .into_iter()
            .map(|t| SiteLine {
                site: t.site,
                name: t.display,
                category: t.category,
                secs: t.secs,
            })
            .collect();
        for s in spans {
            let (start, end) = (s.start.max(from), s.end.min(now));
            if end <= start {
                continue;
            }
            let info = config.sites.lookup(&s.key);
            self.rows.push(RawRow {
                kind: "site",
                start: format_time(start, tz),
                end: format_time(end, tz),
                seconds: end - start,
                app: info.display,
                category: info.category,
                raw_class: s.key.clone(),
                detail: String::new(),
            });
        }
        // Keep the rows in time order (the local ISO strings sort correctly).
        self.rows.sort_by(|a, b| a.start.cmp(&b.start));
        self
    }
}

// ---------------------------------------------------------------- CSV

/// Quote a CSV field when needed, and defuse spreadsheet formulas.
///
/// A field starting with `=`, `+`, `-`, `@`, a tab or a carriage return can be
/// run as a formula when the file is opened in a spreadsheet, so it gets a
/// leading apostrophe.
fn csv_field(text: &str) -> String {
    let mut field = text.to_string();
    if field.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        field.insert(0, '\'');
    }
    if field.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field
    }
}

/// One row per recorded interval, focus and listening together.
pub fn to_csv(data: &ReportData) -> String {
    let mut out = String::from("kind,start,end,seconds,app,category,raw_class,detail\n");
    for row in &data.rows {
        let fields = [
            csv_field(row.kind),
            csv_field(&row.start),
            csv_field(&row.end),
            row.seconds.to_string(),
            csv_field(&row.app),
            csv_field(&row.category),
            csv_field(&row.raw_class),
            csv_field(&row.detail),
        ];
        out.push_str(&fields.join(","));
        out.push('\n');
    }
    out
}

// --------------------------------------------------------------- JSON

/// The whole report, rows included, as pretty-printed JSON.
pub fn to_json(data: &ReportData) -> String {
    let value = json!({
        "generated": data.generated,
        "period": {
            "label": data.label,
            "from": data.first_date,
            "to": data.last_date,
        },
        "total_active_seconds": data.total,
        "categories": data.categories.iter()
            .map(|(name, secs)| json!({"name": name, "seconds": secs}))
            .collect::<Vec<_>>(),
        "apps": data.apps.iter()
            .map(|a| json!({"name": a.name, "category": a.category, "seconds": a.secs}))
            .collect::<Vec<_>>(),
        "sites": data.sites.iter()
            .map(|s| json!({"site": s.site, "name": s.name, "category": s.category, "seconds": s.secs}))
            .collect::<Vec<_>>(),
        "days": data.days.iter()
            .map(|d| json!({
                "date": d.date,
                "active_seconds": d.active,
                "listening_seconds": d.listening,
            }))
            .collect::<Vec<_>>(),
        "listening": {
            "total_seconds": data.listening_total,
            "players": data.listening.iter()
                .map(|(player, secs)| json!({"player": player, "seconds": secs}))
                .collect::<Vec<_>>(),
        },
        "intervals": data.rows.iter()
            .map(|r| json!({
                "kind": r.kind,
                "start": r.start,
                "end": r.end,
                "seconds": r.seconds,
                "app": r.app,
                "category": r.category,
                "raw_class": r.raw_class,
                "detail": r.detail,
            }))
            .collect::<Vec<_>>(),
    });
    // Pretty-printing a value built from plain data cannot fail.
    let mut text = serde_json::to_string_pretty(&value).unwrap_or_default();
    text.push('\n');
    text
}

// ----------------------------------------------------------- Markdown

/// Make text safe inside a Markdown table cell.
fn md_cell(text: &str) -> String {
    let mut out = String::new();
    for ch in text.chars() {
        match ch {
            '|' | '\\' | '`' | '*' | '_' => {
                out.push('\\');
                out.push(ch);
            }
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\n' | '\r' => out.push(' '),
            _ => out.push(ch),
        }
    }
    out
}

/// Percentage of `part` in `whole`, one decimal place.
fn share(part: i64, whole: i64) -> String {
    if whole <= 0 {
        "0.0%".to_string()
    } else {
        format!("{:.1}%", part as f64 * 100.0 / whole as f64)
    }
}

/// A readable summary with tables (no raw rows).
pub fn to_markdown(data: &ReportData) -> String {
    let mut out = String::new();
    out.push_str(&format!("# timesink report: {}\n\n", md_cell(&data.label)));
    out.push_str(&format!(
        "{} to {} · {} active · generated {}\n\n",
        data.first_date,
        data.last_date,
        fmt_duration(data.total),
        data.generated
    ));

    out.push_str("## Categories\n\n| Category | Time | Share |\n| --- | ---: | ---: |\n");
    for (name, secs) in &data.categories {
        out.push_str(&format!(
            "| {} | {} | {} |\n",
            md_cell(name),
            fmt_duration(*secs),
            share(*secs, data.total)
        ));
    }

    out.push_str("\n## Applications\n\n| Application | Category | Time | Share |\n| --- | --- | ---: | ---: |\n");
    for app in &data.apps {
        out.push_str(&format!(
            "| {} | {} | {} | {} |\n",
            md_cell(&app.name),
            md_cell(&app.category),
            fmt_duration(app.secs),
            share(app.secs, data.total)
        ));
    }

    if !data.sites.is_empty() {
        out.push_str("\n## Sites\n\nWebsite time from the browser extension (main site only), counted while the browser was in front.\n\n| Site | Category | Time |\n| --- | --- | ---: |\n");
        for site in &data.sites {
            out.push_str(&format!(
                "| {} | {} | {} |\n",
                md_cell(&site.name),
                md_cell(&site.category),
                fmt_duration(site.secs)
            ));
        }
    }

    out.push_str("\n## Days\n\n| Date | Active | Listening |\n| --- | ---: | ---: |\n");
    for day in &data.days {
        out.push_str(&format!(
            "| {} | {} | {} |\n",
            day.date,
            fmt_duration(day.active),
            fmt_duration(day.listening)
        ));
    }

    out.push_str(&format!(
        "\n## Listening\n\nListening overlaps active time, so it is not part of the totals above. Total: {}.\n\n",
        fmt_duration(data.listening_total)
    ));
    if data.listening.is_empty() {
        out.push_str("Nothing playing was recorded.\n");
    } else {
        out.push_str("| Player | Time |\n| --- | ---: |\n");
        for (player, secs) in &data.listening {
            out.push_str(&format!(
                "| {} | {} |\n",
                md_cell(player),
                fmt_duration(*secs)
            ));
        }
    }
    out
}

// --------------------------------------------------------------- HTML

/// Escape text for HTML. Names come from window classes, so this matters.
fn esc(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

/// One bar row: name, a coloured bar scaled against `max`, and the time.
fn html_bar(name: &str, secs: i64, max: i64, colour: &str) -> String {
    let pct = if max > 0 {
        (secs as f64 * 100.0 / max as f64).clamp(0.0, 100.0)
    } else {
        0.0
    };
    format!(
        "<div class=\"row\"><span class=\"name\">{}</span><span class=\"track\"><span class=\"bar\" style=\"width:{:.1}%;background:{}\"></span></span><span class=\"time\">{}</span></div>\n",
        esc(name),
        pct,
        colour,
        fmt_duration(secs)
    )
}

/// A self-contained page (no scripts, no outside files) with the summary.
pub fn to_html(data: &ReportData) -> String {
    let colour_of =
        |category: &str| palette::hex_for(data.category_names.iter().position(|c| c == category));
    let mut out = String::new();
    out.push_str("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n");
    out.push_str("<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; style-src 'unsafe-inline'\">\n");
    out.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    out.push_str(&format!(
        "<title>timesink report: {}</title>\n",
        esc(&data.label)
    ));
    out.push_str(
        "<style>\n\
         :root{color-scheme:light dark;--bg:#fff;--fg:#1c1e24;--muted:#6a6f7a;--track:#e6e8ec;--line:#d8dbe0}\n\
         @media (prefers-color-scheme:dark){:root{--bg:#22252b;--fg:#e6e8ec;--muted:#9aa0ab;--track:#343841;--line:#3b404a}}\n\
         body{font:15px/1.5 system-ui,sans-serif;background:var(--bg);color:var(--fg);max-width:880px;margin:2rem auto;padding:0 1rem}\n\
         h1{margin-bottom:.2rem}h2{margin-top:2rem;border-bottom:1px solid var(--line);padding-bottom:.2rem}\n\
         .meta{color:var(--muted)}\n\
         .row{display:grid;grid-template-columns:11rem 1fr 6rem;gap:.6rem;align-items:center;margin:.25rem 0}\n\
         .name{overflow:hidden;text-overflow:ellipsis;white-space:nowrap}\n\
         .track{background:var(--track);height:.9rem;border-radius:.45rem;overflow:hidden;display:block}\n\
         .bar{display:block;height:100%;border-radius:.45rem}\n\
         .time{text-align:right;font-variant-numeric:tabular-nums}\n\
         table{border-collapse:collapse;width:100%}th,td{padding:.25rem .5rem;border-bottom:1px solid var(--line)}\n\
         th{text-align:left}td.n,th.n{text-align:right;font-variant-numeric:tabular-nums}\n\
         </style>\n</head>\n<body>\n",
    );
    out.push_str(&format!("<h1>timesink report: {}</h1>\n", esc(&data.label)));
    out.push_str(&format!(
        "<p class=\"meta\">{} to {} · {} active · generated {}</p>\n",
        esc(&data.first_date),
        esc(&data.last_date),
        fmt_duration(data.total),
        esc(&data.generated)
    ));

    out.push_str("<h2>Categories</h2>\n");
    let max = data.categories.iter().map(|c| c.1).max().unwrap_or(0);
    for (name, secs) in &data.categories {
        out.push_str(&html_bar(name, *secs, max, &colour_of(name)));
    }

    out.push_str("<h2>Applications</h2>\n");
    let max = data.apps.iter().map(|a| a.secs).max().unwrap_or(0);
    for app in &data.apps {
        out.push_str(&html_bar(
            &app.name,
            app.secs,
            max,
            &colour_of(&app.category),
        ));
    }

    if !data.sites.is_empty() {
        out.push_str("<h2>Sites</h2>\n");
        let max = data.sites.iter().map(|s| s.secs).max().unwrap_or(0);
        for site in &data.sites {
            out.push_str(&html_bar(
                &site.name,
                site.secs,
                max,
                &palette::hex_for(None),
            ));
        }
    }

    out.push_str("<h2>Days</h2>\n<table>\n<tr><th>Date</th><th class=\"n\">Active</th><th class=\"n\">Listening</th></tr>\n");
    for day in &data.days {
        out.push_str(&format!(
            "<tr><td>{}</td><td class=\"n\">{}</td><td class=\"n\">{}</td></tr>\n",
            esc(&day.date),
            fmt_duration(day.active),
            fmt_duration(day.listening)
        ));
    }
    out.push_str("</table>\n");

    out.push_str(&format!(
        "<h2>Listening</h2>\n<p class=\"meta\">Listening overlaps active time, so it is not part of the totals above. Total: {}.</p>\n",
        fmt_duration(data.listening_total)
    ));
    let max = data.listening.iter().map(|l| l.1).max().unwrap_or(0);
    for (player, secs) in &data.listening {
        out.push_str(&html_bar(player, *secs, max, &palette::hex_for(None)));
    }
    out.push_str("</body>\n</html>\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Five hours behind UTC, like New York in winter.
    fn minus5(_: i64) -> i64 {
        -5 * 3600
    }

    /// A map with a couple of categories.
    fn map() -> AppMap {
        AppMap::from_conf("zen zen Browser\nwezterm wezterm Terminal\n")
    }

    /// A stored focus row.
    fn interval(start: i64, end: i64, class: &str, title: &str) -> StoredInterval {
        StoredInterval {
            id: 1,
            device_id: "box".into(),
            start_ts: start,
            end_ts: end,
            app_class: class.into(),
            title: title.into(),
            workspace: "1".into(),
        }
    }

    /// "Now" is 15:00 local (20:00 UTC) on UTC day 100.
    const NOW: i64 = 100 * 86400 + 20 * 3600;

    /// A small, realistic report.
    fn sample() -> ReportData {
        let rows = [
            interval(NOW - 7200, NOW - 3600, "zen", ""),
            interval(NOW - 3600, NOW - 1800, "wezterm", "ssh: lab"),
            interval(NOW - 1800, NOW - 1700, "mystery.App", ""),
        ];
        let music = [("cider".to_string(), NOW - 7200, NOW - 5400)];
        ReportData::build(&rows, &music, &map(), 7, NOW, &minus5)
    }

    #[test]
    fn report_totals_labels_and_dates_are_right() {
        let r = sample();
        assert_eq!(r.label, "last 7 days");
        assert_eq!(r.total, 3600 + 1800 + 100);
        assert_eq!(r.categories[0], ("Browser".to_string(), 3600));
        assert_eq!(r.apps[0].name, "zen");
        assert_eq!(r.days.len(), 7);
        assert_eq!(r.listening_total, 1800);
        assert_eq!(r.first_date.len(), 10);
        assert!(r.generated.ends_with("-05:00"), "{}", r.generated);
        assert_eq!(
            r.category_names,
            vec!["Browser".to_string(), "Terminal".to_string()]
        );
    }

    #[test]
    fn rows_are_sorted_and_cover_focus_and_listening() {
        let r = sample();
        assert_eq!(r.rows.len(), 4);
        assert!(r.rows.windows(2).all(|w| w[0].start <= w[1].start));
        assert_eq!(r.rows.iter().filter(|x| x.kind == "listening").count(), 1);
        let ssh = r.rows.iter().find(|x| x.detail == "ssh: lab").unwrap();
        assert_eq!(ssh.app, "wezterm");
        assert_eq!(ssh.category, "Terminal");
        assert_eq!(ssh.seconds, 1800);
    }

    #[test]
    fn rows_are_clipped_to_the_period() {
        // A row that started 10 days ago and ends an hour ago: only the part
        // inside "today" is exported for a 1-day report.
        let rows = [interval(NOW - 10 * 86400, NOW - 3600, "zen", "")];
        let r = ReportData::build(&rows, &[], &map(), 1, NOW, &minus5);
        assert_eq!(r.rows.len(), 1);
        // Local midnight is 05:00 UTC; "now" is 20:00 UTC; row ends at 19:00 UTC.
        assert_eq!(r.rows[0].seconds, 14 * 3600);
        assert_eq!(r.total, 14 * 3600);
        assert_eq!(r.label, "today");
    }

    #[test]
    fn csv_has_a_header_and_one_line_per_row() {
        let csv = to_csv(&sample());
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(
            lines[0],
            "kind,start,end,seconds,app,category,raw_class,detail"
        );
        assert_eq!(lines.len(), 1 + 4);
        assert!(csv.contains("ssh: lab"));
    }

    #[test]
    fn csv_quotes_special_characters_and_defuses_formulas() {
        assert_eq!(csv_field("plain"), "plain");
        assert_eq!(csv_field("a,b"), "\"a,b\"");
        assert_eq!(csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(csv_field("two\nlines"), "\"two\nlines\"");
        // Formula starters are neutralised.
        assert_eq!(csv_field("=cmd()"), "'=cmd()");
        assert_eq!(csv_field("+1"), "'+1");
        assert_eq!(csv_field("@sum"), "'@sum");
        assert_eq!(csv_field("-5"), "'-5");
    }

    #[test]
    fn json_is_valid_and_complete() {
        let text = to_json(&sample());
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["period"]["label"], "last 7 days");
        assert_eq!(v["total_active_seconds"], 5500);
        assert_eq!(v["categories"][0]["name"], "Browser");
        assert_eq!(v["intervals"].as_array().unwrap().len(), 4);
        assert_eq!(v["listening"]["players"][0]["player"], "cider");
        assert_eq!(v["days"].as_array().unwrap().len(), 7);
        assert!(text.ends_with('\n'));
    }

    #[test]
    fn json_escapes_awkward_names() {
        let mut r = sample();
        r.apps[0].name = "quote\" and \\ backslash\nnewline".into();
        let v: serde_json::Value = serde_json::from_str(&to_json(&r)).unwrap();
        assert_eq!(v["apps"][0]["name"], "quote\" and \\ backslash\nnewline");
    }

    #[test]
    fn markdown_has_tables_and_escapes_pipes() {
        let mut r = sample();
        r.apps[0].name = "a|b*c".into();
        let md = to_markdown(&r);
        assert!(md.contains("# timesink report: last 7 days"));
        assert!(md.contains("## Categories"));
        assert!(md.contains("| Browser | 1h 00m |"));
        assert!(md.contains("a\\|b\\*c"));
        assert!(md.contains("not part of the totals"));
        // No raw rows in the summary.
        assert!(!md.contains("ssh: lab"));
    }

    #[test]
    fn html_is_escaped_self_contained_and_scriptless() {
        let mut r = sample();
        r.apps[0].name = "<script>alert(1)</script>".into();
        let html = to_html(&r);
        assert!(!html.contains("<script"), "{html}");
        assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(html.contains("Content-Security-Policy"));
        assert!(html.starts_with("<!DOCTYPE html>"));
        assert!(html.contains("last 7 days"));
        // Bars never exceed their track.
        for part in html.split("style=\"width:").skip(1) {
            let pct: f64 = part.split('%').next().unwrap().parse().unwrap();
            assert!((0.0..=100.0).contains(&pct));
        }
        // Category colours come from the shared palette in config order:
        // Browser is first (blue), Terminal second (orange).
        assert!(html.contains("#569cd6"));
        assert!(html.contains("#e69150"));
    }

    #[test]
    fn empty_reports_render_in_every_format() {
        let r = ReportData::build(&[], &[], &AppMap::default(), 7, NOW, &minus5);
        for format in [Format::Csv, Format::Json, Format::Markdown, Format::Html] {
            assert!(!format.render(&r).is_empty());
        }
        assert!(to_markdown(&r).contains("Nothing playing was recorded."));
    }

    #[test]
    fn formats_are_recognised_by_name() {
        assert_eq!(Format::parse("CSV"), Some(Format::Csv));
        assert_eq!(Format::parse("md"), Some(Format::Markdown));
        assert_eq!(Format::parse("markdown"), Some(Format::Markdown));
        assert_eq!(Format::parse("html"), Some(Format::Html));
        assert_eq!(Format::parse("pdf"), None);
        assert_eq!(Format::Markdown.extension(), "md");
    }

    #[test]
    fn times_follow_the_zone_offset() {
        // 20:00 UTC is 15:00 at -05:00.
        assert_eq!(format_time(NOW, &minus5), "1970-04-11T15:00:00-05:00");
        assert_eq!(format_time(NOW, &|_| 0), "1970-04-11T20:00:00+00:00");
    }

    /// The sample report plus an hour on one site and ten minutes on a hostile-looking one.
    fn with_sites() -> ReportData {
        let config = DomainConfig::from_conf("github.com github Research\n");
        let spans = vec![
            Span {
                start: NOW - 7200,
                end: NOW - 3600,
                key: "github.com".into(),
            },
            Span {
                start: NOW - 3000,
                end: NOW - 2400,
                key: "<b>x</b>.example".into(),
            },
        ];
        sample().with_sites(&spans, &config, 7, NOW, &minus5)
    }

    #[test]
    fn site_time_appears_in_every_format() {
        let r = with_sites();
        assert_eq!(r.sites[0].site, "github.com");
        assert_eq!(r.sites[0].secs, 3600);
        assert_eq!(r.rows.iter().filter(|x| x.kind == "site").count(), 2);
        assert!(r.rows.windows(2).all(|w| w[0].start <= w[1].start));

        assert!(to_csv(&r).contains("site,"));
        let v: serde_json::Value = serde_json::from_str(&to_json(&r)).unwrap();
        assert_eq!(v["sites"][0]["name"], "github");
        assert_eq!(v["sites"][0]["seconds"], 3600);
        assert!(to_markdown(&r).contains("## Sites"));
        let html = to_html(&r);
        assert!(html.contains("<h2>Sites</h2>"));
        // Site names come from outside, so they are escaped like everything else.
        assert!(!html.contains("<b>x</b>"));
    }

    #[test]
    fn without_site_time_there_is_no_sites_section() {
        let r = sample();
        assert!(r.sites.is_empty());
        assert!(!to_markdown(&r).contains("## Sites"));
        assert!(!to_html(&r).contains("<h2>Sites</h2>"));
        let v: serde_json::Value = serde_json::from_str(&to_json(&r)).unwrap();
        assert_eq!(v["sites"].as_array().unwrap().len(), 0);
    }
}
