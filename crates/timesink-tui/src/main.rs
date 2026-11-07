//! `timesink`: the terminal interface. Reads the database the daemon writes.

mod cli;
mod commands;
mod data;
mod export;
mod palette;
mod ui;

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::{Local, Offset, TimeZone};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use timesink_core::appmap::AppMap;
use timesink_core::db::Db;
use timesink_core::paths;

use crate::data::{Snapshot, View};
use crate::ui::{App, SortMode};

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
            out,
            force,
        } => {
            let request = commands::ExportRequest {
                format,
                days,
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
        cli::Command::Tui | cli::Command::Help => run_interface(db, map),
    }
}

/// Open the interactive screens and run them until the user quits.
fn run_interface(db: Db, map: AppMap) -> Result<()> {
    let snapshot = Snapshot::load(&db, &map, View::Today, now_ts(), &local_offset)?;
    let mut app = App {
        snapshot,
        category_names: map.categories(),
        sort: SortMode::Time,
        show_help: false,
        status: None,
    };

    // Restores the terminal even if drawing panics.
    let mut terminal = ratatui::init();
    let result = run(&mut terminal, &mut app, &db, &map);
    ratatui::restore();
    result
}

/// Draw, wait for a key or the refresh timer, repeat.
fn run(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    db: &Db,
    map: &AppMap,
) -> Result<()> {
    let mut last_refresh = Instant::now();
    loop {
        terminal.draw(|frame| ui::draw(frame, app))?;

        // Wake up for the next refresh even if no key is pressed.
        let wait = REFRESH.saturating_sub(last_refresh.elapsed());
        if event::poll(wait).context("reading the keyboard")? {
            if let Event::Key(key) = event::read()? {
                // Ignore key releases (some terminals send them).
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                let view = app.snapshot.view;
                let new_view = match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        return Ok(())
                    }
                    KeyCode::Right | KeyCode::Tab | KeyCode::Char('l') => Some(view.next()),
                    KeyCode::Left | KeyCode::BackTab | KeyCode::Char('h') => Some(view.prev()),
                    KeyCode::Char('1') => Some(View::Today),
                    KeyCode::Char('2') => Some(View::Week),
                    KeyCode::Char('3') => Some(View::Month),
                    KeyCode::Char('?') => {
                        app.show_help = !app.show_help;
                        None
                    }
                    KeyCode::Char('s') => {
                        // Only the order changes, so no need to re-read the database.
                        app.sort = app.sort.toggle();
                        None
                    }
                    KeyCode::Char('r') => Some(view),
                    _ => None,
                };
                if let Some(v) = new_view {
                    reload(app, db, map, v);
                    last_refresh = Instant::now();
                }
            }
        } else {
            // The timer ran out: refresh whatever is on screen.
            let view = app.snapshot.view;
            reload(app, db, map, view);
            last_refresh = Instant::now();
        }
    }
}

/// Recompute the numbers. A failure is shown in the footer, not fatal.
fn reload(app: &mut App, db: &Db, map: &AppMap, view: View) {
    match Snapshot::load(db, map, view, now_ts(), &local_offset) {
        Ok(snapshot) => {
            app.snapshot = snapshot;
            app.status = None;
        }
        Err(e) => app.status = Some(format!(" could not read the database: {e:#}")),
    }
}
