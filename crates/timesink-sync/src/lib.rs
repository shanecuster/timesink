//! Syncing with CouchDB: `timesink sync`.
//!
//! Each computer uploads its own rows as one document per day and downloads the other
//! computers' documents (see `timesink_core::syncdoc`). Because every document has exactly
//! one writer there are no conflicts to resolve. Window titles never leave the computer.
//!
//! The work is split in two: [`run`] holds the sync logic and talks to a [`Remote`], and
//! [`HttpRemote`] is the real CouchDB over HTTP. Tests use a simulated CouchDB instead.

use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use timesink_core::db::Db;
use timesink_core::syncdoc::{self, DayRows};

/// Where to find the CouchDB server and how to log in (from `sync.conf`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncConfig {
    /// Server address, e.g. `http://host:5984` (no trailing slash).
    pub url: String,
    /// Database name on the server.
    pub database: String,
    /// CouchDB user.
    pub user: String,
    /// CouchDB password.
    pub password: String,
}

impl SyncConfig {
    /// Parse `sync.conf`: `key = value` lines (`url`, `database`, `user`, `password`),
    /// `#` starts a comment. Every field is required.
    pub fn parse(text: &str) -> Result<SyncConfig> {
        let (mut url, mut database, mut user, mut password) = (None, None, None, None);
        for (n, raw) in text.lines().enumerate() {
            // Strip comments only at the start of a line, so a '#' inside a password is kept.
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (key, value) = line
                .split_once('=')
                .ok_or_else(|| anyhow!("sync.conf line {}: expected 'key = value'", n + 1))?;
            let value = value.trim().to_string();
            match key.trim() {
                "url" => url = Some(value),
                "database" => database = Some(value),
                "user" => user = Some(value),
                "password" => password = Some(value),
                other => bail!("sync.conf line {}: unknown setting '{other}'", n + 1),
            }
        }
        let need = |v: Option<String>, name: &str| {
            v.filter(|s| !s.is_empty())
                .ok_or_else(|| anyhow!("sync.conf is missing '{name}'"))
        };
        let url = need(url, "url")?.trim_end_matches('/').to_string();
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            bail!("sync.conf: url must start with http:// or https://");
        }
        let database = need(database, "database")?;
        // CouchDB database names: lowercase letters, digits and _$()+- ; starting with a letter.
        // Checking it here also keeps odd characters out of the request path.
        let valid = database
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase())
            && database
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "_$()+-".contains(c));
        if !valid {
            bail!("sync.conf: database must be lowercase letters, digits and _$()+- (starting with a letter)");
        }
        Ok(SyncConfig {
            url,
            database,
            user: need(user, "user")?,
            password: need(password, "password")?,
        })
    }
}

/// A warning when `sync.conf` can be read by other users (it holds a password).
#[cfg(unix)]
pub fn permission_warning(path: &std::path::Path) -> Option<String> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(path).ok()?.permissions().mode();
    (mode & 0o077 != 0).then(|| {
        format!(
            "{} is readable by other users; run: chmod 600 {}",
            path.display(),
            path.display()
        )
    })
}

/// No permission bits to check on other systems.
#[cfg(not(unix))]
pub fn permission_warning(_path: &std::path::Path) -> Option<String> {
    None
}

/// One entry of CouchDB's changes feed.
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    /// Document id.
    pub id: String,
    /// The document's current revision.
    pub rev: String,
    /// True when the document was deleted.
    pub deleted: bool,
    /// The document body (absent for deletions).
    pub doc: Option<Value>,
}

/// The result of storing one document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PutResult {
    /// Stored; this is the new revision.
    Ok(String),
    /// Someone else changed the document since the revision we sent.
    Conflict,
    /// Anything else, with CouchDB's reason.
    Failed(String),
}

/// What syncing needs from a CouchDB server.
pub trait Remote {
    /// Make sure the database exists (creating it when the user may).
    fn ensure_db(&mut self) -> Result<()>;
    /// Store several documents at once; one result per document, in order.
    fn put_docs(&mut self, docs: &[Value]) -> Result<Vec<PutResult>>;
    /// The current revision of a document, or `None` if it does not exist.
    fn current_rev(&mut self, id: &str) -> Result<Option<String>>;
    /// Changes after `since`, at most `limit`; returns them with the new position.
    fn changes(&mut self, since: &str, limit: usize) -> Result<(Vec<Change>, String)>;
}

/// What a sync run did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// Days of this computer uploaded.
    pub uploaded: usize,
    /// Days removed from the server because they no longer have any rows here.
    pub removed: usize,
    /// Days of other computers downloaded (or removed).
    pub downloaded: usize,
    /// Things that were skipped or went wrong, in words.
    pub warnings: Vec<String>,
}

/// How many changes are fetched per request.
const BATCH: usize = 100;

/// Upload this computer's changed days, then download everyone else's.
pub fn run(db: &Db, remote: &mut dyn Remote) -> Result<Report> {
    let mut report = Report::default();
    remote.ensure_db()?;
    upload(db, remote, &mut report)?;
    download(db, remote, &mut report)?;
    Ok(report)
}

/// Upload every day marked as changed since the last upload.
fn upload(db: &Db, remote: &mut dyn Remote, report: &mut Report) -> Result<()> {
    let own = db.device_id().to_string();
    // The marks are cleared first, so changes made during the upload mark their day again;
    // anything that fails below puts its mark back.
    let days = db.take_dirty_days()?;
    if days.is_empty() {
        return Ok(());
    }
    // Build one request per day: the document, whether it is a deletion, and the id.
    struct Job {
        day: i64,
        id: String,
        rows: DayRows,
    }
    let mut jobs = Vec::new();
    for day in &days {
        let rows = db.day_rows(*day)?;
        jobs.push(Job {
            day: *day,
            id: syncdoc::doc_id(&own, *day),
            rows,
        });
    }
    // Days with no rows are only worth sending if the server has a document to remove.
    let mut docs = Vec::new();
    let mut sent: Vec<usize> = Vec::new();
    for (i, job) in jobs.iter().enumerate() {
        let rev = db.doc_rev(&job.id)?;
        if job.rows.is_empty() {
            if let Some(rev) = rev {
                docs.push(json!({"_id": job.id, "_rev": rev, "_deleted": true}));
                sent.push(i);
            }
        } else {
            docs.push(syncdoc::build_doc(&own, job.day, &job.rows, rev.as_deref()));
            sent.push(i);
        }
    }
    if docs.is_empty() {
        return Ok(());
    }
    let results = match remote.put_docs(&docs) {
        Ok(r) if r.len() == docs.len() => r,
        other => {
            // Nothing is known to have been stored: mark every day again and stop.
            for job in &jobs {
                db.mark_dirty(job.day)?;
            }
            return Err(match other {
                Ok(_) => anyhow!("the server answered with the wrong number of results"),
                Err(e) => e,
            });
        }
    };
    // Days that needed no request are done; the rest are settled one by one.
    for (n, i) in sent.iter().enumerate() {
        let job = &jobs[*i];
        let deleting = job.rows.is_empty();
        let mut result = results[n].clone();
        // A conflict means the server holds a newer revision than we remembered (for
        // example after restoring the database). Learn it and try once more.
        if result == PutResult::Conflict {
            let rev = remote.current_rev(&job.id)?;
            let retry = if deleting {
                rev.map(|r| json!({"_id": job.id, "_rev": r, "_deleted": true}))
            } else {
                Some(syncdoc::build_doc(&own, job.day, &job.rows, rev.as_deref()))
            };
            result = match retry {
                Some(doc) => remote.put_docs(&[doc])?.remove(0),
                // Already gone from the server: nothing left to do.
                None => PutResult::Ok(String::new()),
            };
        }
        match result {
            PutResult::Ok(rev) => {
                if deleting {
                    db.set_doc_rev(&job.id, None)?;
                    report.removed += 1;
                } else {
                    db.set_doc_rev(&job.id, Some(&rev))?;
                    report.uploaded += 1;
                }
            }
            PutResult::Conflict => {
                db.mark_dirty(job.day)?;
                report
                    .warnings
                    .push(format!("{}: still in conflict, will retry", job.id));
            }
            PutResult::Failed(why) => {
                db.mark_dirty(job.day)?;
                report.warnings.push(format!("{}: {why}", job.id));
            }
        }
    }
    Ok(())
}

/// Download changes made by other computers and apply them.
fn download(db: &Db, remote: &mut dyn Remote, report: &mut Report) -> Result<()> {
    let own = db.device_id().to_string();
    let mut since = db.sync_state("seq")?.unwrap_or_else(|| "0".to_string());
    loop {
        let (changes, last) = remote.changes(&since, BATCH)?;
        for change in &changes {
            // Only day documents matter; design documents and the like are skipped.
            let Some((device, day)) = syncdoc::parse_doc_id(&change.id) else {
                continue;
            };
            if device == own {
                // Our own document: just learn its revision, so later uploads can update it.
                let rev = (!change.deleted).then_some(change.rev.as_str());
                db.set_doc_rev(&change.id, rev)?;
                continue;
            }
            if change.deleted {
                db.replace_day(&device, day, &DayRows::default())?;
                report.downloaded += 1;
                continue;
            }
            // A malformed document is skipped with a message, never half-applied.
            let parsed = change
                .doc
                .as_ref()
                .ok_or_else(|| anyhow!("no body"))
                .and_then(syncdoc::parse_doc);
            match parsed {
                Ok((dev, d, rows)) if dev == device && d == day => {
                    db.replace_day(&device, day, &rows)?;
                    report.downloaded += 1;
                }
                Ok(_) => report.warnings.push(format!(
                    "{}: contents do not match its id, skipped",
                    change.id
                )),
                Err(e) => report.warnings.push(format!("{}: {e}, skipped", change.id)),
            }
        }
        // Remember how far we got, so the next run starts here.
        db.set_sync_state("seq", &last)?;
        since = last;
        if changes.len() < BATCH {
            return Ok(());
        }
    }
}

/// Write `text` for use as one path segment or query value (percent-encoding everything
/// except letters, digits and `-._~`).
pub fn pct_encode(text: &str) -> String {
    let mut out = String::new();
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The real CouchDB, over HTTP with basic authentication.
pub struct HttpRemote {
    /// Where the server is and how to log in.
    config: SyncConfig,
    /// Shared connection settings (timeouts).
    agent: ureq::Agent,
    /// The prepared `Authorization` header value.
    auth: String,
}

impl HttpRemote {
    /// Prepare a connection (nothing is sent yet).
    pub fn new(config: SyncConfig) -> Self {
        use base64::Engine;
        let auth = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD
                .encode(format!("{}:{}", config.user, config.password))
        );
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(10))
            .timeout(Duration::from_secs(60))
            .build();
        HttpRemote {
            config,
            agent,
            auth,
        }
    }

    /// `<server>/<database><path>`.
    fn url(&self, path: &str) -> String {
        format!("{}/{}{}", self.config.url, self.config.database, path)
    }

    /// Turn an HTTP failure into a message that says what to check.
    fn describe(err: ureq::Error) -> anyhow::Error {
        match err {
            ureq::Error::Status(401, _) => {
                anyhow!("the server refused the login (401): check user and password in sync.conf")
            }
            ureq::Error::Status(403, _) => {
                anyhow!("the server says this user is not allowed to do that (403)")
            }
            ureq::Error::Status(code, resp) => {
                let body = resp.into_string().unwrap_or_default();
                anyhow!("the server answered {code}: {}", body.trim())
            }
            ureq::Error::Transport(t) => anyhow!("could not reach the server: {t}"),
        }
    }
}

impl Remote for HttpRemote {
    fn ensure_db(&mut self) -> Result<()> {
        let get = self
            .agent
            .get(&self.url(""))
            .set("Authorization", &self.auth)
            .call();
        match get {
            Ok(_) => Ok(()),
            // Missing: create it (works when the user is allowed to; 412 means it exists).
            Err(ureq::Error::Status(404, _)) => {
                match self
                    .agent
                    .put(&self.url(""))
                    .set("Authorization", &self.auth)
                    .call()
                {
                    Ok(_) | Err(ureq::Error::Status(412, _)) => Ok(()),
                    Err(e) => Err(Self::describe(e)).context("creating the database"),
                }
            }
            Err(e) => Err(Self::describe(e)),
        }
    }

    fn put_docs(&mut self, docs: &[Value]) -> Result<Vec<PutResult>> {
        let resp: Value = self
            .agent
            .post(&self.url("/_bulk_docs"))
            .set("Authorization", &self.auth)
            .send_json(json!({ "docs": docs }))
            .map_err(Self::describe)?
            .into_json()
            .context("reading the upload answer")?;
        let list = resp.as_array().context("unexpected upload answer")?;
        Ok(list
            .iter()
            .map(|item| match (item["ok"].as_bool(), item["rev"].as_str()) {
                (Some(true), Some(rev)) => PutResult::Ok(rev.to_string()),
                _ if item["error"].as_str() == Some("conflict") => PutResult::Conflict,
                _ => PutResult::Failed(format!(
                    "{}: {}",
                    item["error"].as_str().unwrap_or("error"),
                    item["reason"].as_str().unwrap_or("")
                )),
            })
            .collect())
    }

    fn current_rev(&mut self, id: &str) -> Result<Option<String>> {
        match self
            .agent
            .get(&self.url(&format!("/{}", pct_encode(id))))
            .set("Authorization", &self.auth)
            .call()
        {
            Ok(resp) => {
                let doc: Value = resp.into_json().context("reading the document")?;
                Ok(doc["_rev"].as_str().map(str::to_string))
            }
            Err(ureq::Error::Status(404, _)) => Ok(None),
            Err(e) => Err(Self::describe(e)),
        }
    }

    fn changes(&mut self, since: &str, limit: usize) -> Result<(Vec<Change>, String)> {
        let resp: Value = self
            .agent
            .get(&self.url(&format!(
                "/_changes?since={}&limit={limit}&include_docs=true",
                pct_encode(since)
            )))
            .set("Authorization", &self.auth)
            .call()
            .map_err(Self::describe)?
            .into_json()
            .context("reading the changes")?;
        let results = resp["results"]
            .as_array()
            .context("unexpected changes answer")?;
        let changes = results
            .iter()
            .filter_map(|r| {
                Some(Change {
                    id: r["id"].as_str()?.to_string(),
                    rev: r["changes"][0]["rev"].as_str()?.to_string(),
                    deleted: r["deleted"].as_bool().unwrap_or(false),
                    doc: r.get("doc").filter(|d| !d.is_null()).cloned(),
                })
            })
            .collect();
        // The change number can be a string (CouchDB 2+) or a number (1.x).
        let last = match &resp["last_seq"] {
            Value::String(s) => s.clone(),
            Value::Null => since.to_string(),
            other => other.to_string(),
        };
        Ok((changes, last))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    use timesink_core::model::WindowSnapshot;
    use timesink_core::syncdoc::DAY_SECS;

    use super::*;

    /// A stand-in for CouchDB that follows the rules sync depends on: updates need the
    /// current revision, a changes feed lists each document once at its latest change, and
    /// deleted documents are reported as deleted.
    #[derive(Default)]
    struct MemRemote {
        /// Documents by id: (revision number, revision text, body, deleted).
        docs: HashMap<String, (u32, String, Value, bool)>,
        /// Change log: (sequence number, document id).
        log: Vec<(u64, String)>,
        /// When set, every upload fails like a dropped connection.
        fail_puts: bool,
        /// How many upload requests were made.
        put_calls: usize,
    }

    impl MemRemote {
        /// Store a document as CouchDB would; returns the result.
        fn store(&mut self, doc: &Value) -> PutResult {
            let id = doc["_id"].as_str().unwrap().to_string();
            let sent_rev = doc["_rev"].as_str();
            let deleted = doc["_deleted"].as_bool().unwrap_or(false);
            let (num, current) = match self.docs.get(&id) {
                Some((n, rev, _, was_deleted)) => (*n, Some((rev.clone(), *was_deleted))),
                None => (0, None),
            };
            let allowed = match (&current, sent_rev) {
                (None, None) => true,
                (Some((_, true)), None) => true,
                (Some((rev, _)), Some(sent)) => rev == sent,
                _ => false,
            };
            if !allowed {
                return PutResult::Conflict;
            }
            let seq = self.log.len() as u64 + 1;
            let rev = format!("{}-r{seq}", num + 1);
            self.docs
                .insert(id.clone(), (num + 1, rev.clone(), doc.clone(), deleted));
            self.log.push((seq, id));
            PutResult::Ok(rev)
        }

        /// Put a document straight into the server (as if another computer had).
        fn seed(&mut self, doc: Value) {
            assert!(matches!(self.store(&doc), PutResult::Ok(_)));
        }
    }

    impl Remote for MemRemote {
        fn ensure_db(&mut self) -> Result<()> {
            Ok(())
        }
        fn put_docs(&mut self, docs: &[Value]) -> Result<Vec<PutResult>> {
            self.put_calls += 1;
            if self.fail_puts {
                bail!("could not reach the server");
            }
            Ok(docs.iter().map(|d| self.store(d)).collect())
        }
        fn current_rev(&mut self, id: &str) -> Result<Option<String>> {
            Ok(self
                .docs
                .get(id)
                .filter(|(_, _, _, deleted)| !deleted)
                .map(|(_, rev, _, _)| rev.clone()))
        }
        fn changes(&mut self, since: &str, limit: usize) -> Result<(Vec<Change>, String)> {
            let since: u64 = since.parse().unwrap_or(0);
            // Each document appears once, at its latest sequence number.
            let mut latest: HashMap<&str, u64> = HashMap::new();
            for (seq, id) in &self.log {
                latest.insert(id, *seq);
            }
            let mut pending: Vec<(u64, &str)> = latest
                .iter()
                .filter(|(_, seq)| **seq > since)
                .map(|(id, seq)| (*seq, *id))
                .collect();
            pending.sort();
            pending.truncate(limit);
            let last = pending.last().map_or(since, |(seq, _)| *seq);
            let changes = pending
                .iter()
                .map(|(_, id)| {
                    let (_, rev, doc, deleted) = &self.docs[*id];
                    Change {
                        id: id.to_string(),
                        rev: rev.clone(),
                        deleted: *deleted,
                        doc: (!deleted).then(|| doc.clone()),
                    }
                })
                .collect();
            // With nothing pending the position is the end of the log.
            let last = if last == since {
                since.max(self.log.len() as u64)
            } else {
                last
            };
            Ok((changes, last.to_string()))
        }
    }

    /// Record a focus row on `db` that starts at `start` and lasts `secs`.
    fn record(db: &Db, start: i64, secs: i64, app: &str) {
        let id = db
            .insert_open(
                start,
                &WindowSnapshot {
                    app_class: app.into(),
                    title: "secret title".into(),
                    workspace: "1".into(),
                },
            )
            .unwrap();
        db.set_end(id, start + secs).unwrap();
    }

    /// Everything in a database as comparable (device, app, start, end) tuples.
    fn rows(db: &Db) -> Vec<(String, String, i64, i64)> {
        db.intervals_between(0, i64::MAX / 2, None)
            .unwrap()
            .into_iter()
            .map(|r| (r.device_id, r.app_class, r.start_ts, r.end_ts))
            .collect()
    }

    #[test]
    fn the_first_sync_uploads_each_day_without_titles_and_the_second_sends_nothing() {
        let db = Db::open_in_memory("mangrove").unwrap();
        record(&db, 5 * DAY_SECS + 100, 60, "zen");
        record(&db, 6 * DAY_SECS + 100, 90, "kitty");
        db.insert_listen_open(6 * DAY_SECS + 100, "spotify")
            .unwrap();
        let mut remote = MemRemote::default();

        let report = run(&db, &mut remote).unwrap();
        assert_eq!(
            (report.uploaded, report.removed, report.downloaded),
            (2, 0, 0)
        );
        assert!(report.warnings.is_empty());
        let doc = &remote.docs["day:mangrove:1970-01-07"].2;
        assert_eq!(doc["intervals"][0][2], "kitty");
        assert_eq!(doc["listening"][0][2], "spotify");
        // Titles never leave the computer.
        for (_, _, doc, _) in remote.docs.values() {
            assert!(!doc.to_string().contains("secret title"));
        }
        // Nothing changed, so nothing is sent.
        let calls = remote.put_calls;
        let again = run(&db, &mut remote).unwrap();
        assert_eq!((again.uploaded, again.downloaded), (0, 0));
        assert_eq!(remote.put_calls, calls);
    }

    #[test]
    fn two_computers_end_up_with_each_others_rows_and_do_not_echo_forever() {
        let a = Db::open_in_memory("mangrove").unwrap();
        let b = Db::open_in_memory("laptop").unwrap();
        record(&a, 10 * DAY_SECS + 10, 100, "zen");
        record(&b, 10 * DAY_SECS + 500, 200, "kitty");
        let mut remote = MemRemote::default();

        run(&a, &mut remote).unwrap();
        let rb = run(&b, &mut remote).unwrap();
        assert_eq!((rb.uploaded, rb.downloaded), (1, 1));
        run(&a, &mut remote).unwrap();

        // Both now hold both computers' rows (with blank titles for the downloaded ones).
        let expected = vec![
            (
                "mangrove".to_string(),
                "zen".to_string(),
                10 * DAY_SECS + 10,
                10 * DAY_SECS + 110,
            ),
            (
                "laptop".to_string(),
                "kitty".to_string(),
                10 * DAY_SECS + 500,
                10 * DAY_SECS + 700,
            ),
        ];
        assert_eq!(rows(&a), expected);
        assert_eq!(rows(&b), expected);
        let theirs = b
            .intervals_between(0, i64::MAX / 2, Some("mangrove"))
            .unwrap();
        assert_eq!(theirs[0].title, "");

        // More rounds change nothing: no uploads, no new revisions on the server.
        let revs: Vec<String> = {
            let mut v: Vec<String> = remote.docs.values().map(|d| d.1.clone()).collect();
            v.sort();
            v
        };
        for _ in 0..3 {
            for db in [&a, &b] {
                let r = run(db, &mut remote).unwrap();
                assert_eq!((r.uploaded, r.removed, r.downloaded), (0, 0, 0));
            }
        }
        let after: Vec<String> = {
            let mut v: Vec<String> = remote.docs.values().map(|d| d.1.clone()).collect();
            v.sort();
            v
        };
        assert_eq!(revs, after);
    }

    #[test]
    fn a_changed_day_replaces_the_old_copy_everywhere() {
        let a = Db::open_in_memory("mangrove").unwrap();
        let b = Db::open_in_memory("laptop").unwrap();
        let mut remote = MemRemote::default();
        record(&a, 3 * DAY_SECS + 10, 100, "zen");
        run(&a, &mut remote).unwrap();
        run(&b, &mut remote).unwrap();
        assert_eq!(rows(&b).len(), 1);

        // The row grows and a second row appears the same day.
        let id = a.recent(1).unwrap()[0].id;
        a.set_end(id, 3 * DAY_SECS + 400).unwrap();
        record(&a, 3 * DAY_SECS + 1000, 50, "kitty");
        let r = run(&a, &mut remote).unwrap();
        assert_eq!(r.uploaded, 1);
        let r = run(&b, &mut remote).unwrap();
        assert_eq!(r.downloaded, 1);
        assert_eq!(rows(&a), rows(&b));
        assert_eq!(rows(&b).len(), 2);
        assert_eq!(rows(&b)[0].3, 3 * DAY_SECS + 400);
    }

    #[test]
    fn forgetting_data_removes_it_from_the_server_and_the_other_computers() {
        let a = Db::open_in_memory("mangrove").unwrap();
        let b = Db::open_in_memory("laptop").unwrap();
        let mut remote = MemRemote::default();
        a.record_domain(4 * DAY_SECS + 10, Some("github.com"), "x")
            .unwrap();
        a.record_domain(4 * DAY_SECS + 70, None, "x").unwrap();
        run(&a, &mut remote).unwrap();
        run(&b, &mut remote).unwrap();
        assert_eq!(b.domains_between(0, i64::MAX / 2, None).unwrap().len(), 1);

        // Forgetting leaves the day empty, so its document is deleted on the server.
        a.forget_domains(None).unwrap();
        let r = run(&a, &mut remote).unwrap();
        assert_eq!((r.uploaded, r.removed), (0, 1));
        assert!(remote.docs["day:mangrove:1970-01-05"].3);
        let r = run(&b, &mut remote).unwrap();
        assert_eq!(r.downloaded, 1);
        assert!(b.domains_between(0, i64::MAX / 2, None).unwrap().is_empty());
        // Afterwards it is quiet.
        assert_eq!(run(&a, &mut remote).unwrap(), Report::default());
    }

    #[test]
    fn a_failed_upload_is_retried_next_time_with_nothing_lost() {
        let db = Db::open_in_memory("mangrove").unwrap();
        record(&db, 2 * DAY_SECS + 10, 60, "zen");
        let mut remote = MemRemote {
            fail_puts: true,
            ..MemRemote::default()
        };
        assert!(run(&db, &mut remote).is_err());
        assert_eq!(db.dirty_count().unwrap(), 1);
        remote.fail_puts = false;
        let r = run(&db, &mut remote).unwrap();
        assert_eq!(r.uploaded, 1);
        assert_eq!(db.dirty_count().unwrap(), 0);
    }

    #[test]
    fn a_forgotten_revision_is_recovered_after_a_conflict() {
        let db = Db::open_in_memory("mangrove").unwrap();
        let mut remote = MemRemote::default();
        record(&db, 2 * DAY_SECS + 10, 60, "zen");
        run(&db, &mut remote).unwrap();
        // Pretend the database was restored from a backup that does not know the revision.
        db.set_doc_rev("day:mangrove:1970-01-03", None).unwrap();
        record(&db, 2 * DAY_SECS + 500, 60, "kitty");
        let r = run(&db, &mut remote).unwrap();
        assert_eq!(r.uploaded, 1);
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
        let doc = &remote.docs["day:mangrove:1970-01-03"].2;
        assert_eq!(doc["intervals"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn bad_documents_are_skipped_with_a_message_and_the_rest_still_apply() {
        let db = Db::open_in_memory("mangrove").unwrap();
        let mut remote = MemRemote::default();
        remote.seed(json!({"_id": "day:deck:1970-01-02", "device": "deck",
                           "intervals": [[1, 2, "zen"]]}));
        remote.seed(json!({"_id": "day:deck:1970-01-03", "device": "deck",
                           "intervals": [[3 * DAY_SECS, 3 * DAY_SECS + 60, "steam", "1"]]}));
        remote.seed(json!({"_id": "_design/views"}));
        let r = run(&db, &mut remote).unwrap();
        assert_eq!(r.downloaded, 1);
        assert_eq!(r.warnings.len(), 1, "{:?}", r.warnings);
        assert!(r.warnings[0].contains("day:deck:1970-01-02"));
        assert_eq!(rows(&db).len(), 1);
    }

    #[test]
    fn many_changes_are_downloaded_in_batches() {
        let db = Db::open_in_memory("mangrove").unwrap();
        let mut remote = MemRemote::default();
        for day in 0..250i64 {
            let rows = DayRows {
                intervals: vec![(
                    day * DAY_SECS,
                    day * DAY_SECS + 60,
                    "steam".into(),
                    "1".into(),
                )],
                ..DayRows::default()
            };
            remote.seed(syncdoc::build_doc("deck", day, &rows, None));
        }
        let r = run(&db, &mut remote).unwrap();
        assert_eq!(r.downloaded, 250);
        assert_eq!(rows(&db).len(), 250);
        // The position is remembered, so the next run asks for nothing new.
        assert_eq!(run(&db, &mut remote).unwrap().downloaded, 0);
    }

    #[test]
    fn a_reinstalled_computer_learns_its_old_revisions_from_the_feed() {
        let db = Db::open_in_memory("mangrove").unwrap();
        let mut remote = MemRemote::default();
        // The server already holds a document of ours that this fresh database never saw.
        let old = DayRows {
            intervals: vec![(DAY_SECS, DAY_SECS + 60, "zen".into(), "1".into())],
            ..DayRows::default()
        };
        remote.seed(syncdoc::build_doc("mangrove", 1, &old, None));
        run(&db, &mut remote).unwrap();
        assert!(db.doc_rev("day:mangrove:1970-01-02").unwrap().is_some());
        // Our own old rows are not copied back, and a new row updates the document cleanly.
        assert!(rows(&db).is_empty());
        record(&db, DAY_SECS + 500, 30, "kitty");
        let r = run(&db, &mut remote).unwrap();
        assert_eq!((r.uploaded, r.warnings.len()), (1, 0));
    }

    #[test]
    fn settings_are_parsed_and_checked() {
        let ok = SyncConfig::parse(
            "# my server\nurl = http://host.example:5984/\ndatabase = timesink\nuser = shane\npassword = pa#ss = word\n",
        )
        .unwrap();
        assert_eq!(ok.url, "http://host.example:5984");
        assert_eq!(ok.database, "timesink");
        assert_eq!(ok.password, "pa#ss = word");
        let err = |text: &str| SyncConfig::parse(text).unwrap_err().to_string();
        assert!(err("url = http://h\ndatabase = d\nuser = u\n").contains("password"));
        assert!(err("url = ftp://h\ndatabase = d\nuser = u\npassword = p\n").contains("http"));
        assert!(
            err("url = http://h\ndatabase = Bad/Name\nuser = u\npassword = p\n")
                .contains("database")
        );
        assert!(
            err("url = http://h\ndatabase = d\nuser = u\npassword = p\nextra = 1\n")
                .contains("unknown setting")
        );
        assert!(err("just words").contains("key = value"));
    }

    #[cfg(unix)]
    #[test]
    fn a_world_readable_password_file_is_flagged() {
        use std::os::unix::fs::PermissionsExt;
        let path = std::env::temp_dir().join(format!("timesink-perm-{}", std::process::id()));
        std::fs::write(&path, "x").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(permission_warning(&path).unwrap().contains("chmod 600"));
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(permission_warning(&path).is_none());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn ids_are_percent_encoded_for_urls() {
        assert_eq!(
            pct_encode("day:mangrove:2026-11-10"),
            "day%3Amangrove%3A2026-11-10"
        );
        assert_eq!(pct_encode("12-abc_DEF.~"), "12-abc_DEF.~");
        assert_eq!(pct_encode("a b/c"), "a%20b%2Fc");
    }

    /// Serve canned answers on a local port, one connection per request, and return the
    /// address plus a handle that yields the requests received (request line and headers).
    fn fake_server(
        answers: Vec<(&'static str, u16, String)>,
    ) -> (String, std::thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for (expect, status, body) in answers {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buf = vec![0u8; 65536];
                let mut got = 0;
                // Read until the headers (and any body) have arrived.
                loop {
                    let n = stream.read(&mut buf[got..]).unwrap();
                    got += n;
                    let text = String::from_utf8_lossy(&buf[..got]).to_string();
                    if let Some(end) = text.find("\r\n\r\n") {
                        let len = text
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        if got >= end + 4 + len || n == 0 {
                            break;
                        }
                    } else if n == 0 {
                        break;
                    }
                }
                let request = String::from_utf8_lossy(&buf[..got]).to_string();
                assert!(
                    request.starts_with(expect),
                    "wanted {expect}, got {request}"
                );
                let reply = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(reply.as_bytes()).unwrap();
                seen.push(request);
            }
            seen
        });
        (url, handle)
    }

    #[test]
    fn the_http_client_speaks_couchdb() {
        let answers = vec![
            ("GET /timesink HTTP/1.1", 200, "{}".to_string()),
            (
                "POST /timesink/_bulk_docs HTTP/1.1",
                201,
                r#"[{"ok":true,"id":"a","rev":"1-x"},{"id":"b","error":"conflict","reason":"Document update conflict."},{"id":"c","error":"forbidden","reason":"no"}]"#.to_string(),
            ),
            (
                "GET /timesink/day%3Ax%3A1970-01-01 HTTP/1.1",
                200,
                r#"{"_id":"day:x:1970-01-01","_rev":"4-z"}"#.to_string(),
            ),
            (
                "GET /timesink/_changes?since=7-abc&limit=50&include_docs=true HTTP/1.1",
                200,
                r#"{"results":[{"seq":"8-d","id":"day:x:1970-01-01","changes":[{"rev":"4-z"}],"doc":{"_id":"day:x:1970-01-01"}},{"seq":"9-e","id":"day:x:1970-01-02","changes":[{"rev":"2-y"}],"deleted":true}],"last_seq":"9-e","pending":0}"#.to_string(),
            ),
            ("GET /timesink/nope HTTP/1.1", 404, r#"{"error":"not_found"}"#.to_string()),
            ("GET /timesink HTTP/1.1", 401, r#"{"error":"unauthorized"}"#.to_string()),
        ];
        let (url, handle) = fake_server(answers);
        let mut remote = HttpRemote::new(SyncConfig {
            url,
            database: "timesink".into(),
            user: "shane".into(),
            password: "pw".into(),
        });
        remote.ensure_db().unwrap();
        let results = remote
            .put_docs(&[
                json!({"_id": "a"}),
                json!({"_id": "b"}),
                json!({"_id": "c"}),
            ])
            .unwrap();
        assert_eq!(results[0], PutResult::Ok("1-x".into()));
        assert_eq!(results[1], PutResult::Conflict);
        assert!(matches!(&results[2], PutResult::Failed(why) if why.contains("forbidden")));
        assert_eq!(
            remote.current_rev("day:x:1970-01-01").unwrap().as_deref(),
            Some("4-z")
        );
        let (changes, last) = remote.changes("7-abc", 50).unwrap();
        assert_eq!(last, "9-e");
        assert_eq!(changes.len(), 2);
        assert!(!changes[0].deleted && changes[0].doc.is_some());
        assert!(changes[1].deleted && changes[1].doc.is_none());
        assert_eq!(remote.current_rev("nope").unwrap(), None);
        // A refused login explains what to check.
        let err = remote.ensure_db().unwrap_err().to_string();
        assert!(err.contains("401") && err.contains("sync.conf"), "{err}");

        let seen = handle.join().unwrap();
        // Every request carried the login (user "shane", password "pw").
        for request in &seen {
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("authorization: basic ")
                    && request.contains("c2hhbmU6cHc="),
                "{request}"
            );
        }
        // The upload sent the documents as JSON.
        assert!(seen[1].contains(r#""docs""#));
    }
}
