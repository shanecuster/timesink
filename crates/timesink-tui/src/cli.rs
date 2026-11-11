//! Command-line arguments for `timesink`.
//!
//! ```text
//! timesink                      open the interface
//! timesink export [options]     write a report file
//! timesink backup [--out PATH]  copy the database
//! timesink sync [status]        sync with CouchDB (or show its state)
//! ```

use std::path::PathBuf;

use crate::export::Format;
use timesink_core::period;

/// What the user asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Open the interactive screens.
    Tui,
    /// Write a report.
    Export {
        /// File format.
        format: Format,
        /// Number of days, ending today (0 = everything recorded).
        days: i64,
        /// First day of an explicit range (local day number); with it, `days` is ignored.
        from: Option<i64>,
        /// Last day of an explicit range; today when `from` is given without it.
        to: Option<i64>,
        /// Where to write; the default folder is used when absent.
        out: Option<PathBuf>,
        /// Replace an existing file.
        force: bool,
    },
    /// Copy the database.
    Backup {
        /// Where to write; the default folder is used when absent.
        out: Option<PathBuf>,
    },
    /// Sync this computer's rows with CouchDB, or (with `status`) just describe the setup.
    Sync {
        /// Only show the state; do not contact the server.
        status: bool,
    },
    /// Fill an empty sandbox database with made-up data (refused outside the sandbox).
    DemoData {
        /// How many days to make, ending today.
        days: i64,
        /// Seed for the repeatable random numbers.
        seed: u64,
    },
    /// Show the usage text.
    Help,
}

/// The text shown for `--help`.
pub const USAGE: &str = "\
timesink: where did my time go?

USAGE
  timesink                         open the interface
  timesink export [options]        write a report file
  timesink backup [--out PATH]     copy the database (safe while the daemon runs)
  timesink sync                    upload this computer's days to CouchDB and download
                                   the other computers' (needs ~/.config/timesink/sync.conf)
  timesink sync status             show the sync setup and what is waiting (offline)

EXPORT OPTIONS
  -f, --format FORMAT   csv, json, md or html           (default: html)
  -p, --period PERIOD   today, week, month, year or all (default: week)
      --days N          last N days instead of a period
      --from DATE       a range of days instead: first day, as YYYY-MM-DD
      --to DATE         last day of that range (default: today)
  -o, --out PATH        a file (name with an extension) or a folder
      --force           replace an existing file

SANDBOX
  timesink demo-data [--days N] [--seed N]   fill an empty sandbox database with
                                              made-up data (see contrib/sandbox.sh)

Files go to ~/Documents/timesink-exports unless --out is given.
CSV and JSON include every recorded row; md and html are summaries.
";

/// Turn the program's arguments (without the program name) into a command.
/// The error is a message meant for the user.
pub fn parse(args: &[String]) -> Result<Command, String> {
    let Some(first) = args.first() else {
        return Ok(Command::Tui);
    };
    match first.as_str() {
        "-h" | "--help" | "help" => Ok(Command::Help),
        "export" => parse_export(&args[1..]),
        "backup" => parse_backup(&args[1..]),
        "demo-data" => parse_demo(&args[1..]),
        "sync" => match args.get(1).map(String::as_str) {
            None => Ok(Command::Sync { status: false }),
            Some("status") if args.len() == 2 => Ok(Command::Sync { status: true }),
            Some(other) => Err(format!(
                "unknown sync option '{other}' (use: timesink sync, or timesink sync status)"
            )),
        },
        other => Err(format!("unknown command '{other}' (try: timesink --help)")),
    }
}

/// The value after an option such as `--format`.
fn value<'a>(args: &'a [String], i: usize, name: &str) -> Result<&'a str, String> {
    args.get(i + 1)
        .map(String::as_str)
        .ok_or_else(|| format!("{name} needs a value"))
}

/// Parse the arguments after `export`.
fn parse_export(args: &[String]) -> Result<Command, String> {
    let mut format = Format::Html;
    let mut days = 7;
    let (mut from, mut to) = (None, None);
    // Was a period or a day count given? (It cannot be combined with --from / --to.)
    let mut period_given = false;
    let mut out = None;
    let mut force = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-f" | "--format" => {
                let text = value(args, i, "--format")?;
                format = Format::parse(text).ok_or_else(|| {
                    format!("unknown format '{text}' (use csv, json, md or html)")
                })?;
                i += 1;
            }
            "--from" | "--to" => {
                let name = args[i].clone();
                let text = value(args, i, &name)?;
                let day = period::parse_date(text)
                    .ok_or_else(|| format!("{name} needs a date like 2026-11-03, not '{text}'"))?;
                if name == "--from" {
                    from = Some(day);
                } else {
                    to = Some(day);
                }
                i += 1;
            }
            "-p" | "--period" => {
                period_given = true;
                days = match value(args, i, "--period")? {
                    "today" => 1,
                    "week" => 7,
                    "month" => 30,
                    "year" => 365,
                    // 0 = from the first recorded day (worked out when the report is made).
                    "all" => 0,
                    other => {
                        return Err(format!(
                            "unknown period '{other}' (use today, week, month, year or all, or --days N)"
                        ))
                    }
                };
                i += 1;
            }
            "--days" => {
                period_given = true;
                let text = value(args, i, "--days")?;
                days = text
                    .parse::<i64>()
                    .ok()
                    .filter(|n| (1..=3650).contains(n))
                    .ok_or_else(|| format!("--days needs a number from 1 to 3650, not '{text}'"))?;
                i += 1;
            }
            "-o" | "--out" => {
                out = Some(PathBuf::from(value(args, i, "--out")?));
                i += 1;
            }
            "--force" => force = true,
            other => return Err(format!("unknown option '{other}' (try: timesink --help)")),
        }
        i += 1;
    }
    if to.is_some() && from.is_none() {
        return Err("--to needs --from (the first day of the range)".to_string());
    }
    if from.is_some() && period_given {
        return Err("use either --from / --to or a period (-p, --days), not both".to_string());
    }
    if let (Some(a), Some(b)) = (from, to) {
        if a > b {
            return Err("--from must not be later than --to".to_string());
        }
    }
    Ok(Command::Export {
        format,
        days,
        from,
        to,
        out,
        force,
    })
}

/// Parse the arguments after `demo-data`: `[--days N] [--seed N]`.
fn parse_demo(args: &[String]) -> Result<Command, String> {
    let (mut days, mut seed) = (35, 1);
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--days" => {
                let text = value(args, i, "--days")?;
                days = text
                    .parse::<i64>()
                    .ok()
                    .filter(|n| (1..=400).contains(n))
                    .ok_or_else(|| format!("--days needs a number from 1 to 400, not '{text}'"))?;
                i += 1;
            }
            "--seed" => {
                let text = value(args, i, "--seed")?;
                seed = text
                    .parse::<u64>()
                    .map_err(|_| format!("--seed needs a whole number, not '{text}'"))?;
                i += 1;
            }
            other => return Err(format!("unknown option '{other}' (try: timesink --help)")),
        }
        i += 1;
    }
    Ok(Command::DemoData { days, seed })
}

/// Parse the arguments after `backup`.
fn parse_backup(args: &[String]) -> Result<Command, String> {
    let mut out = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-o" | "--out" => {
                out = Some(PathBuf::from(value(args, i, "--out")?));
                i += 1;
            }
            other => return Err(format!("unknown option '{other}' (try: timesink --help)")),
        }
        i += 1;
    }
    Ok(Command::Backup { out })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Split a command line the way the shell would (no quoting needed here).
    fn args(line: &str) -> Vec<String> {
        line.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn no_arguments_opens_the_interface() {
        assert_eq!(parse(&[]), Ok(Command::Tui));
    }

    #[test]
    fn help_is_recognised() {
        assert_eq!(parse(&args("--help")), Ok(Command::Help));
        assert_eq!(parse(&args("-h")), Ok(Command::Help));
        assert_eq!(parse(&args("help")), Ok(Command::Help));
    }

    #[test]
    fn export_defaults_to_a_week_of_html() {
        assert_eq!(
            parse(&args("export")),
            Ok(Command::Export {
                format: Format::Html,
                days: 7,
                from: None,
                to: None,
                out: None,
                force: false
            })
        );
    }

    #[test]
    fn export_options_are_understood() {
        assert_eq!(
            parse(&args("export -f csv -p month -o /tmp/x.csv --force")),
            Ok(Command::Export {
                format: Format::Csv,
                days: 30,
                from: None,
                to: None,
                out: Some(PathBuf::from("/tmp/x.csv")),
                force: true
            })
        );
        assert_eq!(
            parse(&args("export --format markdown --period today")),
            Ok(Command::Export {
                format: Format::Markdown,
                days: 1,
                from: None,
                to: None,
                out: None,
                force: false
            })
        );
        // `year` is 365 days; `all` is 0, which means "from the first recorded day".
        assert!(matches!(
            parse(&args("export -p year")),
            Ok(Command::Export { days: 365, .. })
        ));
        assert!(matches!(
            parse(&args("export --period all")),
            Ok(Command::Export { days: 0, .. })
        ));
        assert!(matches!(
            parse(&args("export --days 45")),
            Ok(Command::Export { days: 45, .. })
        ));
    }

    #[test]
    fn export_accepts_a_range_of_dates() {
        let day = |text: &str| period::parse_date(text).unwrap();
        assert!(matches!(
            parse(&args("export --from 2026-11-02 --to 2026-11-08")),
            Ok(Command::Export { from, to, .. })
                if from == Some(day("2026-11-02")) && to == Some(day("2026-11-08"))
        ));
        // The end defaults to today, which is only known when the report is made.
        assert!(matches!(
            parse(&args("export --from 2026-11-02 -f csv")),
            Ok(Command::Export {
                from: Some(_),
                to: None,
                format: Format::Csv,
                ..
            })
        ));
    }

    #[test]
    fn bad_export_ranges_give_clear_messages() {
        assert!(parse(&args("export --to 2026-11-08"))
            .unwrap_err()
            .contains("--to needs --from"));
        assert!(parse(&args("export --from 2026-11-08 --to 2026-11-02"))
            .unwrap_err()
            .contains("must not be later"));
        assert!(parse(&args("export --from 2026-11-02 --days 7"))
            .unwrap_err()
            .contains("not both"));
        assert!(parse(&args("export --from 2026-11-02 -p week"))
            .unwrap_err()
            .contains("not both"));
        assert!(parse(&args("export --from tomorrow"))
            .unwrap_err()
            .contains("needs a date like 2026-11-03, not 'tomorrow'"));
        assert!(parse(&args("export --from"))
            .unwrap_err()
            .contains("needs a value"));
    }

    #[test]
    fn bad_export_options_give_clear_messages() {
        assert!(parse(&args("export -f pdf"))
            .unwrap_err()
            .contains("unknown format 'pdf'"));
        assert!(parse(&args("export -p decade"))
            .unwrap_err()
            .contains("unknown period 'decade'"));
        assert!(parse(&args("export --days 0"))
            .unwrap_err()
            .contains("1 to 3650"));
        assert!(parse(&args("export --days abc"))
            .unwrap_err()
            .contains("'abc'"));
        assert!(parse(&args("export --days"))
            .unwrap_err()
            .contains("needs a value"));
        assert!(parse(&args("export --bogus"))
            .unwrap_err()
            .contains("unknown option"));
    }

    #[test]
    fn backup_takes_an_optional_path() {
        assert_eq!(parse(&args("backup")), Ok(Command::Backup { out: None }));
        assert_eq!(
            parse(&args("backup --out /tmp/b.db")),
            Ok(Command::Backup {
                out: Some(PathBuf::from("/tmp/b.db"))
            })
        );
        assert!(parse(&args("backup --what")).is_err());
    }

    #[test]
    fn unknown_commands_are_rejected() {
        assert!(parse(&args("frobnicate"))
            .unwrap_err()
            .contains("unknown command"));
    }

    #[test]
    fn demo_data_options() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            parse(&a(&["demo-data"])),
            Ok(Command::DemoData { days: 35, seed: 1 })
        );
        assert_eq!(
            parse(&a(&["demo-data", "--days", "10", "--seed", "9"])),
            Ok(Command::DemoData { days: 10, seed: 9 })
        );
        assert!(parse(&a(&["demo-data", "--days", "0"])).is_err());
        assert!(parse(&a(&["demo-data", "--days", "9999"])).is_err());
        assert!(parse(&a(&["demo-data", "--seed", "x"])).is_err());
        assert!(parse(&a(&["demo-data", "--nope"])).is_err());
    }
}
