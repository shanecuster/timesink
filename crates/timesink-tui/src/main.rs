//! `timesink`: the terminal interface. Reads the database the daemon writes.

mod assign;
mod banner;
mod cli;
mod commands;
mod dates;
mod export;
mod palette;
mod ui;

use std::cell::Cell;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::{Local, Offset, TimeZone};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use timesink_core::appmap::{self, AppMap};
use timesink_core::db::Db;
use timesink_core::paths;
use timesink_core::settings::Settings;
use timesink_core::theme;

use crate::export::Format;
use crate::ui::{App, Scroll, SortMode};
use timesink_core::data::{Snapshot, View};

/// How often the screen refreshes by itself.
const REFRESH: Duration = Duration::from_secs(5);

/// Offset of the local time zone from UTC, in seconds, at a given moment.
fn local_offset(ts: i64) -> i64 {
    Local
        .timestamp_opt(ts, 0)
        .single()
        .map(|d| d.offset().fix().local_minus_utc() as i64)
        .unwrap_or(0)
}

/// Seconds since the Unix epoch now.
fn now_ts() -> i64 {
    Local::now().timestamp()
}

fn main() {
    // Print errors plainly (no stack trace) and exit with a failure code.
    if let Err(e) = start() {
        eprintln!("timesink: {e:#}");
        std::process::exit(1);
    }
}

/// Work out what was asked for, then do it.
fn start() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = cli::parse(&args).map_err(anyhow::Error::msg)?;
    if command == cli::Command::Help {
        print!("{}", cli::USAGE);
        return Ok(());
    }

    // The sandbox command creates its own database, so it runs before the "is there a
    // database?" check below.
    if let cli::Command::DemoData { days, seed } = command {
        let summary = commands::run_demo(days, seed, now_ts(), &local_offset)?;
        println!("{summary}");
        return Ok(());
    }

    let path = paths::db_path()?;
    if !path.exists() {
        anyhow::bail!(
            "no database at {}; is the daemon running? (systemctl --user status timesink-daemon)",
            path.display()
        );
    }
    let db = Db::open(&path, &paths::device_id())?;
    let map = paths::load_app_map()?;

    match command {
        cli::Command::Export {
            format,
            days,
            from,
            to,
            out,
            force,
        } => {
            // An explicit range wins; its end defaults to today.
            let span = match from {
                Some(first) => {
                    let today = timesink_core::stats::local_day(now_ts(), &local_offset);
                    commands::ExportSpan::Between(timesink_core::period::Range::new(
                        first,
                        to.unwrap_or(today),
                    ))
                }
                None => commands::ExportSpan::Last(days),
            };
            let request = commands::ExportRequest {
                format,
                span,
                out: out.as_deref(),
                force,
            };
            let file = commands::run_export(&db, &map, &request, now_ts(), &local_offset)?;
            println!("Wrote {}", file.display());
            Ok(())
        }
        cli::Command::Backup { out } => {
            let file = commands::run_backup(&db, out.as_deref(), now_ts(), &local_offset)?;
            println!("Backup written to {}", file.display());
            Ok(())
        }
        cli::Command::Sync { status } => {
            println!("{}", commands::run_sync(&db, status)?);
            Ok(())
        }
        // Handled before the database was opened; listed so the match stays complete.
        cli::Command::DemoData { .. } => Ok(()),
        cli::Command::Tui | cli::Command::Help => run_interface(db, map),
    }
}

/// Open the interactive screens and run them until the user quits.
fn run_interface(db: Db, mut map: AppMap) -> Result<()> {
    let snapshot = Snapshot::load_default(&db, &map, View::Today, None, now_ts(), &local_offset)?;
    // Remembered choices (theme, sort order, banner). A broken settings file is not worth
    // refusing to start over, so fall back to the defaults.
    let settings = paths::tui_settings_path()
        .and_then(|p| Settings::load(&p))
        .unwrap_or_default();
    let mut app = App {
        snapshot,
        category_names: map.categories(),
        sort: if settings.sort_by_name {
            SortMode::Name
        } else {
            SortMode::Time
        },
        theme: *theme::find_or_default(&settings.theme),
        banner: settings.banner,
        assign: assign::Assign::Closed,
        site_categories: Vec::new(),
        scroll: Scroll::default(),
        limits: Cell::new([0; 4]),
        device: None,
        devices: db.devices().unwrap_or_default(),
        syncing: false,
        sync_note: None,
        offset: 0,
        custom: None,
        date_prompt: None,
        export_menu: false,
        notice: None,
        show_help: false,
        status: None,
    };

    // Website time, if the browser extension has recorded any.
    attach_sites(&mut app, &db);
    app.sync_note = read_sync_note(&db);

    // Restores the terminal even if drawing panics.
    let mut terminal = ratatui::init();
    let result = run(&mut terminal, &mut app, &db, &mut map);
    ratatui::restore();
    result
}

/// Draw, wait for a key or the refresh timer, repeat.
fn run(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    db: &Db,
    map: &mut AppMap,
) -> Result<()> {
    let mut last_refresh = Instant::now();
    let mut sync_rx: Option<mpsc::Receiver<String>> = None;
    loop {
        terminal.draw(|frame| ui::draw(frame, app))?;

        // A finished background sync shows its result and re-reads the numbers.
        if let Some(rx) = &sync_rx {
            match rx.try_recv() {
                Ok(message) => {
                    sync_rx = None;
                    app.syncing = false;
                    app.notice = Some(format!(" {message}"));
                    let view = app.snapshot.view;
                    reload(app, db, map, view);
                    last_refresh = Instant::now();
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    sync_rx = None;
                    app.syncing = false;
                    app.notice = Some(" sync stopped unexpectedly".into());
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }

        // Wake up for the next refresh even if no key is pressed (sooner while a sync
        // runs, so its result shows up promptly).
        let mut wait = REFRESH.saturating_sub(last_refresh.elapsed());
        if sync_rx.is_some() {
            wait = wait.min(Duration::from_millis(300));
        }
        if event::poll(wait).context("reading the keyboard")? {
            if let Event::Key(key) = event::read()? {
                // Ignore key releases (some terminals send them).
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                let view = app.snapshot.view;
                // Any key clears the last notice (for example "Wrote ...").
                app.notice = None;

                // While the date prompt is open it gets every key too.
                if let Some(prompt) = app.date_prompt.as_mut() {
                    let today = timesink_core::stats::local_day(now_ts(), &local_offset);
                    match dates::handle_key(prompt, key, today) {
                        dates::Outcome::Open => {}
                        dates::Outcome::Cancelled => app.date_prompt = None,
                        dates::Outcome::Chosen(range) => {
                            app.date_prompt = None;
                            app.custom = Some(range);
                            app.offset = 0;
                            reload(app, db, map, View::Custom);
                            last_refresh = Instant::now();
                        }
                    }
                    continue;
                }

                // While the app editor is open it gets every key (so typing "q" in a name
                // does not quit).
                if app.assign != assign::Assign::Closed {
                    if let Some(write) =
                        assign::handle_key(&mut app.assign, key, &app.category_names)
                    {
                        app.notice = Some(apply_assignment(app, map, &write));
                        reload(app, db, map, view);
                        last_refresh = Instant::now();
                    }
                    continue;
                }

                // While the export menu is open, the next key picks a format.
                if app.export_menu {
                    app.export_menu = false;
                    let format = match key.code {
                        KeyCode::Char('c') => Some(Format::Csv),
                        KeyCode::Char('j') => Some(Format::Json),
                        KeyCode::Char('m') => Some(Format::Markdown),
                        KeyCode::Char('h') => Some(Format::Html),
                        _ => None, // Esc or anything else cancels
                    };
                    if let Some(format) = format {
                        app.notice = Some(export_view(db, map, format, app));
                    }
                    continue;
                }

                // Stepping, refreshing and switching computer stay on the period being
                // looked at; picking a view starts again at its current period.
                let mut keep_offset = false;
                let new_view = match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        return Ok(())
                    }
                    KeyCode::Right | KeyCode::Tab | KeyCode::Char('l') => view.next(),
                    KeyCode::Left | KeyCode::BackTab | KeyCode::Char('h') => view.prev(),
                    KeyCode::Char('1') => Some(View::Today),
                    KeyCode::Char('2') => Some(View::Week),
                    KeyCode::Char('3') => Some(View::Month),
                    KeyCode::Char('4') => Some(View::Year),
                    KeyCode::Char('5') => Some(View::All),
                    KeyCode::Char('6') => Some(View::CalWeek),
                    KeyCode::Char('7') => Some(View::Quarter),
                    KeyCode::Char('8') => {
                        // The first time there is nothing to show yet, so ask for the days.
                        if app.custom.is_none() {
                            app.date_prompt = Some(dates::DatePrompt::default());
                        }
                        Some(View::Custom)
                    }
                    KeyCode::Char('c') => {
                        // Start from the range already chosen, if any, so it can be edited.
                        app.date_prompt = Some(match app.custom {
                            Some(range) => dates::DatePrompt::starting_with(range),
                            None => dates::DatePrompt::default(),
                        });
                        None
                    }
                    KeyCode::Char(c @ ('[' | ']')) => {
                        // `[` goes back one period, `]` forward again.
                        let delta = if c == '[' { -1 } else { 1 };
                        let today = timesink_core::stats::local_day(now_ts(), &local_offset);
                        match view.step(app.offset, delta, today, earliest_day(db), app.custom) {
                            Some(offset) => {
                                app.offset = offset;
                                keep_offset = true;
                                Some(view)
                            }
                            None => {
                                app.notice = Some(
                                    if delta < 0 && view != View::All {
                                        " no earlier data"
                                    } else if view == View::All {
                                        " all time is one period"
                                    } else {
                                        " already at the latest"
                                    }
                                    .into(),
                                );
                                None
                            }
                        }
                    }
                    KeyCode::Char('?') => {
                        app.show_help = !app.show_help;
                        None
                    }
                    KeyCode::Char('e') => {
                        app.export_menu = true;
                        None
                    }
                    KeyCode::Char('s') => {
                        // Only the order changes, so no need to re-read the database.
                        app.sort = app.sort.toggle();
                        save_settings(app);
                        None
                    }
                    KeyCode::Char('a') => {
                        app.assign = assign::open(app.ordered_apps());
                        None
                    }
                    KeyCode::Char('t') => {
                        // Next theme in the list; the colours change on the next draw.
                        app.theme = *theme::next(&app.theme);
                        app.notice = Some(format!(" Theme: {}", app.theme.name));
                        save_settings(app);
                        None
                    }
                    KeyCode::Char('b') => {
                        app.banner = !app.banner;
                        save_settings(app);
                        None
                    }
                    KeyCode::Char('r') => {
                        keep_offset = true;
                        Some(view)
                    }
                    KeyCode::Char('d') => {
                        app.devices = db.devices().unwrap_or_default();
                        if app.devices.len() < 2 {
                            app.notice = Some(" only one computer has data here".into());
                            None
                        } else {
                            app.device = next_device(&app.devices, app.device.as_deref());
                            keep_offset = true;
                            Some(view)
                        }
                    }
                    KeyCode::Char('S') => {
                        if app.syncing {
                            app.notice = Some(" a sync is already running".into());
                        } else {
                            sync_rx = Some(start_sync());
                            app.syncing = true;
                        }
                        None
                    }
                    // `p` picks which list the scroll keys move.
                    KeyCode::Char('p') => {
                        let panels = app.panels();
                        app.scroll.next_focus(&panels);
                        None
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        app.scroll.by(1, app.limits.get());
                        None
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        app.scroll.by(-1, app.limits.get());
                        None
                    }
                    KeyCode::PageDown => {
                        app.scroll.by(5, app.limits.get());
                        None
                    }
                    KeyCode::PageUp => {
                        app.scroll.by(-5, app.limits.get());
                        None
                    }
                    KeyCode::Home => {
                        app.scroll.jump(false, app.limits.get());
                        None
                    }
                    KeyCode::End => {
                        app.scroll.jump(true, app.limits.get());
                        None
                    }
                    _ => None,
                };
                if let Some(v) = new_view {
                    if !keep_offset {
                        app.offset = 0;
                    }
                    reload(app, db, map, v);
                    last_refresh = Instant::now();
                }
            }
        } else if sync_rx.is_some() && last_refresh.elapsed() < REFRESH {
            // Only the short sync poll ran out; nothing to redraw but the spinner.
        } else {
            // The timer ran out: refresh whatever is on screen.
            let view = app.snapshot.view;
            reload(app, db, map, view);
            last_refresh = Instant::now();
        }
    }
}

/// Save an edit from the app editor to `apps.conf`, re-read the file and describe what
/// happened. One line is appended for each raw class the row stands for.
fn apply_assignment(app: &mut App, map: &mut AppMap, write: &assign::Write) -> String {
    let result = paths::apps_conf_path().and_then(|path| {
        for raw in &write.raws {
            appmap::append_entry(&path, raw, &write.display, &write.category)?;
        }
        paths::load_app_map()
    });
    match result {
        Ok(new_map) => {
            *map = new_map;
            app.category_names = map.categories();
            format!(" {} is now in {}", write.display, write.category)
        }
        Err(e) => format!(" could not update apps.conf: {e:#}"),
    }
}

/// Remember the current theme, sort order and banner for the next run. A failure (say, a
/// read-only home folder) is shown in the footer rather than stopping the interface.
fn save_settings(app: &mut App) {
    let settings = Settings {
        theme: app.theme.name.to_string(),
        sort_by_name: app.sort == SortMode::Name,
        banner: app.banner,
    };
    let result = paths::tui_settings_path().and_then(|p| settings.save(&p));
    if let Err(e) = result {
        app.notice = Some(format!(" could not save settings: {e:#}"));
    }
}

/// Write the report for the period on screen and describe what happened.
fn export_view(db: &Db, map: &AppMap, format: Format, app: &App) -> String {
    let view = app.snapshot.view;
    // The usual rolling views (not stepped back) keep their short names ("week", "month");
    // "All time" means everything recorded; anything else is exported by its exact days.
    let span = match view.days() {
        Some(days) if app.offset == 0 => commands::ExportSpan::Last(days),
        _ if view == View::All => commands::ExportSpan::Last(0),
        _ => commands::ExportSpan::Between(app.snapshot.range),
    };
    let request = commands::ExportRequest {
        format,
        span,
        out: None,
        force: false,
    };
    match commands::run_export(db, map, &request, now_ts(), &local_offset) {
        Ok(path) => format!(" Wrote {}", path.display()),
        Err(e) => format!(" Export failed: {e:#}"),
    }
}

/// Add website time to the current snapshot. A problem here is not worth interrupting the
/// screen for: the Sites panel just stays empty.
fn attach_sites(app: &mut App, db: &Db) {
    let config = paths::load_domain_config().unwrap_or_default();
    app.site_categories = config.sites.categories();
    if let Err(e) =
        app.snapshot
            .attach_sites(db, &config, app.device.as_deref(), now_ts(), &local_offset)
    {
        app.status = Some(format!(" could not read website time: {e:#}"));
    }
}

/// Read how the last sync went from the database, for the footer.
fn read_sync_note(db: &Db) -> Option<String> {
    // A saved time, or None if the key is missing, empty ("cleared") or not a number.
    let time = |key: &str| -> Option<i64> {
        db.sync_state(key)
            .ok()
            .flatten()
            .and_then(|v| v.trim().parse().ok())
    };
    ui::sync_note(time("last_ok"), time("last_failed"), now_ts())
}

/// The next choice after `current` in: all, then each computer, then all again.
fn next_device(devices: &[String], current: Option<&str>) -> Option<String> {
    match current {
        None => devices.first().cloned(),
        Some(c) => {
            let i = devices.iter().position(|d| d == c)?;
            devices.get(i + 1).cloned()
        }
    }
}

/// Run a sync on its own thread with its own database connection, and send back one
/// line describing the outcome.
fn start_sync() -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let outcome = paths::db_path()
            .and_then(|p| Db::open(&p, &paths::device_id()))
            .and_then(|db| commands::run_sync(&db, false));
        let message = match outcome {
            Ok(text) => text.replace('\n', " · "),
            Err(e) => format!("sync failed: {e:#}"),
        };
        let _ = tx.send(message);
    });
    rx
}

/// The first day anything was recorded (a local day number), if anything was.
fn earliest_day(db: &Db) -> Option<i64> {
    db.earliest_start()
        .ok()
        .flatten()
        .map(|t| timesink_core::stats::local_day(t, &local_offset))
}

/// Recompute the numbers. A failure is shown in the footer, not fatal.
fn reload(app: &mut App, db: &Db, map: &AppMap, view: View) {
    app.devices = db.devices().unwrap_or_default();
    app.sync_note = read_sync_note(db);
    // A device that vanished (forgotten elsewhere) goes back to the combined view.
    if app
        .device
        .as_ref()
        .is_some_and(|d| !app.devices.contains(d))
    {
        app.device = None;
    }
    let tz = &local_offset;
    let today = timesink_core::stats::local_day(now_ts(), tz);
    let range = view.range(app.offset, today, earliest_day(db), app.custom);
    match Snapshot::load(db, map, view, range, app.device.as_deref(), now_ts(), tz) {
        Ok(snapshot) => {
            app.snapshot = snapshot;
            app.status = None;
            attach_sites(app, db);
        }
        Err(e) => app.status = Some(format!(" could not read the database: {e:#}")),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn device_key_cycles_through_all_then_each_then_all() {
        let d = vec!["deck".to_string(), "laptop".to_string()];
        assert_eq!(super::next_device(&d, None), Some("deck".into()));
        assert_eq!(super::next_device(&d, Some("deck")), Some("laptop".into()));
        assert_eq!(super::next_device(&d, Some("laptop")), None);
        // A device that disappeared falls back to the combined view.
        assert_eq!(super::next_device(&d, Some("gone")), None);
    }
}
