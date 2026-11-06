//! timesink-daemon: records how long each window is focused.
//!
//! Usage:
//!   timesink-daemon            run the collector (same as `run`)
//!   timesink-daemon run -v     run and print each new interval to stderr
//!   timesink-daemon report     print totals and recent intervals from the database
//!
//! Environment overrides (all optional):
//!   TIMESINK_DB      full path of the database file
//!   TIMESINK_DEVICE  device name stored with every row (default: /etc/hostname)

mod hyprland;

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use timesink_core::db::{Db, Recorder};
use timesink_core::model::WindowSnapshot;
use timesink_core::sanitize::Sanitizer;
use timesink_core::tracker::{Effect, Tracker};

use hyprland::{is_relevant_event, Hypr, Next};

/// How often the open interval is refreshed when no events arrive.
const HEARTBEAT: Duration = Duration::from_secs(30);

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let verbose = args.iter().any(|a| a == "-v" || a == "--verbose");

    // The first non-flag argument picks the subcommand; default is "run".
    match args
        .iter()
        .find(|a| !a.starts_with('-'))
        .map(String::as_str)
    {
        None | Some("run") => run(verbose),
        Some("report") => report(),
        Some(other) => anyhow::bail!("unknown command '{other}' (try: run, report)"),
    }
}

/// Current time in whole seconds since the Unix epoch.
fn now_ts() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Folder for data files: `$XDG_DATA_HOME/timesink` or `~/.local/share/timesink`.
fn data_dir() -> Result<PathBuf> {
    xdg_dir("XDG_DATA_HOME", ".local/share")
}

/// Folder for config files: `$XDG_CONFIG_HOME/timesink` or `~/.config/timesink`.
fn config_dir() -> Result<PathBuf> {
    xdg_dir("XDG_CONFIG_HOME", ".config")
}

/// Resolve an XDG base folder (or its `$HOME` fallback) and append `timesink`.
fn xdg_dir(var: &str, home_fallback: &str) -> Result<PathBuf> {
    let base = match std::env::var_os(var) {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => {
            PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?).join(home_fallback)
        }
    };
    Ok(base.join("timesink"))
}

/// Database path: `TIMESINK_DB` if set, otherwise the default data folder.
fn db_path() -> Result<PathBuf> {
    match std::env::var_os("TIMESINK_DB") {
        Some(path) => Ok(PathBuf::from(path)),
        None => Ok(data_dir()?.join("timesink.db")),
    }
}

/// Name of this computer: `TIMESINK_DEVICE`, else /etc/hostname, else "unknown".
fn device_id() -> String {
    if let Ok(name) = std::env::var("TIMESINK_DEVICE") {
        if !name.trim().is_empty() {
            return name.trim().to_string();
        }
    }
    std::fs::read_to_string("/etc/hostname")
        .map(|s| s.trim().to_string())
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Load the optional `hosts.conf` that maps ssh hosts to friendly labels.
fn load_sanitizer() -> Result<Sanitizer> {
    let path = config_dir()?.join("hosts.conf");
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(Sanitizer::from_hosts_conf(&text)),
        // No file is fine: every ssh host is then recorded as "ssh: other".
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Sanitizer::default()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Ask Hyprland what is focused, clean it, and feed it to the tracker/recorder.
fn sample(
    hypr: &Hypr,
    sanitizer: &Sanitizer,
    tracker: &mut Tracker,
    recorder: &mut Recorder,
    verbose: bool,
) -> Result<()> {
    // Turn Hyprland's raw answer into a privacy-cleaned snapshot.
    let snapshot = hypr.active_window()?.map(|w| WindowSnapshot {
        title: sanitizer.clean_title(&w.class, &w.title),
        app_class: w.class,
        workspace: w.workspace,
    });
    let effects = tracker.observe(now_ts(), snapshot);

    // In verbose mode, show each interval as it opens.
    if verbose {
        for effect in &effects {
            if let Effect::Opened { snapshot, .. } = effect {
                eprintln!(
                    "focus: {} | {} | ws {}",
                    snapshot.app_class, snapshot.title, snapshot.workspace
                );
            }
        }
    }
    recorder.apply(&effects)
}

/// The collector: watch Hyprland and record until it exits.
fn run(verbose: bool) -> Result<()> {
    let hypr = Hypr::from_env()?;
    let sanitizer = load_sanitizer()?;
    let path = db_path()?;
    let mut recorder = Recorder::new(Db::open(&path, &device_id())?);
    let mut tracker = Tracker::new();
    eprintln!("timesink: recording to {}", path.display());

    let mut events = hypr.events(HEARTBEAT)?;
    // Record the window that is focused right now before waiting for events.
    sample(&hypr, &sanitizer, &mut tracker, &mut recorder, verbose)?;

    loop {
        match events.next()? {
            // A relevant event: re-check what is focused.
            Next::Event(line) if is_relevant_event(&line) => {
                sample_logged(&hypr, &sanitizer, &mut tracker, &mut recorder, verbose);
            }
            Next::Event(_) => {}
            // Quiet period: refresh the open interval so its end time advances.
            Next::Timeout => {
                sample_logged(&hypr, &sanitizer, &mut tracker, &mut recorder, verbose);
            }
            // Hyprland went away: close the interval and stop.
            Next::Closed => {
                recorder.apply(&tracker.finish(now_ts()))?;
                eprintln!("timesink: Hyprland closed the event stream, exiting");
                return Ok(());
            }
        }
    }
}

/// Like `sample`, but a failed sample is logged instead of stopping the daemon.
fn sample_logged(
    hypr: &Hypr,
    sanitizer: &Sanitizer,
    tracker: &mut Tracker,
    recorder: &mut Recorder,
    verbose: bool,
) {
    if let Err(e) = sample(hypr, sanitizer, tracker, recorder, verbose) {
        eprintln!("timesink: sample failed: {e:#}");
    }
}

/// Print a quick summary so you can check what was recorded.
fn report() -> Result<()> {
    let path = db_path()?;
    let db = Db::open(&path, &device_id())?;
    let now = now_ts();

    println!("Database: {}", path.display());
    println!("\nLast 24 hours, by application:");
    for (app, secs) in db.totals_since(now - 24 * 3600)? {
        println!("  {:>9}  {}", format_duration(secs), app);
    }

    println!("\nMost recent intervals:");
    for i in db.recent(15)? {
        let title = if i.title.is_empty() { "-" } else { &i.title };
        println!(
            "  {:>9} ago  {:>9}  {} | {}",
            format_duration(now - i.start_ts),
            format_duration(i.duration_secs()),
            i.app_class,
            title
        );
    }
    Ok(())
}

/// Format seconds as `1h 05m`, `4m 07s` or `12s`.
fn format_duration(secs: i64) -> String {
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

#[cfg(test)]
mod tests {
    use super::format_duration;

    #[test]
    fn formats_durations() {
        assert_eq!(format_duration(12), "12s");
        assert_eq!(format_duration(247), "4m 07s");
        assert_eq!(format_duration(3900), "1h 05m");
        assert_eq!(format_duration(-5), "0s");
    }
}
