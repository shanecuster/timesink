//! The non-interactive commands: writing reports and backing up the database.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use timesink_core::appmap::AppMap;
use timesink_core::db::Db;
use timesink_core::paths;
use timesink_core::stats;

use crate::data;
use crate::export::{format_time, Format, ReportData};

/// Short word for a period in file names: `today`, `week`, `month`, `45days`.
fn period_slug(days: i64) -> String {
    match days {
        1 => "today".to_string(),
        7 => "week".to_string(),
        30 => "month".to_string(),
        n => format!("{n}days"),
    }
}

/// The folder exports and backups go to when no path is given:
/// `~/Documents/timesink-exports`.
pub fn default_dir() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home)
        .join("Documents")
        .join("timesink-exports"))
}

/// The first of `stem.ext`, `stem-2.ext`, `stem-3.ext`... that does not exist
/// in `dir`, so nothing is ever overwritten by accident.
pub fn free_path(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let first = dir.join(format!("{stem}.{ext}"));
    if !first.exists() {
        return first;
    }
    (2..)
        .map(|n| dir.join(format!("{stem}-{n}.{ext}")))
        .find(|p| !p.exists())
        .expect("an unused file name always exists")
}

/// Decide the file to write.
///
/// * no `out`: a free name in the default folder (created if needed);
/// * `out` is a folder (it exists, ends with `/`, or has no file extension):
///   a free name inside it, creating the folder if needed;
/// * `out` is a file path (has an extension): that file, which must not
///   exist unless `force`.
pub fn resolve_output(
    out: Option<&Path>,
    default_dir: &Path,
    stem: &str,
    ext: &str,
    force: bool,
) -> Result<PathBuf> {
    let (dir, file) = match out {
        None => (Some(default_dir.to_path_buf()), None),
        Some(p)
            if p.is_dir()
                || p.as_os_str().to_string_lossy().ends_with('/')
                || p.extension().is_none() =>
        {
            (Some(p.to_path_buf()), None)
        }
        Some(p) => (None, Some(p.to_path_buf())),
    };
    if let Some(dir) = dir {
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("creating folder {}", dir.display()))?;
        return Ok(free_path(&dir, stem, ext));
    }
    let file = file.expect("either a folder or a file was chosen");
    if let Some(parent) = file.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating folder {}", parent.display()))?;
    }
    if file.exists() && !force {
        bail!(
            "{} already exists (use --force to replace it)",
            file.display()
        );
    }
    Ok(file)
}

/// Write `text` to `path`. An existing file is only replaced when `replace`
/// is set; otherwise creating it fails rather than overwriting.
pub fn write_file(path: &Path, text: &str, replace: bool) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true);
    if replace {
        options.create(true).truncate(true);
    } else {
        options.create_new(true);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("writing {}", path.display()))?;
    file.write_all(text.as_bytes())
        .with_context(|| format!("writing {}", path.display()))
}

/// Build a report for the last `days` days from the database.
pub fn collect(
    db: &Db,
    map: &AppMap,
    days: i64,
    now: i64,
    tz: &dyn Fn(i64) -> i64,
) -> Result<ReportData> {
    let today = stats::local_day(now, tz);
    let from = stats::day_start_utc(today - (days.max(1) - 1), tz);
    let intervals = db.intervals_between(from, now + 1)?;
    let listening = db.listening_between(from, now + 1)?;
    let report = ReportData::build(&intervals, &listening, map, days, now, tz);
    // Website time, only when the browser extension has recorded some.
    let sites = db.domains_between(from, now + 1)?;
    if sites.is_empty() {
        return Ok(report);
    }
    let config = paths::load_domain_config().unwrap_or_default();
    let spans = data::site_spans(&sites, &intervals, &config);
    Ok(report.with_sites(&spans, &config, days, now, tz))
}

/// Local date of `now` as `YYYY-MM-DD`, for file names.
fn local_date(now: i64, tz: &dyn Fn(i64) -> i64) -> String {
    format_time(now, tz).chars().take(10).collect()
}

/// What to export and where to put it.
pub struct ExportRequest<'a> {
    /// File format.
    pub format: Format,
    /// Number of days, ending today.
    pub days: i64,
    /// Where to write; the default folder is used when absent.
    pub out: Option<&'a Path>,
    /// Replace an existing file.
    pub force: bool,
}

/// `timesink export`: write a report and return the file it went to.
pub fn run_export(
    db: &Db,
    map: &AppMap,
    request: &ExportRequest,
    now: i64,
    tz: &dyn Fn(i64) -> i64,
) -> Result<PathBuf> {
    let data = collect(db, map, request.days, now, tz)?;
    let stem = format!(
        "timesink-{}-{}",
        period_slug(request.days),
        local_date(now, tz)
    );
    let path = resolve_output(
        request.out,
        &default_dir()?,
        &stem,
        request.format.extension(),
        request.force,
    )?;
    // `force` only matters for an explicit file; free names never exist.
    write_file(&path, &request.format.render(&data), request.force)?;
    Ok(path)
}

/// `timesink backup`: copy the database to a dated file and return its path.
pub fn run_backup(
    db: &Db,
    out: Option<&Path>,
    now: i64,
    tz: &dyn Fn(i64) -> i64,
) -> Result<PathBuf> {
    // e.g. 2026-11-07T05:30:00-05:00 -> 20261107-053000
    let stamp: String = format_time(now, tz)
        .chars()
        .take(19)
        .filter(|c| c.is_ascii_digit() || *c == 'T')
        .map(|c| if c == 'T' { '-' } else { c })
        .collect();
    let stem = format!("timesink-backup-{stamp}");
    let path = resolve_output(out, &default_dir()?, &stem, "db", false)?;
    db.backup_to(&path)
        .with_context(|| format!("writing backup {}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh empty folder for one test.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("timesink-cmd-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn free_names_count_up_instead_of_overwriting() {
        let dir = scratch("free");
        assert_eq!(free_path(&dir, "r", "csv"), dir.join("r.csv"));
        std::fs::write(dir.join("r.csv"), "x").unwrap();
        assert_eq!(free_path(&dir, "r", "csv"), dir.join("r-2.csv"));
        std::fs::write(dir.join("r-2.csv"), "x").unwrap();
        assert_eq!(free_path(&dir, "r", "csv"), dir.join("r-3.csv"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn default_folder_is_created_and_used() {
        let base = scratch("default");
        let dflt = base.join("exports");
        let p = resolve_output(None, &dflt, "stem", "md", false).unwrap();
        assert_eq!(p, dflt.join("stem.md"));
        assert!(dflt.is_dir());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn an_output_folder_gets_a_generated_name() {
        let dir = scratch("folder");
        let p = resolve_output(Some(&dir), Path::new("/unused"), "stem", "json", false).unwrap();
        assert_eq!(p, dir.join("stem.json"));
        // A trailing slash on a folder that does not exist yet also means a folder.
        let new = dir.join("sub/");
        let p = resolve_output(Some(&new), Path::new("/unused"), "stem", "json", false).unwrap();
        assert_eq!(p, dir.join("sub").join("stem.json"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_path_without_an_extension_is_a_folder_and_one_with_is_a_file() {
        let dir = scratch("ext");
        let folder = dir.join("reports");
        let p = resolve_output(Some(&folder), Path::new("/unused"), "stem", "csv", false).unwrap();
        assert_eq!(p, folder.join("stem.csv"));
        assert!(folder.is_dir());
        let file = dir.join("deep/er/mine.csv");
        let p = resolve_output(Some(&file), Path::new("/unused"), "stem", "csv", false).unwrap();
        assert_eq!(p, file);
        assert!(dir.join("deep/er").is_dir());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_existing_file_is_protected_unless_forced() {
        let dir = scratch("force");
        let file = dir.join("mine.csv");
        std::fs::write(&file, "keep me").unwrap();
        let err = resolve_output(Some(&file), Path::new("/unused"), "s", "csv", false)
            .unwrap_err()
            .to_string();
        assert!(err.contains("already exists"), "{err}");
        assert_eq!(
            resolve_output(Some(&file), Path::new("/unused"), "s", "csv", true).unwrap(),
            file
        );
        // write_file refuses to overwrite unless told to.
        assert!(write_file(&file, "new", false).is_err());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "keep me");
        write_file(&file, "new", true).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "new");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn period_names_are_short_words() {
        assert_eq!(period_slug(1), "today");
        assert_eq!(period_slug(7), "week");
        assert_eq!(period_slug(30), "month");
        assert_eq!(period_slug(45), "45days");
    }

    /// Five hours behind UTC.
    fn minus5(_: i64) -> i64 {
        -5 * 3600
    }

    #[test]
    fn export_and_backup_write_real_files() {
        use timesink_core::db::Recorder;
        use timesink_core::listen::ListenEffect;
        let dir = scratch("e2e");

        // A database with one listening row.
        let db_path = dir.join("t.db");
        let now = 100 * 86400 + 20 * 3600;
        {
            let mut rec = Recorder::new(Db::open(&db_path, "box").unwrap());
            rec.apply_listen(&[
                ListenEffect::Opened {
                    start_ts: now - 600,
                    player: "cider".into(),
                },
                ListenEffect::Closed { end_ts: now - 60 },
            ])
            .unwrap();
        }
        let db = Db::open(&db_path, "box").unwrap();
        let map = AppMap::default();

        let out = dir.join("exports");
        let request = ExportRequest {
            format: Format::Csv,
            days: 7,
            out: Some(&out),
            force: false,
        };
        let csv = run_export(&db, &map, &request, now, &minus5).unwrap();
        assert_eq!(csv, out.join("timesink-week-1970-04-11.csv"));
        let text = std::fs::read_to_string(&csv).unwrap();
        assert!(text.contains("listening"));
        assert!(text.contains("cider"));

        // A second export the same day gets a new name, never overwriting.
        let again = run_export(&db, &map, &request, now, &minus5).unwrap();
        assert_eq!(again, out.join("timesink-week-1970-04-11-2.csv"));

        let backup = run_backup(&db, Some(&out), now, &minus5).unwrap();
        assert_eq!(backup, out.join("timesink-backup-19700411-150000.db"));
        // The backup is a real, readable database with the same data.
        let copy = Db::open(&backup, "box").unwrap();
        assert_eq!(
            copy.listening_since(0).unwrap(),
            db.listening_since(0).unwrap()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
