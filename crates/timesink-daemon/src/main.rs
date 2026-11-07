//! timesink-daemon: records how long each window is focused.
//!
//! Usage:
//!   timesink-daemon            run the collector (same as `run`)
//!   timesink-daemon run -v     run and print each new interval to stderr
//!   timesink-daemon report     print totals and recent intervals from the database
//!   timesink-daemon scrub      back up the database, then remove stored titles the
//!                              current settings would no longer record, and merge
//!                              the rows that become identical
//!
//! Environment overrides (all optional):
//!   TIMESINK_DB      full path of the database file
//!   TIMESINK_DEVICE  device name stored with every row (default: /etc/hostname)

mod hyprland;
mod idle;
mod logind;
mod media;
mod paths;
mod service;

use std::path::PathBuf;

use anyhow::Result;
use timesink_core::db::{Db, MERGE_GAP_SECS};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let verbose = args.iter().any(|a| a == "-v" || a == "--verbose");

    // The first non-flag argument picks the subcommand; default is "run".
    match args
        .iter()
        .find(|a| !a.starts_with('-'))
        .map(String::as_str)
    {
        None | Some("run") => service::run(verbose),
        Some("report") => report(),
        Some("scrub") => scrub(),
        Some(other) => anyhow::bail!("unknown command '{other}' (try: run, report, scrub)"),
    }
}

/// Clean up rows recorded before the current privacy settings existed.
///
/// A backup is written first. Stop the service before running this
/// (`systemctl --user stop timesink-daemon`) and start it again afterwards.
fn scrub() -> Result<()> {
    let config = paths::load_config()?;
    let sanitizer = paths::load_sanitizer(&config)?;
    let path = paths::db_path()?;
    if !path.exists() {
        anyhow::bail!("no database at {}", path.display());
    }
    let db = Db::open(&path, &paths::device_id())?;

    // 1. Back up first, to a name that cannot clash with an earlier backup.
    let backup = PathBuf::from(format!(
        "{}.bak-before-scrub-{}",
        path.display(),
        service::now_ts()
    ));
    db.backup_to(&backup)?;
    println!("Backup written to {}", backup.display());

    // 2. Re-apply today's rules to every stored title.
    let changed = db.rewrite_titles(|app, title| sanitizer.rescrub_stored(app, title))?;
    println!("Titles cleaned on {changed} rows");

    // 3. Rows that are now identical neighbours become one.
    let merged = db.merge_adjacent(MERGE_GAP_SECS)?;
    println!("Rows merged away: {merged}");
    Ok(())
}

/// Print a quick summary so you can check what was recorded.
fn report() -> Result<()> {
    let path = paths::db_path()?;
    let db = Db::open(&path, &paths::device_id())?;
    let now = service::now_ts();

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
