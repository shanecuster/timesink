//! Drawing the screens with ratatui.
//!
//! `draw` only reads an `App` and paints a frame; it never touches the
//! database. That keeps it easy to test with ratatui's in-memory backend.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Tabs};
use ratatui::Frame;

use timesink_core::theme::{Rgb, Theme};

use crate::assign::{self, Assign};
use crate::banner;
use crate::data::{DayRow, Snapshot, View};

/// Everything the screen needs.
pub struct App {
    /// The numbers to show.
    pub snapshot: Snapshot,
    /// Categories named in the config, for stable colours.
    pub category_names: Vec<String>,
    /// How the lists are ordered.
    pub sort: SortMode,
    /// Is the export menu (choose a format) open?
    pub export_menu: bool,
    /// A short message about something that just happened (an export); it is
    /// shown in place of the key hints until the next key press.
    pub notice: Option<String>,
    /// Is the help overlay open?
    pub show_help: bool,
    /// A short message for the bottom line (e.g. a refresh error).
    pub status: Option<String>,
    /// The colours for text, borders and highlights (category colours stay fixed).
    pub theme: Theme,
    /// Is the title banner (name, date, day sparkline) shown?
    pub banner: bool,
    /// The application name / category editor (the `a` key).
    pub assign: Assign,
}

/// A theme colour as a terminal colour.
fn tc(c: Rgb) -> Color {
    Color::Rgb(c.0, c.1, c.2)
}

/// How the Categories and Applications lists are ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortMode {
    /// Most time first.
    Time,
    /// A to Z (with "Other" last).
    Name,
}

impl SortMode {
    /// The other mode.
    pub fn toggle(self) -> SortMode {
        match self {
            SortMode::Time => SortMode::Name,
            SortMode::Name => SortMode::Time,
        }
    }

    /// Word shown in the panel titles.
    pub fn label(self) -> &'static str {
        match self {
            SortMode::Time => "by time",
            SortMode::Name => "by name",
        }
    }
}

impl App {
    /// The applications in the order the Applications panel shows them.
    pub fn ordered_apps(&self) -> Vec<timesink_core::appmap::AppTotal> {
        let mut apps = self.snapshot.apps.clone();
        if self.sort == SortMode::Name {
            apps.sort_by_key(|a| a.display.to_lowercase());
        }
        apps
    }

    /// A bordered box in the theme's border colour with a title in the text colour.
    fn block<'a>(&self, title: impl Into<Line<'a>>) -> Block<'a> {
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(tc(self.theme.border)))
            .title(title)
    }

    /// Quiet text (hints, axis labels).
    fn muted(&self) -> Style {
        Style::default().fg(tc(self.theme.muted))
    }

    /// The colour of a category: its place in the config picks a slot in the current
    /// theme's own accent list, so it stays the same until the theme changes.
    pub fn color_of(&self, category: &str) -> Color {
        let index = self.category_names.iter().position(|c| c == category);
        tc(self.theme.category_color(index))
    }
}

/// Paint the whole screen.
pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    // Fill everything with the theme's background and text colour first; every widget
    // drawn after this inherits them wherever it does not set its own.
    frame.render_widget(
        Block::default().style(Style::default().bg(tc(app.theme.bg)).fg(tc(app.theme.fg))),
        area,
    );

    // The banner only shows when switched on and when the terminal is tall enough.
    let banner_h = if app.banner && area.height >= banner::MIN_TERMINAL_HEIGHT {
        banner::HEIGHT
    } else {
        0
    };
    let [banner_area, tabs_area, body, footer] = Layout::vertical([
        Constraint::Length(banner_h),
        Constraint::Length(3),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(area);

    if banner_h > 0 {
        draw_banner(frame, banner_area, app);
    }
    draw_tabs(frame, tabs_area, app);
    match app.snapshot.view {
        View::Today => draw_today(frame, body, app),
        View::Week | View::Month => draw_range(frame, body, app),
    }
    draw_footer(frame, footer, app);
    if app.show_help {
        draw_help(frame, area, app);
    }
    if app.assign != Assign::Closed {
        draw_assign(frame, area, app);
    }
}

/// The title banner: name and date on the left, today's hour-by-hour sparkline on the right.
fn draw_banner(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let snap = &app.snapshot;
    let [left, right] =
        Layout::horizontal([Constraint::Length(banner::LEFT_WIDTH), Constraint::Min(0)])
            .areas(area);

    // Left: the spaced-out name in the accent colour, the date line underneath.
    let name = Line::from(Span::styled(
        format!(" {}", banner::NAME),
        Style::default()
            .fg(tc(theme.accent))
            .add_modifier(Modifier::BOLD),
    ));
    let date = Line::styled(
        format!(" {}", banner::date_line(snap.today)),
        Style::default().fg(tc(theme.fg)),
    );
    frame.render_widget(Paragraph::new(vec![name, date]), left);

    // Right: the sparkline, if the terminal is wide enough for it.
    let per_hour = banner::columns_per_hour(area.width);
    if per_hour == 0 {
        return;
    }
    let hours = snap.days.last().map(|d| d.hours).unwrap_or([0; 24]);
    let max = hours.iter().copied().max().unwrap_or(0);
    let now_hour = (snap.now_secs / 3600).clamp(0, 23) as usize;
    let mut bars: Vec<Span> = Vec::with_capacity(24);
    for (h, secs) in hours.iter().enumerate() {
        let glyph = banner::level_glyph(*secs, max).to_string().repeat(per_hour);
        // Quiet hours use the border colour; busy ones fade from accent to banner_end
        // through the day, so the right-hand side of the chart looks warmer.
        let colour = if *secs > 0 {
            theme.accent.lerp(theme.banner_end, h as f32 / 23.0)
        } else {
            theme.border
        };
        let mut style = Style::default().fg(tc(colour));
        // Underline the current hour so "now" is easy to find (Today's numbers only).
        if h == now_hour {
            style = style.add_modifier(Modifier::UNDERLINED | Modifier::BOLD);
        }
        bars.push(Span::styled(glyph, style));
    }
    let axis = Line::styled(banner::axis_line(per_hour), app.muted());
    frame.render_widget(Paragraph::new(vec![Line::from(bars), axis]), right);
}

/// The tab bar with the period's total on the right.
fn draw_tabs(frame: &mut Frame, area: Rect, app: &App) {
    let titles: Vec<Line> = View::ALL.iter().map(|v| Line::from(v.title())).collect();
    let selected = View::ALL
        .iter()
        .position(|v| *v == app.snapshot.view)
        .unwrap_or(0);
    let title = format!(" timesink · {} active ", fmt_duration(app.snapshot.total));
    let tabs = Tabs::new(titles)
        .select(selected)
        .block(app.block(title))
        .highlight_style(
            Style::default()
                .fg(tc(app.theme.accent))
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        );
    frame.render_widget(tabs, area);
}

/// Today: summary panels on the left, hourly bars and timeline on the right.
fn draw_today(frame: &mut Frame, area: Rect, app: &App) {
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)]).areas(area);
    draw_summary(frame, left, app);

    let [hours, timeline, listening] = Layout::vertical([
        Constraint::Min(8),
        Constraint::Length(5),
        Constraint::Length(4),
    ])
    .areas(right);
    draw_hourly(frame, hours, app);
    draw_timeline(frame, timeline, app);
    draw_listening(frame, listening, app);
}

/// 7 or 30 days: the heatmap on the left, summaries on the right.
fn draw_range(frame: &mut Frame, area: Rect, app: &App) {
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(62), Constraint::Percentage(38)]).areas(area);
    let [map_area, listening_area] =
        Layout::vertical([Constraint::Min(6), Constraint::Length(4)]).areas(left);
    draw_heatmap(frame, map_area, app);
    draw_listening(frame, listening_area, app);
    draw_summary(frame, right, app);
}

/// Category bars above application bars, in the chosen order.
fn draw_summary(frame: &mut Frame, area: Rect, app: &App) {
    let snap = &app.snapshot;
    let [cat_area, app_area] = Layout::vertical([
        Constraint::Length((snap.categories.len() as u16 + 2).clamp(4, 12)),
        Constraint::Min(3),
    ])
    .areas(area);

    // Order the lists. The snapshot is already by time; by name is A to Z,
    // ignoring case, with "Other" kept last.
    let mut categories = snap.categories.clone();
    let apps = app.ordered_apps();
    if app.sort == SortMode::Name {
        categories.sort_by_key(|(name, _)| (name == "Other", name.to_lowercase()));
    }

    // Bars are scaled against the biggest value, wherever it is in the list.
    let max = categories.iter().map(|c| c.1).max().unwrap_or(1).max(1);
    let inner_w = cat_area.width.saturating_sub(2) as usize;
    let lines: Vec<Line> = if categories.is_empty() {
        vec![Line::styled("  nothing recorded yet", app.muted())]
    } else {
        categories
            .iter()
            .map(|(name, secs)| {
                bar_line(
                    name,
                    *secs,
                    max,
                    inner_w,
                    app.color_of(name),
                    12,
                    tc(app.theme.border),
                )
            })
            .collect()
    };
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" Categories · {} ", app.sort.label())),
        ),
        cat_area,
    );

    let max = apps.iter().map(|a| a.secs).max().unwrap_or(1).max(1);
    let inner_w = app_area.width.saturating_sub(2) as usize;
    let rows = app_area.height.saturating_sub(2) as usize;
    let lines: Vec<Line> = apps
        .iter()
        .take(rows)
        .map(|a| {
            bar_line(
                &a.display,
                a.secs,
                max,
                inner_w,
                app.color_of(&a.category),
                14,
                tc(app.theme.border),
            )
        })
        .collect();
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" Applications · {} ", app.sort.label())),
        ),
        app_area,
    );
}

/// One row: `name  ████████  1h 05m`. The bar fills whatever width is left.
fn bar_line(
    name: &str,
    secs: i64,
    max: i64,
    width: usize,
    color: Color,
    name_w: usize,
    track: Color,
) -> Line<'static> {
    let time = fmt_duration(secs);
    // " name  " + bar + " time"
    let fixed = name_w + 2 + time.chars().count() + 2;
    let bar_w = width.saturating_sub(fixed);
    let filled = ((secs as f64 / max as f64) * bar_w as f64).round() as usize;
    // A tiny but non-zero value still gets one block so it stays visible.
    let filled = if secs > 0 {
        filled.max(1).min(bar_w)
    } else {
        0
    };
    Line::from(vec![
        Span::raw(format!(" {:<w$} ", truncate(name, name_w), w = name_w)),
        Span::styled("█".repeat(filled), Style::default().fg(color)),
        Span::styled(
            "░".repeat(bar_w.saturating_sub(filled)),
            Style::default().fg(track),
        ),
        Span::raw(format!(" {time}")),
    ])
}

/// Vertical bars: active minutes in each hour of today.
fn draw_hourly(frame: &mut Frame, area: Rect, app: &App) {
    let hours = app.snapshot.days.last().map(|d| d.hours).unwrap_or([0; 24]);
    let inner_h = area.height.saturating_sub(3) as usize; // borders + hour labels
    let max = hours.iter().copied().max().unwrap_or(0).max(1);
    let bar_w = (area.width.saturating_sub(2) / 24).max(1) as usize;

    // Each hour column is `bar_w` wide; draw rows from the top down.
    let mut lines: Vec<Line> = Vec::new();
    for row in (0..inner_h).rev() {
        let mut spans = Vec::new();
        for secs in hours.iter() {
            let level = (*secs as f64 / max as f64) * inner_h as f64;
            let on = level > row as f64 || (*secs > 0 && row == 0);
            let glyph = if on { "█" } else { " " };
            spans.push(Span::styled(
                glyph.repeat(bar_w.saturating_sub(1).max(1)),
                Style::default().fg(tc(app.theme.accent)),
            ));
            spans.push(Span::raw(
                " ".repeat(bar_w.saturating_sub(bar_w.saturating_sub(1).max(1))),
            ));
        }
        lines.push(Line::from(spans));
    }
    // Hour numbers under the bars (every third hour when narrow).
    let mut labels = Vec::new();
    for h in 0..24 {
        let text = if bar_w >= 3 || h % 3 == 0 {
            format!("{:<w$}", h, w = bar_w)
        } else {
            " ".repeat(bar_w)
        };
        labels.push(Span::styled(text, app.muted()));
    }
    lines.push(Line::from(labels));
    frame.render_widget(
        Paragraph::new(lines).block(app.block(" Active time by hour ")),
        area,
    );
}

/// A strip for the whole day coloured by category, with a marker for "now".
fn draw_timeline(frame: &mut Frame, area: Rect, app: &App) {
    let snap = &app.snapshot;
    let width = area.width.saturating_sub(2) as usize;
    let mut cells: Vec<Span> = Vec::with_capacity(width);
    for col in 0..width {
        let from = col as i64 * 86400 / width.max(1) as i64;
        let to = ((col as i64 + 1) * 86400 / width.max(1) as i64).max(from + 1);
        // The category with the most time in this column wins it.
        let mut best: Option<(&str, i64)> = None;
        let mut by_cat: Vec<(&str, i64)> = Vec::new();
        for block in &snap.timeline {
            let overlap = block.end.min(to) - block.start.max(from);
            if overlap > 0 {
                match by_cat.iter_mut().find(|(c, _)| *c == block.category) {
                    Some((_, t)) => *t += overlap,
                    None => by_cat.push((block.category.as_str(), overlap)),
                }
            }
        }
        for (cat, t) in by_cat {
            if best.map(|b| t > b.1).unwrap_or(true) {
                best = Some((cat, t));
            }
        }
        let is_now = snap.now_secs >= from && snap.now_secs < to;
        cells.push(match best {
            Some((cat, _)) => Span::styled("█", Style::default().fg(app.color_of(cat))),
            None if is_now => Span::styled("│", Style::default().fg(tc(app.theme.fg))),
            None => Span::styled("·", Style::default().fg(tc(app.theme.border))),
        });
    }
    // Hour ticks every six hours.
    let mut ticks = vec![' '; width];
    for h in [0usize, 6, 12, 18] {
        let col = h * width / 24;
        let text = format!("{h:02}");
        for (i, ch) in text.chars().enumerate() {
            if col + i < width {
                ticks[col + i] = ch;
            }
        }
    }
    let tick_line: String = ticks.into_iter().collect();
    let lines = vec![Line::from(cells), Line::styled(tick_line, app.muted())];
    frame.render_widget(Paragraph::new(lines).block(app.block(" Timeline ")), area);
}

/// Listening time per player, shown beside (not inside) the totals.
fn draw_listening(frame: &mut Frame, area: Rect, app: &App) {
    let snap = &app.snapshot;
    let text = if snap.listening.is_empty() {
        "  nothing playing recorded".to_string()
    } else {
        let parts: Vec<String> = snap
            .listening
            .iter()
            .take(4)
            .map(|(p, s)| format!("{p} {}", fmt_duration(*s)))
            .collect();
        format!(
            "  {} total · {}",
            fmt_duration(snap.listening_total),
            parts.join(" · ")
        )
    };
    frame.render_widget(
        Paragraph::new(Line::styled(
            text,
            Style::default().fg(tc(app.theme.banner_end)),
        ))
        .block(app.block(" Listening (not in totals) ")),
        area,
    );
}

/// Days down the side, hours across, darker = less active.
fn draw_heatmap(frame: &mut Frame, area: Rect, app: &App) {
    let snap = &app.snapshot;
    let inner_h = area.height.saturating_sub(3) as usize; // borders + hour header
                                                          // If there is not room for every day, keep the most recent ones.
    let skip = snap.days.len().saturating_sub(inner_h);
    let busiest_hour = snap
        .days
        .iter()
        .flat_map(|d| d.hours.iter().copied())
        .max()
        .unwrap_or(0)
        .max(1);
    // Two characters per hour when there is room, otherwise one.
    let cell_w = if area.width >= 24 * 2 + 24 { 2 } else { 1 };

    // One label every three hours, padded to the width of three cells, so the
    // numbers stay lined up with their columns even when they have two digits.
    let mut header = vec![Span::styled("          ", Style::default())];
    for h in (0..24).step_by(3) {
        header.push(Span::styled(
            format!("{:<w$}", h, w = cell_w * 3),
            app.muted(),
        ));
    }
    let mut lines = vec![Line::from(header)];
    for (i, day) in snap.days.iter().enumerate().skip(skip) {
        lines.push(heat_row(
            app,
            day,
            busiest_hour,
            cell_w,
            day.day == snap.today,
            i,
        ));
    }
    frame.render_widget(
        Paragraph::new(lines).block(app.block(" Active time by day and hour ")),
        area,
    );
}

/// One heatmap row: date label, 24 coloured cells, daily total.
fn heat_row(
    app: &App,
    day: &DayRow,
    busiest: i64,
    cell_w: usize,
    is_today: bool,
    _index: usize,
) -> Line<'static> {
    let date = chrono::DateTime::from_timestamp(day.day * 86400, 0)
        .map(|d| d.format("%a %d").to_string())
        .unwrap_or_else(|| "?".to_string());
    let label_style = if is_today {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        app.muted()
    };
    let mut spans = vec![Span::styled(format!(" {date:<8} "), label_style)];
    for secs in day.hours.iter() {
        spans.push(Span::styled(
            "█".repeat(cell_w),
            Style::default().fg(heat_color(&app.theme, *secs, busiest)),
        ));
    }
    spans.push(Span::raw(format!(" {}", fmt_duration(day.total))));
    Line::from(spans)
}

/// From the theme's faint fill (no activity) to its bright "ok" colour (the busiest hour
/// on the screen).
fn heat_color(theme: &Theme, secs: i64, busiest: i64) -> Color {
    if secs <= 0 {
        return tc(theme.selection);
    }
    let t = (secs as f64 / busiest as f64).clamp(0.0, 1.0) as f32;
    // Never fully faint once there is any activity, so a quiet hour still shows up.
    tc(theme.selection.lerp(theme.ok, 0.25 + 0.75 * t))
}

/// The footer line: the export menu, an error, a notice, or the key hints.
fn draw_footer(frame: &mut Frame, area: Rect, app: &App) {
    let text = if app.export_menu {
        " Export this view as:  c csv · j json · m markdown · h html · Esc cancel".to_string()
    } else if let Some(msg) = &app.status {
        msg.clone()
    } else if let Some(msg) = &app.notice {
        msg.clone()
    } else {
        " ←/→ view · s sort · a apps · t theme · b banner · e export · r refresh · ? help · q quit"
            .to_string()
    };
    frame.render_widget(Paragraph::new(Line::styled(text, app.muted())), area);
}

/// A small box listing the keys.
fn draw_help(frame: &mut Frame, area: Rect, app: &App) {
    let w = 56.min(area.width);
    let h = 15.min(area.height);
    let rect = Rect::new(
        area.x + (area.width.saturating_sub(w)) / 2,
        area.y + (area.height.saturating_sub(h)) / 2,
        w,
        h,
    );
    let text = vec![
        Line::from(" ←/→, Tab, h/l   next / previous view"),
        Line::from(" 1  2  3         Today / 7 days / 30 days"),
        Line::from(" s               sort by time / by name"),
        Line::from(" a               rename an app / change its category"),
        Line::from(" t               next colour theme"),
        Line::from(" b               show / hide the title banner"),
        Line::from(" e               export this view (csv, json, md, html)"),
        Line::from(" r               refresh now"),
        Line::from(" ?               close this help"),
        Line::from(" q, Esc          quit"),
        Line::from(""),
        Line::styled(" Listening time overlaps focus time, so it is", app.muted()),
        Line::styled(" shown beside the totals, not inside.", app.muted()),
    ];
    frame.render_widget(ratatui::widgets::Clear, rect);
    frame.render_widget(Paragraph::new(text).block(app.block(" Help ")), rect);
}

/// The editor popup for application names and categories.
fn draw_assign(frame: &mut Frame, area: Rect, app: &App) {
    let w = 54.min(area.width);
    let h = 16.min(area.height);
    let rect = Rect::new(
        area.x + (area.width.saturating_sub(w)) / 2,
        area.y + (area.height.saturating_sub(h)) / 2,
        w,
        h,
    );
    let accent = Style::default()
        .fg(tc(app.theme.accent))
        .add_modifier(Modifier::BOLD);
    // How many list rows fit inside the box (borders and the hint line excluded).
    let room = h.saturating_sub(3).max(1) as usize;
    // A window of `room` rows around the highlighted one.
    let window = |len: usize, sel: usize| -> (usize, usize) {
        let start = sel.saturating_sub(room - 1).min(len.saturating_sub(room));
        (start, (start + room).min(len))
    };
    let (title, lines): (String, Vec<Line>) = match &app.assign {
        Assign::Closed => return,
        Assign::PickApp { list, sel } => {
            let (a, b) = window(list.len(), *sel);
            let mut lines: Vec<Line> = list[a..b]
                .iter()
                .enumerate()
                .map(|(i, t)| {
                    let row = format!(" {:<26} {}", truncate(&t.display, 26), t.category);
                    if a + i == *sel {
                        Line::styled(row, accent)
                    } else {
                        Line::from(row)
                    }
                })
                .collect();
            lines.push(Line::styled(
                " ↑/↓ choose · Enter select · Esc cancel",
                app.muted(),
            ));
            (" Edit which app? ".to_string(), lines)
        }
        Assign::Name { target, text } => (
            format!(" Short name for {} ", truncate(&target.display, 24)),
            vec![
                Line::from(""),
                Line::styled(format!(" {text}▏"), accent),
                Line::from(""),
                Line::styled(" Enter keep / confirm · Esc cancel", app.muted()),
            ],
        ),
        Assign::PickCategory { name, sel, .. } => {
            let rows = assign::category_rows(&app.category_names);
            let (a, b) = window(rows.len(), *sel);
            let mut lines: Vec<Line> = rows[a..b]
                .iter()
                .enumerate()
                .map(|(i, r)| {
                    if a + i == *sel {
                        Line::styled(format!(" ▸ {r}"), accent)
                    } else {
                        Line::from(format!("   {r}"))
                    }
                })
                .collect();
            lines.push(Line::styled(
                " ↑/↓ choose · Enter select · Esc cancel",
                app.muted(),
            ));
            (format!(" Category for {} ", truncate(name, 24)), lines)
        }
        Assign::NewCategory { text, .. } => (
            " New category ".to_string(),
            vec![
                Line::from(""),
                Line::styled(format!(" {text}▏"), accent),
                Line::from(""),
                Line::styled(" Enter confirm · Esc cancel", app.muted()),
            ],
        ),
    };
    frame.render_widget(ratatui::widgets::Clear, rect);
    // The popup gets the theme background explicitly: Clear resets the cells it covers.
    frame.render_widget(
        Paragraph::new(lines)
            .style(Style::default().bg(tc(app.theme.bg)).fg(tc(app.theme.fg)))
            .block(app.block(title)),
        rect,
    );
}

/// `1h 05m`, `4m 07s` or `12s`.
pub fn fmt_duration(secs: i64) -> String {
    let secs = secs.max(0);
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{h}h {m:02}m")
    } else if m > 0 {
        format!("{m}m {s:02}s")
    } else {
        format!("{s}s")
    }
}

/// Shorten `text` to at most `max` characters, ending with `…` if cut.
fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{Snapshot, View};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use timesink_core::appmap::AppMap;
    use timesink_core::stats::Span as TimeSpan;

    /// UTC for the tests.
    fn utc(_: i64) -> i64 {
        0
    }

    /// An app with a realistic amount of data over the last 30 days.
    fn sample_app(view: View) -> App {
        let map =
            AppMap::from_conf("zen zen Browser\nwezterm wezterm Terminal\ndiscord discord Chat\n");
        let now = 100 * 86400 + 15 * 3600;
        let mut spans = Vec::new();
        let mut music = Vec::new();
        for d in 60..=100 {
            let day = d * 86400;
            spans.push(TimeSpan {
                start: day + 9 * 3600,
                end: day + 10 * 3600 + (d % 5) * 600,
                key: "zen".into(),
            });
            spans.push(TimeSpan {
                start: day + 11 * 3600,
                end: day + 12 * 3600 + 900,
                key: "wezterm".into(),
            });
            spans.push(TimeSpan {
                start: day + 13 * 3600,
                end: day + 13 * 3600 + 400,
                key: "discord".into(),
            });
            spans.push(TimeSpan {
                start: day + 14 * 3600,
                end: day + 14 * 3600 + 120,
                key: "mystery.App".into(),
            });
            music.push(TimeSpan {
                start: day + 9 * 3600,
                end: day + 10 * 3600,
                key: "cider".into(),
            });
        }
        App {
            snapshot: Snapshot::build(&spans, &music, &map, view, now, &utc),
            category_names: map.categories(),
            sort: SortMode::Time,
            export_menu: false,
            notice: None,
            show_help: false,
            status: None,
            theme: timesink_core::theme::THEMES[0],
            banner: true,
            assign: Assign::Closed,
        }
    }

    /// Render an app into a plain-text screen of the given size.
    fn render(app: &App, w: u16, h: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let mut out = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                out.push_str(buffer[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn today_shows_summary_hours_timeline_and_listening() {
        let screen = render(&sample_app(View::Today), 110, 32);
        for needle in [
            "Today",
            "7 days",
            "Categories",
            "Applications",
            "Browser",
            "zen",
            "Active time by hour",
            "Timeline",
            "Listening",
            "cider",
        ] {
            assert!(screen.contains(needle), "missing {needle:?}:\n{screen}");
        }
    }

    #[test]
    fn week_and_month_show_the_heatmap_with_day_labels() {
        for view in [View::Week, View::Month] {
            let screen = render(&sample_app(view), 120, 40);
            assert!(screen.contains("Active time by day and hour"), "{screen}");
            assert!(screen.contains("Applications"), "{screen}");
        }
        // 7-day view: exactly seven dated rows.
        let screen = render(&sample_app(View::Week), 120, 40);
        let dated = screen
            .lines()
            .filter(|l| {
                l.contains(" 1970")
                    || l.contains("Thu ")
                    || l.contains("Fri ")
                    || l.contains("Sat ")
                    || l.contains("Sun ")
                    || l.contains("Mon ")
                    || l.contains("Tue ")
                    || l.contains("Wed ")
            })
            .count();
        assert!(dated >= 7, "{screen}");
    }

    #[test]
    fn help_overlay_and_status_message_appear() {
        let mut app = sample_app(View::Today);
        app.show_help = true;
        app.status = Some(" something went wrong".into());
        let screen = render(&app, 100, 30);
        assert!(screen.contains("Help"));
        assert!(screen.contains("something went wrong"));
        // The longest help line must fit inside the box.
        assert!(screen.contains("overlaps focus time, so it is"), "{screen}");
        assert!(
            screen.contains("shown beside the totals, not inside."),
            "{screen}"
        );
    }

    #[test]
    fn heatmap_hour_labels_line_up_with_their_columns() {
        // With 1-wide cells the label for hour 12 must start exactly 12
        // characters after the label for hour 0.
        let screen = render(&sample_app(View::Week), 100, 30);
        let header = screen
            .lines()
            .find(|l| !l.contains("Active time by day") && l.contains("  3  6"))
            .unwrap_or_else(|| panic!("no header row:\n{screen}"));
        let start = header.find('0').unwrap();
        let twelve = header.find("12").unwrap();
        assert_eq!(twelve - start, 12, "{header}");
    }

    #[test]
    fn empty_database_still_draws() {
        let map = AppMap::default();
        let app = App {
            snapshot: Snapshot::build(&[], &[], &map, View::Week, 50 * 86400, &utc),
            category_names: vec![],
            sort: SortMode::Time,
            export_menu: false,
            notice: None,
            show_help: false,
            status: None,
            theme: timesink_core::theme::THEMES[0],
            banner: true,
            assign: Assign::Closed,
        };
        let screen = render(&app, 100, 30);
        assert!(screen.contains("nothing recorded yet"));
    }

    #[test]
    fn tiny_terminals_do_not_panic() {
        for view in View::ALL {
            let app = sample_app(view);
            for (w, h) in [(1, 1), (10, 3), (20, 5), (40, 10), (60, 15), (200, 60)] {
                let _ = render(&app, w, h);
            }
        }
    }

    #[test]
    fn busier_hours_are_brighter_and_empty_ones_use_the_faint_fill() {
        for theme in timesink_core::theme::THEMES {
            let Color::Rgb(r1, g1, b1) = heat_color(theme, 60, 3600) else {
                panic!("expected an rgb colour")
            };
            let Color::Rgb(r2, g2, b2) = heat_color(theme, 3600, 3600) else {
                panic!("expected an rgb colour")
            };
            // The busiest hour is exactly the theme's "ok" colour; quiet ones are nearer the fill.
            assert_eq!((r2, g2, b2), (theme.ok.0, theme.ok.1, theme.ok.2));
            assert_ne!((r1, g1, b1), (r2, g2, b2), "{}", theme.name);
            assert_eq!(heat_color(theme, 0, 3600), tc(theme.selection));
        }
    }

    #[test]
    fn every_theme_draws_every_view_with_its_own_background() {
        for theme in timesink_core::theme::THEMES {
            for view in View::ALL {
                let mut app = sample_app(view);
                app.theme = *theme;
                let backend = TestBackend::new(120, 40);
                let mut terminal = Terminal::new(backend).unwrap();
                terminal.draw(|f| draw(f, &app)).unwrap();
                // Every cell carries the theme background (nothing is left terminal-default).
                let want = tc(theme.bg);
                let buf = terminal.backend().buffer();
                assert!(
                    buf.content().iter().all(|c| c.bg == want),
                    "{} / {:?}",
                    theme.name,
                    view
                );
            }
        }
    }

    #[test]
    fn banner_shows_name_date_week_and_chart_and_can_hide() {
        let app = sample_app(View::Today);
        let screen = render(&app, 120, 40);
        assert!(screen.contains("t i m e s i n k"), "{screen}");
        // Day 100 after the epoch is 1970-04-11, a Saturday in ISO week 15.
        assert!(
            screen.contains("Saturday April 11 1970 · Week 15"),
            "{screen}"
        );
        assert!(screen.contains("06"), "{screen}");

        let mut hidden = sample_app(View::Today);
        hidden.banner = false;
        assert!(!render(&hidden, 120, 40).contains("t i m e s i n k"));

        // Too short a terminal hides it automatically.
        assert!(!render(&app, 120, 20).contains("t i m e s i n k"));
    }

    #[test]
    fn the_app_editor_popup_shows_each_step() {
        let mut app = sample_app(View::Today);
        app.assign = crate::assign::open(app.ordered_apps());
        let screen = render(&app, 100, 30);
        assert!(screen.contains("Edit which app?"), "{screen}");
        assert!(screen.contains("mystery.App"), "{screen}");

        let key = |c| crossterm::event::KeyEvent::new(c, crossterm::event::KeyModifiers::NONE);
        let known = app.category_names.clone();
        crate::assign::handle_key(
            &mut app.assign,
            key(crossterm::event::KeyCode::Enter),
            &known,
        );
        assert!(render(&app, 100, 30).contains("Short name for"));
        crate::assign::handle_key(
            &mut app.assign,
            key(crossterm::event::KeyCode::Enter),
            &known,
        );
        let screen = render(&app, 100, 30);
        assert!(screen.contains("Category for"), "{screen}");
        assert!(screen.contains("+ new category"), "{screen}");
        // And it survives tiny terminals.
        for (w, h) in [(1, 1), (30, 6), (60, 10)] {
            let _ = render(&app, w, h);
        }
    }

    #[test]
    fn banner_chart_drops_out_on_a_narrow_terminal_but_the_date_stays() {
        let screen = render(&sample_app(View::Today), 50, 30);
        assert!(screen.contains("Week 15"), "{screen}");
        assert!(!screen.contains("▇"), "{screen}");
    }

    #[test]
    fn categories_keep_their_colour_and_unknown_ones_are_grey() {
        let app = sample_app(View::Today);
        let browser = app.color_of("Browser");
        assert_eq!(browser, app.color_of("Browser"));
        assert_ne!(browser, app.color_of("Terminal"));
        // "Other" and anything unknown share the theme's quiet grey.
        let grey = tc(app.theme.category_color(None));
        assert_eq!(app.color_of("Other"), grey);
        assert_eq!(app.color_of("Never Heard Of It"), grey);
    }

    #[test]
    fn category_colours_follow_the_theme() {
        let mut app = sample_app(View::Today);
        let everforest = app.color_of("Browser");
        app.theme = *timesink_core::theme::find("Nord").unwrap();
        assert_ne!(app.color_of("Browser"), everforest);
    }

    #[test]
    fn the_first_twelve_categories_all_get_different_colours() {
        let mut app = sample_app(View::Today);
        app.category_names = (0..14).map(|i| format!("cat{i}")).collect();
        let colours: Vec<Color> = (0..12).map(|i| app.color_of(&format!("cat{i}"))).collect();
        for (i, a) in colours.iter().enumerate() {
            for b in &colours[i + 1..] {
                assert_ne!(a, b);
            }
        }
        // Past the theme's own accents the colours are lighter or deeper copies, never identical.
        assert_ne!(app.color_of("cat12"), app.color_of("cat0"));
        assert_ne!(app.color_of("cat13"), app.color_of("cat1"));
    }

    #[test]
    fn nine_categories_like_the_real_config_are_all_distinct() {
        let mut app = sample_app(View::Today);
        app.category_names = [
            "Terminal",
            "Browser",
            "Email",
            "Notes",
            "Chat",
            "Music",
            "Files",
            "Development",
            "Passwords",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        // The two biggest are the ones that must never look alike.
        assert_ne!(app.color_of("Terminal"), app.color_of("Browser"));
        let all: Vec<Color> = app.category_names.iter().map(|c| app.color_of(c)).collect();
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }

    /// Row number of the first screen line that contains `text`.
    fn row_of(screen: &str, text: &str) -> usize {
        screen
            .lines()
            .position(|l| l.contains(text))
            .unwrap_or_else(|| panic!("{text:?} not on screen:\n{screen}"))
    }

    #[test]
    fn lists_sort_by_time_or_by_name() {
        let mut app = sample_app(View::Today);

        // By time: Terminal (1h 15m) is above Browser (1h), "Other" is smallest.
        let screen = render(&app, 110, 32);
        assert!(screen.contains("Categories · by time"));
        assert!(row_of(&screen, "│ Terminal") < row_of(&screen, "│ Browser"));

        // By name: A to Z, with "Other" kept last.
        app.sort = SortMode::Name;
        let screen = render(&app, 110, 32);
        assert!(screen.contains("Categories · by name"));
        assert!(screen.contains("Applications · by name"));
        let (b, c, t, o) = (
            row_of(&screen, "│ Browser"),
            row_of(&screen, "│ Chat"),
            row_of(&screen, "│ Terminal"),
            row_of(&screen, "│ Other"),
        );
        assert!(b < c && c < t && t < o, "{screen}");
        // Applications are A to Z too: discord, mystery.App, wezterm, zen.
        assert!(row_of(&screen, "│ discord") < row_of(&screen, "│ wezterm"));
        assert!(row_of(&screen, "│ wezterm") < row_of(&screen, "│ zen"));
    }

    #[test]
    fn sort_mode_toggles_back_and_forth() {
        assert_eq!(SortMode::Time.toggle(), SortMode::Name);
        assert_eq!(SortMode::Name.toggle(), SortMode::Time);
    }

    #[test]
    fn footer_mentions_the_sort_and_export_keys() {
        let screen = render(&sample_app(View::Today), 100, 30);
        assert!(screen.contains("s sort"));
        assert!(screen.contains("e export"));
    }

    #[test]
    fn export_menu_and_notice_replace_the_hints() {
        let mut app = sample_app(View::Today);
        app.export_menu = true;
        let screen = render(&app, 100, 30);
        assert!(screen.contains("c csv"), "{screen}");
        assert!(screen.contains("h html"));
        assert!(!screen.contains("s sort"));

        app.export_menu = false;
        app.notice = Some(" Wrote /tmp/report.html".into());
        let screen = render(&app, 100, 30);
        assert!(screen.contains("Wrote /tmp/report.html"));
        // An error still wins over a notice.
        app.status = Some(" could not read the database".into());
        let screen = render(&app, 100, 30);
        assert!(screen.contains("could not read"));
        assert!(!screen.contains("Wrote /tmp"));
    }

    #[test]
    fn durations_format_like_the_report() {
        assert_eq!(fmt_duration(12), "12s");
        assert_eq!(fmt_duration(247), "4m 07s");
        assert_eq!(fmt_duration(3900), "1h 05m");
    }

    #[test]
    fn long_names_are_cut_with_an_ellipsis() {
        assert_eq!(truncate("android-studio-long", 8), "android…");
        assert_eq!(truncate("zen", 8), "zen");
    }

    /// Prints the three screens so they can be looked at by eye:
    /// `cargo test -p timesink-tui show_screens -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn show_screens() {
        for view in View::ALL {
            println!("{}", render(&sample_app(view), 120, 36));
        }
    }
}
