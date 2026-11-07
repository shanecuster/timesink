//! `timesink-zen-host`: the go-between for the optional Zen/Firefox extension.
//!
//! The browser starts this program itself ("native messaging") and talks to it over
//! stdin/stdout. Each message says which website host is in the active tab, or `null` when
//! nothing trackable is. The host reduces the host name to its registered site, drops blocked
//! sites, and records the result in the timesink database. Nothing leaves the computer and no
//! network port is opened.
//!
//! Commands you run yourself:
//! ```text
//! timesink-zen-host install            register with Zen and Firefox
//! timesink-zen-host uninstall          remove the registration
//! timesink-zen-host forget --all       delete all stored website time
//! timesink-zen-host forget github.com  delete one site's time
//! ```

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use timesink_core::db::Db;
use timesink_core::domains::{registered_domain, DomainConfig};
use timesink_core::paths;

/// The name the browser knows this helper by (also the manifest file name).
const HOST_NAME: &str = "timesink_zen";

/// The extension's id, which must match `extension/manifest.json`. Only this extension is
/// allowed to talk to the helper.
const EXTENSION_ID: &str = "timesink-sites@timesink";

/// Messages bigger than this are refused. Real ones are about 40 bytes; the cap stops a
/// corrupt length from asking for gigabytes of memory.
const MAX_MESSAGE_BYTES: usize = 64 * 1024;

/// What the extension reports: the host of the active tab, or `None`.
#[derive(Debug, PartialEq, Eq)]
struct Report {
    host: Option<String>,
}

/// Read one native-messaging frame: a 4-byte length (in the machine's byte order) followed by
/// that many bytes. `Ok(None)` means the browser closed the pipe.
fn read_frame<R: Read>(input: &mut R) -> io::Result<Option<Vec<u8>>> {
    let mut len_bytes = [0u8; 4];
    match input.read_exact(&mut len_bytes) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_ne_bytes(len_bytes) as usize;
    if len > MAX_MESSAGE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "message too large",
        ));
    }
    let mut body = vec![0u8; len];
    input.read_exact(&mut body)?;
    Ok(Some(body))
}

/// Write one frame (used for the small replies back to the browser).
fn write_frame<W: Write>(output: &mut W, body: &[u8]) -> io::Result<()> {
    output.write_all(&(body.len() as u32).to_ne_bytes())?;
    output.write_all(body)?;
    output.flush()
}

/// Understand a message body: `{"host": "github.com"}` or `{"host": null}`.
/// Anything else is an error and is ignored by the caller.
fn parse_report(body: &[u8]) -> Result<Report> {
    let value: serde_json::Value = serde_json::from_slice(body).context("not valid JSON")?;
    match value.get("host") {
        Some(serde_json::Value::Null) => Ok(Report { host: None }),
        Some(serde_json::Value::String(h)) => Ok(Report {
            host: Some(h.clone()),
        }),
        _ => bail!("message has no \"host\" field"),
    }
}

/// Turn a report into the site to record: reduced to the registered site, or `None` for
/// nothing / untrackable / blocked.
fn site_for(report: &Report, config: &DomainConfig) -> Option<String> {
    let site = registered_domain(report.host.as_deref()?)?;
    (!config.is_blocked(&site)).then_some(site)
}

/// Seconds since the Unix epoch now. The helper uses its own clock, not the extension's.
fn now_ts() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// The normal mode: the browser started us. Read reports until it closes the pipe.
fn run_host() -> Result<()> {
    let db = Db::open(&paths::db_path()?, &paths::device_id())?;
    // Each running browser starts its own helper; the process id keeps their stretches apart
    // so one browser going quiet never ends the other's.
    let source = std::process::id().to_string();
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let stdout = io::stdout();
    let mut output = stdout.lock();
    while let Some(body) = read_frame(&mut input).context("reading from the browser")? {
        // Bad messages and database hiccups are logged and skipped, never fatal: the browser
        // restarts a crashed helper only when the extension next connects.
        let result = parse_report(&body).and_then(|report| {
            // Re-read the config every time so edits to domains.conf apply without a restart.
            let config = paths::load_domain_config()?;
            db.record_domain(now_ts(), site_for(&report, &config).as_deref(), &source)
        });
        if let Err(e) = &result {
            eprintln!("timesink-zen-host: {e:#}");
        }
        // A tiny acknowledgement, so the extension can tell the helper is alive.
        let reply: &[u8] = if result.is_ok() {
            br#"{"ok":true}"#
        } else {
            br#"{"ok":false}"#
        };
        write_frame(&mut output, reply)?;
    }
    Ok(())
}

/// The manifest that tells the browser how to start this helper.
fn manifest_json(exe: &Path) -> String {
    serde_json::to_string_pretty(&serde_json::json!({
        "name": HOST_NAME,
        "description": "timesink: reports the active website's domain to the local database",
        "path": exe,
        "type": "stdio",
        "allowed_extensions": [EXTENSION_ID],
    }))
    .expect("a fixed JSON value always serialises")
}

/// Folders where Zen and Firefox look for native-messaging manifests.
fn manifest_dirs() -> Result<Vec<PathBuf>> {
    let home = PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?);
    Ok(vec![
        home.join(".mozilla/native-messaging-hosts"),
        home.join(".zen/native-messaging-hosts"),
    ])
}

/// Write the manifest into each browser folder. Returns the files written.
fn install() -> Result<Vec<PathBuf>> {
    let exe = std::env::current_exe().context("finding this program's own path")?;
    let text = manifest_json(&exe);
    let mut written = Vec::new();
    for dir in manifest_dirs()? {
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        let file = dir.join(format!("{HOST_NAME}.json"));
        std::fs::write(&file, &text).with_context(|| format!("writing {}", file.display()))?;
        written.push(file);
    }
    Ok(written)
}

/// Remove the manifests that `install` wrote. Missing files are fine.
fn uninstall() -> Result<Vec<PathBuf>> {
    let mut removed = Vec::new();
    for dir in manifest_dirs()? {
        let file = dir.join(format!("{HOST_NAME}.json"));
        match std::fs::remove_file(&file) {
            Ok(()) => removed.push(file),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("removing {}", file.display())),
        }
    }
    Ok(removed)
}

/// Describe one manifest file: missing, wrong target, or fine.
fn manifest_state(file: &Path) -> String {
    let Ok(text) = std::fs::read_to_string(file) else {
        return "missing (run: timesink-zen-host install)".into();
    };
    let target = serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|v| v["path"].as_str().map(PathBuf::from));
    match target {
        None => "unreadable (run: timesink-zen-host install)".into(),
        Some(p) if !p.exists() => format!(
            "points at {} which no longer exists (run: timesink-zen-host install)",
            p.display()
        ),
        Some(p) => format!("ok -> {}", p.display()),
    }
}

/// Human wording for "how long ago".
fn ago(secs: i64) -> String {
    match secs {
        s if s < 90 => format!("{s}s ago"),
        s if s < 5400 => format!("{} min ago", s / 60),
        s if s < 172_800 => format!("{} h ago", s / 3600),
        s => format!("{} days ago", s / 86_400),
    }
}

/// `status`: everything worth checking when site time seems not to be recording.
fn status() -> Result<()> {
    println!("manifests:");
    for dir in manifest_dirs()? {
        let file = dir.join(format!("{HOST_NAME}.json"));
        println!("  {}: {}", file.display(), manifest_state(&file));
    }
    let config = paths::load_domain_config()?;
    println!("browsers counted: {}", config.browsers.join(", "));
    let db = Db::open(&paths::db_path()?, &paths::device_id())?;
    match db.latest_domain_report()? {
        Some((site, end)) => println!(
            "last report: {site}, {} (a quiet stretch is normal if you were not browsing)",
            ago((now_ts() - end).max(0))
        ),
        None => println!("last report: none yet"),
    }
    println!(
        "If reports stopped: check the extension is enabled at about:addons, that it is the \
         signed build (temporary add-ons vanish on restart), and run `install` again after \
         moving or reinstalling this program."
    );
    Ok(())
}

/// Text shown by `help`.
const USAGE: &str = "\
timesink-zen-host: receives website reports from the timesink browser extension.

  install              register with Zen and Firefox (run once, after installing)
  uninstall            remove that registration
  status               check the registration and whether reports are arriving
  forget --all         delete all stored website time
  forget <site>        delete one site's time, e.g. forget github.com
  help                 this text

Run with no arguments the browser starts it; you do not need to.
";

fn main() {
    if let Err(e) = start() {
        eprintln!("timesink-zen-host: {e:#}");
        std::process::exit(1);
    }
}

/// Pick what to do from the command line.
fn start() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("install") => {
            for f in install()? {
                println!("wrote {}", f.display());
            }
            println!("Now load the extension in Zen (see the README).");
            Ok(())
        }
        Some("uninstall") => {
            for f in uninstall()? {
                println!("removed {}", f.display());
            }
            Ok(())
        }
        Some("status") => status(),
        Some("forget") => {
            let db = Db::open(&paths::db_path()?, &paths::device_id())?;
            let n = match args.get(1).map(String::as_str) {
                Some("--all") => db.forget_domains(None)?,
                Some(site) => {
                    let site = registered_domain(site).context("that is not a site name")?;
                    db.forget_domains(Some(&site))?
                }
                None => bail!("say which: forget --all, or forget <site>"),
            };
            println!("deleted {n} rows");
            Ok(())
        }
        Some("help" | "--help" | "-h") => {
            print!("{USAGE}");
            Ok(())
        }
        // The browser starts the helper with the manifest path and extension id as arguments.
        _ => run_host(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a frame the way a browser does.
    fn frame(body: &str) -> Vec<u8> {
        let mut out = (body.len() as u32).to_ne_bytes().to_vec();
        out.extend_from_slice(body.as_bytes());
        out
    }

    #[test]
    fn frames_round_trip_and_eof_is_a_clean_end() {
        let mut data = frame(r#"{"host":"a.com"}"#);
        data.extend(frame(r#"{"host":null}"#));
        let mut input = io::Cursor::new(data);
        assert_eq!(
            read_frame(&mut input).unwrap().unwrap(),
            br#"{"host":"a.com"}"#
        );
        assert_eq!(
            read_frame(&mut input).unwrap().unwrap(),
            br#"{"host":null}"#
        );
        assert!(read_frame(&mut input).unwrap().is_none());

        let mut out = Vec::new();
        write_frame(&mut out, b"hi").unwrap();
        assert_eq!(out, frame("hi"));
    }

    #[test]
    fn oversized_or_truncated_frames_are_errors_not_crashes() {
        let mut big = io::Cursor::new(u32::MAX.to_ne_bytes().to_vec());
        assert!(read_frame(&mut big).is_err());
        // Says 10 bytes, delivers 2.
        let mut short = (10u32).to_ne_bytes().to_vec();
        short.extend_from_slice(b"ab");
        assert!(read_frame(&mut io::Cursor::new(short)).is_err());
    }

    #[test]
    fn reports_parse_and_garbage_is_rejected() {
        assert_eq!(
            parse_report(br#"{"host":"gist.github.com"}"#).unwrap(),
            Report {
                host: Some("gist.github.com".into())
            }
        );
        assert_eq!(
            parse_report(br#"{"host":null}"#).unwrap(),
            Report { host: None }
        );
        assert!(parse_report(b"not json").is_err());
        assert!(parse_report(br#"{"url":"https://x.com/secret"}"#).is_err());
        assert!(parse_report(br#"{"host":5}"#).is_err());
    }

    #[test]
    fn only_the_registered_site_is_ever_recorded() {
        let config = DomainConfig::from_conf("block = bank.example\n");
        let site = |h: &str| {
            site_for(
                &Report {
                    host: Some(h.into()),
                },
                &config,
            )
        };
        assert_eq!(site("gist.github.com").as_deref(), Some("github.com"));
        // Blocked sites (and their subdomains) are dropped entirely.
        assert_eq!(site("login.bank.example"), None);
        assert_eq!(site("localhost"), None);
        assert_eq!(site_for(&Report { host: None }, &config), None);
    }

    #[test]
    fn manifest_state_spots_missing_and_stale_targets() {
        let dir = std::env::temp_dir().join(format!("timesink-ms-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(manifest_state(&dir.join("nope.json")).starts_with("missing"));
        let f = dir.join("m.json");
        std::fs::write(&f, manifest_json(Path::new("/no/such/exe"))).unwrap();
        assert!(manifest_state(&f).contains("no longer exists"));
        std::fs::write(&f, manifest_json(&std::env::current_exe().unwrap())).unwrap();
        assert!(manifest_state(&f).starts_with("ok"));
        assert_eq!(ago(30), "30s ago");
        assert_eq!(ago(600), "10 min ago");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn manifest_names_the_helper_the_path_and_only_our_extension() {
        let text = manifest_json(Path::new("/home/me/.cargo/bin/timesink-zen-host"));
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["name"], HOST_NAME);
        assert_eq!(v["type"], "stdio");
        assert_eq!(v["path"], "/home/me/.cargo/bin/timesink-zen-host");
        assert_eq!(v["allowed_extensions"], serde_json::json!([EXTENSION_ID]));
    }
}
