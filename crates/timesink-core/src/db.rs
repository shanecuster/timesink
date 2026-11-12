//! SQLite storage for focus intervals.
//!
//! One row per interval. Rows are written as soon as an interval opens and their
//! end time is moved forward on every heartbeat, so a crash or power loss only
//! loses a few seconds, never a whole session.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::{params, Connection};

use crate::listen::ListenEffect;
use crate::model::{StoredInterval, WindowSnapshot};
use crate::tracker::Effect;

/// Version of the database layout. Bump it and add a migration step when the
/// layout changes. Stored in SQLite's built-in `user_version` field.
pub const SCHEMA_VERSION: i32 = 5;

/// Intervals shorter than this many seconds are deleted when they close.
/// This drops the flicker from quickly passing over windows.
pub const MIN_INTERVAL_SECS: i64 = 1;

/// A new interval that starts within this many seconds of the previous one
/// ending, with the exact same window, is joined to it instead of being a new row.
pub const MERGE_GAP_SECS: i64 = 2;

/// A listening row that starts within this many seconds of the same player's
/// previous row ending is joined to it. Players pause briefly between tracks,
/// and the player list is only checked every few seconds.
pub const LISTEN_MERGE_GAP_SECS: i64 = 10;

/// A handle to the timesink database.
pub struct Db {
    conn: Connection,
    /// Name of this computer, stored with every row.
    device_id: String,
}

impl Db {
    /// Open (or create) the database file at `path`.
    ///
    /// If an older database layout is found, the file is copied to a backup
    /// BEFORE it is migrated.
    pub fn open(path: &Path, device_id: &str) -> Result<Self> {
        // Make sure the folder exists (first run).
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating data directory {}", parent.display()))?;
        }

        // If the file already exists with an older layout, back it up first.
        if path.exists() {
            let current = read_user_version(&Connection::open(path)?)?;
            if current > 0 && current < SCHEMA_VERSION {
                backup_database(path, current)?;
            }
        }

        let conn = Connection::open(path)
            .with_context(|| format!("opening database {}", path.display()))?;
        Self::init(conn, device_id)
    }

    /// Open a throwaway in-memory database (used by tests).
    pub fn open_in_memory(device_id: &str) -> Result<Self> {
        Self::init(Connection::open_in_memory()?, device_id)
    }

    /// Apply settings and create or migrate the tables.
    fn init(conn: Connection, device_id: &str) -> Result<Self> {
        // WAL mode lets the TUI read while the daemon writes.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        // The daemon and the browser extension's helper both write; wait briefly for each
        // other instead of failing with "database is locked".
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let version = read_user_version(&conn)?;
        if version < 1 {
            // Fresh database: create version 1 of the layout.
            conn.execute_batch(
                "CREATE TABLE intervals (
                     id        INTEGER PRIMARY KEY,
                     device_id TEXT    NOT NULL,
                     start_ts  INTEGER NOT NULL,
                     end_ts    INTEGER NOT NULL,
                     app_class TEXT    NOT NULL,
                     title     TEXT    NOT NULL,
                     workspace TEXT    NOT NULL
                 );
                 CREATE INDEX idx_intervals_start ON intervals(start_ts);",
            )?;
        }
        if version < 2 {
            // Version 2 adds listening time (which media player was playing).
            // Only the player's short id is stored, never track information.
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS listening (
                     id        INTEGER PRIMARY KEY,
                     device_id TEXT    NOT NULL,
                     start_ts  INTEGER NOT NULL,
                     end_ts    INTEGER NOT NULL,
                     player    TEXT    NOT NULL
                 );
                 CREATE INDEX IF NOT EXISTS idx_listening_start ON listening(start_ts);",
            )?;
        }
        if version < 3 {
            // Version 3 adds website time from the optional browser extension. Only the
            // registered site (e.g. "github.com") is stored: no path, query or title.
            // `closed` = 1 once the extension said no tracked site is in front, so a later
            // report of the same site starts a fresh stretch instead of bridging the gap.
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS domains (
                     id        INTEGER PRIMARY KEY,
                     device_id TEXT    NOT NULL,
                     start_ts  INTEGER NOT NULL,
                     end_ts    INTEGER NOT NULL,
                     domain    TEXT    NOT NULL,
                     closed    INTEGER NOT NULL DEFAULT 0
                 );
                 CREATE INDEX IF NOT EXISTS idx_domains_start ON domains(start_ts);",
            )?;
        }
        if version < 4 {
            // Version 4 records which helper process (one per running browser) wrote a
            // stretch, so two browsers with the extension cannot close each other's stretches.
            // Existing rows get '' (unknown source).
            conn.execute_batch("ALTER TABLE domains ADD COLUMN source TEXT NOT NULL DEFAULT '';")?;
        }
        if version < 5 {
            // Version 5 prepares syncing with CouchDB (see `syncdoc`). `sync_dirty` lists the
            // UTC days whose rows from THIS computer changed since they were last uploaded;
            // triggers keep it up to date whatever code writes the rows. `sync_docs` remembers
            // the CouchDB revision of each uploaded day, and `sync_state` holds small values
            // (this computer's name for the triggers, the last change number downloaded).
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS sync_dirty (day INTEGER PRIMARY KEY);
                 CREATE TABLE IF NOT EXISTS sync_docs (doc_id TEXT PRIMARY KEY, rev TEXT NOT NULL);
                 CREATE TABLE IF NOT EXISTS sync_state (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
            )?;
            for table in ["intervals", "listening", "domains"] {
                // Only rows this computer recorded are marked; other computers' rows (once
                // downloaded) are never uploaded. The marks are made by triggers.
                conn.execute_batch(&format!(
                    "CREATE TRIGGER IF NOT EXISTS {table}_sync_ins AFTER INSERT ON {table}
                     WHEN NEW.device_id = (SELECT value FROM sync_state WHERE key = 'device')
                     BEGIN INSERT OR IGNORE INTO sync_dirty (day) VALUES (NEW.start_ts / 86400); END;
                     CREATE TRIGGER IF NOT EXISTS {table}_sync_upd AFTER UPDATE ON {table}
                     WHEN NEW.device_id = (SELECT value FROM sync_state WHERE key = 'device')
                     BEGIN
                         INSERT OR IGNORE INTO sync_dirty (day) VALUES (NEW.start_ts / 86400);
                         INSERT OR IGNORE INTO sync_dirty (day) VALUES (OLD.start_ts / 86400);
                     END;
                     CREATE TRIGGER IF NOT EXISTS {table}_sync_del AFTER DELETE ON {table}
                     WHEN OLD.device_id = (SELECT value FROM sync_state WHERE key = 'device')
                     BEGIN INSERT OR IGNORE INTO sync_dirty (day) VALUES (OLD.start_ts / 86400); END;"
                ))?;
            }
            // Everything already recorded here has never been uploaded.
            conn.execute(
                "INSERT OR REPLACE INTO sync_state (key, value) VALUES ('device', ?1)",
                [device_id],
            )?;
            for table in ["intervals", "listening", "domains"] {
                conn.execute(
                    &format!(
                        "INSERT OR IGNORE INTO sync_dirty (day)
                         SELECT DISTINCT start_ts / 86400 FROM {table} WHERE device_id = ?1"
                    ),
                    [device_id],
                )?;
            }
        }
        // The triggers compare against this computer's name, so keep it current (written
        // only when it changed, so opening the database stays a read most of the time).
        conn.execute(
            "INSERT INTO sync_state (key, value) VALUES ('device', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value WHERE value != excluded.value",
            [device_id],
        )?;
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        Ok(Self {
            conn,
            device_id: device_id.to_string(),
        })
    }

    /// Insert a new interval that has just started. Returns its row id.
    pub fn insert_open(&self, start_ts: i64, snapshot: &WindowSnapshot) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO intervals (device_id, start_ts, end_ts, app_class, title, workspace)
             VALUES (?1, ?2, ?2, ?3, ?4, ?5)",
            params![
                self.device_id,
                start_ts,
                snapshot.app_class,
                snapshot.title,
                snapshot.workspace
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Move an open interval's end time forward (never backwards).
    pub fn set_end(&self, id: i64, end_ts: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE intervals SET end_ts = MAX(end_ts, ?2) WHERE id = ?1",
            params![id, end_ts],
        )?;
        Ok(())
    }

    /// Finish an interval at exactly `end_ts` (never before its start).
    ///
    /// Unlike `set_end` this CAN move the end backwards. That is needed when
    /// idle is noticed late: heartbeats have already pushed the end forward, and
    /// it must be pulled back to the time of the last input.
    /// Very short intervals are deleted instead of kept.
    pub fn finish(&self, id: i64, end_ts: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE intervals SET end_ts = MAX(start_ts, ?2) WHERE id = ?1",
            params![id, end_ts],
        )?;
        self.conn.execute(
            "DELETE FROM intervals WHERE id = ?1 AND end_ts - start_ts < ?2",
            params![id, MIN_INTERVAL_SECS],
        )?;
        Ok(())
    }

    /// Find the row a new interval should be joined to, if any.
    ///
    /// Only the most recently ended row of this device qualifies, and only if it
    /// is the same window and ended no more than `max_gap` seconds before
    /// `start_ts`. Looking at the latest row only guarantees we never join across
    /// a different activity that was recorded in between.
    pub fn find_mergeable(
        &self,
        start_ts: i64,
        snapshot: &WindowSnapshot,
        max_gap: i64,
    ) -> Result<Option<i64>> {
        let latest = self.conn.query_row(
            "SELECT id, end_ts, app_class, title, workspace FROM intervals
             WHERE device_id = ?1 ORDER BY end_ts DESC, id DESC LIMIT 1",
            [&self.device_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            },
        );
        match latest {
            Ok((id, end_ts, class, title, workspace)) => {
                let same = class == snapshot.app_class
                    && title == snapshot.title
                    && workspace == snapshot.workspace;
                Ok((same && start_ts - end_ts <= max_gap).then_some(id))
            }
            // An empty table simply has nothing to merge with.
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// The most recent intervals, newest first.
    pub fn recent(&self, limit: usize) -> Result<Vec<StoredInterval>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, device_id, start_ts, end_ts, app_class, title, workspace
             FROM intervals ORDER BY start_ts DESC, id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit as i64], |row| {
            Ok(StoredInterval {
                id: row.get(0)?,
                device_id: row.get(1)?,
                start_ts: row.get(2)?,
                end_ts: row.get(3)?,
                app_class: row.get(4)?,
                title: row.get(5)?,
                workspace: row.get(6)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("reading recent intervals")
    }

    /// Total seconds per `(app_class)` since `since_ts`, biggest first.
    ///
    /// Intervals that started before `since_ts` only count their part after it.
    pub fn totals_since(&self, since_ts: i64) -> Result<Vec<(String, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT app_class, SUM(end_ts - MAX(start_ts, ?1)) AS secs
             FROM intervals WHERE end_ts > ?1
             GROUP BY app_class ORDER BY secs DESC",
        )?;
        let rows = stmt.query_map([since_ts], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("reading totals")
    }
}

impl Db {
    /// Insert a new listening row that has just started. Returns its id.
    pub fn insert_listen_open(&self, start_ts: i64, player: &str) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO listening (device_id, start_ts, end_ts, player)
             VALUES (?1, ?2, ?2, ?3)",
            params![self.device_id, start_ts, player],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Move an open listening row's end forward (never backwards).
    pub fn set_listen_end(&self, id: i64, end_ts: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE listening SET end_ts = MAX(end_ts, ?2) WHERE id = ?1",
            params![id, end_ts],
        )?;
        Ok(())
    }

    /// Finish a listening row at exactly `end_ts`; very short rows are deleted.
    pub fn finish_listen(&self, id: i64, end_ts: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE listening SET end_ts = MAX(start_ts, ?2) WHERE id = ?1",
            params![id, end_ts],
        )?;
        self.conn.execute(
            "DELETE FROM listening WHERE id = ?1 AND end_ts - start_ts < ?2",
            params![id, MIN_INTERVAL_SECS],
        )?;
        Ok(())
    }

    /// Find the latest listening row of this device if it is the same player
    /// and ended no more than `max_gap` seconds before `start_ts`.
    pub fn find_listen_mergeable(
        &self,
        start_ts: i64,
        player: &str,
        max_gap: i64,
    ) -> Result<Option<i64>> {
        let latest = self.conn.query_row(
            "SELECT id, end_ts, player FROM listening
             WHERE device_id = ?1 ORDER BY end_ts DESC, id DESC LIMIT 1",
            [&self.device_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        );
        match latest {
            Ok((id, end_ts, p)) if p == player && start_ts - end_ts <= max_gap => Ok(Some(id)),
            Ok(_) | Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e).context("looking for a listening row to join"),
        }
    }

    /// Seconds of listening per player since `since_ts`, most first.
    /// Time before `since_ts` is not counted.
    pub fn listening_since(&self, since_ts: i64) -> Result<Vec<(String, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT player, SUM(end_ts - MAX(start_ts, ?1)) AS secs
             FROM listening WHERE end_ts > ?1
             GROUP BY player ORDER BY secs DESC",
        )?;
        let rows = stmt.query_map([since_ts], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("reading listening totals")
    }
}

impl Db {
    /// All focus rows that overlap `[from_ts, to_ts)`, oldest first.
    /// Rows are returned whole; callers clip them to the range.
    pub fn intervals_between(
        &self,
        from_ts: i64,
        to_ts: i64,
        device: Option<&str>,
    ) -> Result<Vec<StoredInterval>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, device_id, start_ts, end_ts, app_class, title, workspace
             FROM intervals WHERE end_ts > ?1 AND start_ts < ?2
               AND (?3 IS NULL OR device_id = ?3)
             ORDER BY start_ts, id",
        )?;
        let rows = stmt.query_map(params![from_ts, to_ts, device], |row| {
            Ok(StoredInterval {
                id: row.get(0)?,
                device_id: row.get(1)?,
                start_ts: row.get(2)?,
                end_ts: row.get(3)?,
                app_class: row.get(4)?,
                title: row.get(5)?,
                workspace: row.get(6)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("reading intervals")
    }

    /// All listening rows that overlap `[from_ts, to_ts)` as
    /// (player, start, end), oldest first.
    pub fn listening_between(
        &self,
        from_ts: i64,
        to_ts: i64,
        device: Option<&str>,
    ) -> Result<Vec<(String, i64, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT player, start_ts, end_ts FROM listening
             WHERE end_ts > ?1 AND start_ts < ?2 AND (?3 IS NULL OR device_id = ?3)
             ORDER BY start_ts, id",
        )?;
        let rows = stmt.query_map(params![from_ts, to_ts, device], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("reading listening rows")
    }
}

/// Methods used by syncing with CouchDB (see `syncdoc` for the document format).
impl Db {
    /// This computer's name, as stored on the rows it records.
    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    /// Every computer that has rows in this database, sorted by name.
    pub fn devices(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT device_id FROM intervals UNION SELECT device_id FROM listening
             UNION SELECT device_id FROM domains ORDER BY 1",
        )?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        rows.collect::<rusqlite::Result<Vec<String>>>()
            .context("listing devices")
    }

    /// Remove and return the UTC days whose rows from this computer changed since they
    /// were last uploaded. Clearing the marks BEFORE reading the rows means a change made
    /// while uploading marks its day again instead of being lost; if the upload fails the
    /// caller puts the marks back with `mark_dirty`.
    pub fn take_dirty_days(&self) -> Result<Vec<i64>> {
        let tx = self.conn.unchecked_transaction()?;
        let days: Vec<i64> = {
            let mut stmt = tx.prepare("SELECT day FROM sync_dirty ORDER BY day")?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            rows.collect::<rusqlite::Result<Vec<i64>>>()?
        };
        tx.execute("DELETE FROM sync_dirty", [])?;
        tx.commit()?;
        Ok(days)
    }

    /// Mark a day as needing upload.
    pub fn mark_dirty(&self, day: i64) -> Result<()> {
        self.conn
            .execute("INSERT OR IGNORE INTO sync_dirty (day) VALUES (?1)", [day])?;
        Ok(())
    }

    /// How many days are waiting to be uploaded.
    pub fn dirty_count(&self) -> Result<usize> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM sync_dirty", [], |r| r.get(0))?;
        Ok(n as usize)
    }

    /// This computer's rows that started on the given UTC day.
    pub fn day_rows(&self, day: i64) -> Result<crate::syncdoc::DayRows> {
        use crate::syncdoc::{DayRows, DAY_SECS};
        let (from, to) = (day * DAY_SECS, (day + 1) * DAY_SECS);
        let mut rows = DayRows::default();
        let mut stmt = self.conn.prepare(
            "SELECT start_ts, end_ts, app_class, workspace FROM intervals
             WHERE device_id = ?1 AND start_ts >= ?2 AND start_ts < ?3 ORDER BY start_ts, id",
        )?;
        rows.intervals = stmt
            .query_map(params![self.device_id, from, to], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        let mut stmt = self.conn.prepare(
            "SELECT start_ts, end_ts, player FROM listening
             WHERE device_id = ?1 AND start_ts >= ?2 AND start_ts < ?3 ORDER BY start_ts, id",
        )?;
        rows.listening = stmt
            .query_map(params![self.device_id, from, to], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        let mut stmt = self.conn.prepare(
            "SELECT start_ts, end_ts, domain FROM domains
             WHERE device_id = ?1 AND start_ts >= ?2 AND start_ts < ?3 ORDER BY start_ts, id",
        )?;
        rows.domains = stmt
            .query_map(params![self.device_id, from, to], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// Replace another computer's rows for one UTC day with the downloaded ones, in one
    /// transaction. Titles are blank (they are never synced). The rows are closed, so
    /// nothing here is ever extended or merged by the local tracker. Refuses to touch this
    /// computer's own rows. Passing an empty `DayRows` removes the day.
    pub fn replace_day(
        &self,
        device: &str,
        day: i64,
        rows: &crate::syncdoc::DayRows,
    ) -> Result<()> {
        use crate::syncdoc::DAY_SECS;
        if device == self.device_id {
            anyhow::bail!("refusing to overwrite this computer's own rows with a download");
        }
        let (from, to) = (day * DAY_SECS, (day + 1) * DAY_SECS);
        let tx = self.conn.unchecked_transaction()?;
        for table in ["intervals", "listening", "domains"] {
            tx.execute(
                &format!(
                    "DELETE FROM {table} WHERE device_id = ?1 AND start_ts >= ?2 AND start_ts < ?3"
                ),
                params![device, from, to],
            )?;
        }
        for (start, end, app, workspace) in &rows.intervals {
            tx.execute(
                "INSERT INTO intervals (device_id, start_ts, end_ts, app_class, title, workspace)
                 VALUES (?1, ?2, ?3, ?4, '', ?5)",
                params![device, start, end, app, workspace],
            )?;
        }
        for (start, end, player) in &rows.listening {
            tx.execute(
                "INSERT INTO listening (device_id, start_ts, end_ts, player)
                 VALUES (?1, ?2, ?3, ?4)",
                params![device, start, end, player],
            )?;
        }
        for (start, end, site) in &rows.domains {
            tx.execute(
                "INSERT INTO domains (device_id, start_ts, end_ts, domain, closed, source)
                 VALUES (?1, ?2, ?3, ?4, 1, 'sync')",
                params![device, start, end, site],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// The CouchDB revision last seen for a document, if any.
    pub fn doc_rev(&self, doc_id: &str) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT rev FROM sync_docs WHERE doc_id = ?1")?;
        let mut rows = stmt.query([doc_id])?;
        match rows.next()? {
            Some(r) => Ok(Some(r.get(0)?)),
            None => Ok(None),
        }
    }

    /// Remember a document's latest revision; `None` forgets it.
    pub fn set_doc_rev(&self, doc_id: &str, rev: Option<&str>) -> Result<()> {
        match rev {
            Some(rev) => self.conn.execute(
                "INSERT OR REPLACE INTO sync_docs (doc_id, rev) VALUES (?1, ?2)",
                params![doc_id, rev],
            )?,
            None => self
                .conn
                .execute("DELETE FROM sync_docs WHERE doc_id = ?1", [doc_id])?,
        };
        Ok(())
    }

    /// A small stored value (for example the last CouchDB change number downloaded).
    pub fn sync_state(&self, key: &str) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT value FROM sync_state WHERE key = ?1")?;
        let mut rows = stmt.query([key])?;
        match rows.next()? {
            Some(r) => Ok(Some(r.get(0)?)),
            None => Ok(None),
        }
    }

    /// Store a small value for syncing.
    pub fn set_sync_state(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO sync_state (key, value) VALUES (?1, ?2)",
            params![key, value],
        )?;
        Ok(())
    }
}

impl Db {
    /// Remember another computer's shared lists (names and categories), replacing what
    /// was kept for it. `None` forgets them. Kept in `sync_state` as JSON, so no new table
    /// (and no database migration) is needed.
    pub fn set_remote_conf(
        &self,
        device: &str,
        lists: Option<&crate::syncdoc::ConfLists>,
    ) -> Result<()> {
        let key = format!("conf:{device}");
        match lists {
            Some(l) => self.set_sync_state(&key, &conf_to_json(l)),
            None => {
                self.conn
                    .execute("DELETE FROM sync_state WHERE key = ?1", [&key])?;
                Ok(())
            }
        }
    }

    /// The shared lists of every other computer that has uploaded some, as
    /// (device, lists), in device-name order.
    pub fn remote_confs(&self) -> Result<Vec<(String, crate::syncdoc::ConfLists)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT key, value FROM sync_state WHERE key LIKE 'conf:%' ORDER BY key")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let mut out = Vec::new();
        for row in rows {
            let (key, value) = row?;
            let device = key.trim_start_matches("conf:").to_string();
            // A value that cannot be read is skipped, never fatal.
            if let Some(lists) = conf_from_json(&value) {
                out.push((device, lists));
            }
        }
        Ok(out)
    }

    /// The lists this computer last uploaded successfully, to tell whether they changed.
    pub fn own_conf_uploaded(&self) -> Result<Option<crate::syncdoc::ConfLists>> {
        Ok(self
            .sync_state("conf_own")?
            .and_then(|v| conf_from_json(&v)))
    }

    /// Remember the lists that were just uploaded.
    pub fn set_own_conf_uploaded(&self, lists: &crate::syncdoc::ConfLists) -> Result<()> {
        self.set_sync_state("conf_own", &conf_to_json(lists))
    }
}

/// Lists as the JSON text kept in `sync_state`.
fn conf_to_json(l: &crate::syncdoc::ConfLists) -> String {
    serde_json::json!({
        "apps": l.apps, "apps_updated": l.apps_updated,
        "domains": l.domains, "domains_updated": l.domains_updated,
    })
    .to_string()
}

/// The reverse of [`conf_to_json`]; `None` if the text is not what it should be.
fn conf_from_json(text: &str) -> Option<crate::syncdoc::ConfLists> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    Some(crate::syncdoc::ConfLists {
        apps: v["apps"].as_str()?.to_string(),
        apps_updated: v["apps_updated"].as_i64().unwrap_or(0),
        domains: v["domains"].as_str()?.to_string(),
        domains_updated: v["domains_updated"].as_i64().unwrap_or(0),
    })
}

impl Db {
    /// Record what the browser extension just reported: `Some(site)` = this site is the
    /// active tab of the focused window, `None` = nothing trackable is in front (another
    /// window, a private window, a blocked or internal page).
    ///
    /// The report either extends the open stretch (same site, reported within
    /// [`crate::domains::DOMAIN_GAP_SECS`]), starts a new one, or closes the open one.
    pub fn record_domain(&self, now: i64, site: Option<&str>, source: &str) -> Result<()> {
        use crate::domains::DOMAIN_GAP_SECS;
        // The newest stretch from this source (one browser's helper) on this computer.
        let last: Option<(i64, i64, String, bool)> = self
            .conn
            .query_row(
                "SELECT id, end_ts, domain, closed FROM domains
                 WHERE device_id = ?1 AND source = ?2 ORDER BY id DESC LIMIT 1",
                params![self.device_id, source],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get::<_, i64>(3)? != 0)),
            )
            .ok();
        // An open stretch whose last report is recent enough to continue.
        let live = last
            .as_ref()
            .filter(|(_, end, _, closed)| !closed && now - end <= DOMAIN_GAP_SECS);
        match (site, live) {
            // Same site again: move the end forward.
            (Some(d), Some((id, _, last_domain, _))) if last_domain == d => {
                self.conn.execute(
                    "UPDATE domains SET end_ts = ?1 WHERE id = ?2",
                    params![now, id],
                )?;
            }
            // A different site (or the first): close any open stretch at `now`, start anew.
            (Some(d), live) => {
                if let Some((id, _, _, _)) = live {
                    self.conn.execute(
                        "UPDATE domains SET end_ts = ?1, closed = 1 WHERE id = ?2",
                        params![now, id],
                    )?;
                }
                self.conn.execute(
                    "INSERT INTO domains (device_id, start_ts, end_ts, domain, closed, source)
                     VALUES (?1, ?2, ?2, ?3, 0, ?4)",
                    params![self.device_id, now, d, source],
                )?;
            }
            // Nothing trackable in front: end the open stretch now.
            (None, Some((id, _, _, _))) => {
                self.conn.execute(
                    "UPDATE domains SET end_ts = ?1, closed = 1 WHERE id = ?2",
                    params![now, id],
                )?;
            }
            // Nothing in front and nothing open (a stale one is simply left as it was).
            (None, None) => {}
        }
        Ok(())
    }

    /// Store a complete site stretch in one go (used only to fill the sandbox with demo data;
    /// real reports go through [`Db::record_domain`]).
    pub fn insert_domain_stretch(&self, site: &str, start_ts: i64, end_ts: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO domains (device_id, start_ts, end_ts, domain, closed)
             VALUES (?1, ?2, ?3, ?4, 1)",
            params![self.device_id, start_ts, end_ts, site],
        )?;
        Ok(())
    }

    /// Start time of the earliest recorded focus interval, if anything has been recorded.
    /// Used by the "All time" view to know how far back to look.
    pub fn earliest_start(&self) -> Result<Option<i64>> {
        let t: Option<i64> =
            self.conn
                .query_row("SELECT MIN(start_ts) FROM intervals", [], |r| r.get(0))?;
        Ok(t)
    }

    /// All site stretches that overlap `[from_ts, to_ts)` as (site, start, end), oldest first.
    pub fn domains_between(
        &self,
        from_ts: i64,
        to_ts: i64,
        device: Option<&str>,
    ) -> Result<Vec<(String, i64, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT domain, start_ts, end_ts FROM domains
             WHERE end_ts > ?1 AND start_ts < ?2 AND (?3 IS NULL OR device_id = ?3)
             ORDER BY start_ts, id",
        )?;
        let rows = stmt.query_map(params![from_ts, to_ts, device], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("reading site rows")
    }

    /// The newest site report stored for this computer as (site, last report time), if any.
    /// Used by the helper's `status` command to show whether the extension is still reporting.
    pub fn latest_domain_report(&self) -> Result<Option<(String, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT domain, end_ts FROM domains WHERE device_id = ?1 ORDER BY end_ts DESC, id DESC LIMIT 1",
        )?;
        let mut rows = stmt.query([&self.device_id])?;
        match rows.next()? {
            Some(r) => Ok(Some((r.get(0)?, r.get(1)?))),
            None => Ok(None),
        }
    }

    /// Delete stored site time: all of it, or just one site. Returns how many rows went.
    pub fn forget_domains(&self, site: Option<&str>) -> Result<usize> {
        let n = match site {
            Some(d) => self
                .conn
                .execute("DELETE FROM domains WHERE domain = ?1", [d])?,
            None => self.conn.execute("DELETE FROM domains", [])?,
        };
        Ok(n)
    }
}

impl Db {
    /// Write a consistent copy of the whole database to `path`.
    ///
    /// Uses SQLite's `VACUUM INTO`, which is safe even while the daemon is
    /// writing (a plain file copy could miss data still in the write-ahead log).
    /// Fails if `path` already exists.
    pub fn backup_to(&self, path: &Path) -> Result<()> {
        self.conn
            .execute("VACUUM INTO ?1", [path.to_string_lossy().as_ref()])
            .with_context(|| format!("backing up database to {}", path.display()))?;
        Ok(())
    }

    /// Rewrite stored titles. `rewrite(app_class, title)` returns the new title.
    ///
    /// Returns how many rows changed. Runs in one transaction, so it either
    /// applies completely or not at all.
    pub fn rewrite_titles(&self, rewrite: impl Fn(&str, &str) -> String) -> Result<usize> {
        // Each distinct (app, title) pair only needs to be rewritten once.
        let mut stmt = self
            .conn
            .prepare("SELECT DISTINCT app_class, title FROM intervals")?;
        let pairs = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let tx = self.conn.unchecked_transaction()?;
        let mut changed = 0;
        for (app_class, title) in pairs {
            let new_title = rewrite(&app_class, &title);
            if new_title != title {
                changed += tx.execute(
                    "UPDATE intervals SET title = ?3 WHERE app_class = ?1 AND title = ?2",
                    params![app_class, title, new_title],
                )?;
            }
        }
        tx.commit()?;
        Ok(changed)
    }

    /// Join neighbouring rows that are now the same window and at most
    /// `max_gap` seconds apart (for example after titles were blanked).
    ///
    /// Returns how many rows were removed by merging. The newest row of each
    /// device is never touched, because the running daemon may still be updating it.
    pub fn merge_adjacent(&self, max_gap: i64) -> Result<usize> {
        /// One row, with just the fields merging needs.
        struct Row {
            id: i64,
            device: String,
            start: i64,
            end: i64,
            key: (String, String, String), // (app_class, title, workspace)
        }

        let mut stmt = self.conn.prepare(
            "SELECT id, device_id, start_ts, end_ts, app_class, title, workspace
             FROM intervals ORDER BY device_id, start_ts, id",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(Row {
                    id: r.get(0)?,
                    device: r.get(1)?,
                    start: r.get(2)?,
                    end: r.get(3)?,
                    key: (r.get(4)?, r.get(5)?, r.get(6)?),
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        // The newest row per device (by end time) is protected.
        let mut protected: std::collections::HashMap<&str, (i64, i64)> =
            std::collections::HashMap::new();
        for row in &rows {
            let newest = protected
                .entry(row.device.as_str())
                .or_insert((row.end, row.id));
            if (row.end, row.id) > *newest {
                *newest = (row.end, row.id);
            }
        }

        // Walk the rows in order, folding each into the current "head" row when allowed.
        let mut deletions: Vec<i64> = Vec::new();
        let mut extensions: Vec<(i64, i64)> = Vec::new(); // (head id, new end)
        let mut head: Option<&Row> = None;
        let mut head_end = 0;
        for row in &rows {
            let is_protected = protected.get(row.device.as_str()).map(|p| p.1) == Some(row.id);
            let joins_head = !is_protected
                && head.is_some_and(|h| {
                    h.device == row.device && h.key == row.key && row.start - head_end <= max_gap
                });
            if joins_head {
                head_end = head_end.max(row.end);
                deletions.push(row.id);
                if let Some(h) = head {
                    // Remember the latest end for this head (later entries win).
                    extensions.push((h.id, head_end));
                }
            } else {
                head = Some(row);
                head_end = row.end;
            }
        }

        let tx = self.conn.unchecked_transaction()?;
        for (id, end) in extensions {
            tx.execute(
                "UPDATE intervals SET end_ts = MAX(end_ts, ?2) WHERE id = ?1",
                params![id, end],
            )?;
        }
        for id in &deletions {
            tx.execute("DELETE FROM intervals WHERE id = ?1", [id])?;
        }
        tx.commit()?;
        Ok(deletions.len())
    }
}

/// Applies tracker effects to the database, remembering which row is open.
pub struct Recorder {
    db: Db,
    /// Row id of the interval currently open, if any.
    open_id: Option<i64>,
    /// Row id of the listening row currently open, if any.
    open_listen_id: Option<i64>,
}

impl Recorder {
    /// Wrap a database.
    pub fn new(db: Db) -> Self {
        Self {
            db,
            open_id: None,
            open_listen_id: None,
        }
    }

    /// Read access to the database (for reports).
    pub fn db(&self) -> &Db {
        &self.db
    }

    /// Apply a batch of listening effects in order.
    pub fn apply_listen(&mut self, effects: &[ListenEffect]) -> Result<()> {
        for effect in effects {
            match effect {
                ListenEffect::Opened { start_ts, player } => {
                    // Re-open the same player's previous row after a short pause.
                    let id = match self.db.find_listen_mergeable(
                        *start_ts,
                        player,
                        LISTEN_MERGE_GAP_SECS,
                    )? {
                        Some(id) => id,
                        None => self.db.insert_listen_open(*start_ts, player)?,
                    };
                    self.open_listen_id = Some(id);
                }
                ListenEffect::Extended { end_ts } => {
                    if let Some(id) = self.open_listen_id {
                        self.db.set_listen_end(id, *end_ts)?;
                    }
                }
                ListenEffect::Closed { end_ts } => {
                    if let Some(id) = self.open_listen_id.take() {
                        self.db.finish_listen(id, *end_ts)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Apply a batch of effects in order.
    pub fn apply(&mut self, effects: &[Effect]) -> Result<()> {
        for effect in effects {
            match effect {
                Effect::Opened { start_ts, snapshot } => {
                    // Re-open the previous row if this is just a flicker away
                    // from and back to the same window; otherwise start a new row.
                    let id = match self
                        .db
                        .find_mergeable(*start_ts, snapshot, MERGE_GAP_SECS)?
                    {
                        Some(id) => id,
                        None => self.db.insert_open(*start_ts, snapshot)?,
                    };
                    self.open_id = Some(id);
                }
                Effect::Extended { end_ts } => {
                    if let Some(id) = self.open_id {
                        self.db.set_end(id, *end_ts)?;
                    }
                }
                Effect::Closed { end_ts } => {
                    if let Some(id) = self.open_id.take() {
                        self.db.finish(id, *end_ts)?;
                    }
                }
            }
        }
        Ok(())
    }
}

/// Read SQLite's `user_version` number (0 for a brand-new file).
fn read_user_version(conn: &Connection) -> Result<i32> {
    Ok(conn.query_row("PRAGMA user_version", [], |row| row.get(0))?)
}

/// Copy the database file next to itself as `<name>.bak-v<old version>`.
/// Returns the backup path.
pub fn backup_database(path: &Path, from_version: i32) -> Result<PathBuf> {
    let mut backup_name = path.as_os_str().to_owned();
    backup_name.push(format!(".bak-v{from_version}"));
    let backup = PathBuf::from(backup_name);
    std::fs::copy(path, &backup)
        .with_context(|| format!("backing up database to {}", backup.display()))?;
    Ok(backup)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracker::Tracker;

    /// Build a snapshot quickly for tests.
    fn snap(class: &str) -> WindowSnapshot {
        WindowSnapshot {
            app_class: class.into(),
            title: String::new(),
            workspace: "1".into(),
        }
    }

    /// Feed (time, window) samples through a tracker into a recorder.
    fn run(samples: &[(i64, Option<&str>)]) -> Recorder {
        let mut recorder = Recorder::new(Db::open_in_memory("testbox").unwrap());
        let mut tracker = Tracker::new();
        for (now, class) in samples {
            let effects = tracker.observe(*now, class.map(snap));
            recorder.apply(&effects).unwrap();
        }
        recorder
    }

    #[test]
    fn records_two_apps_with_correct_durations() {
        let r = run(&[
            (1000, Some("zen")),
            (1030, Some("zen")),
            (1060, Some("wezterm")),
            (1090, Some("wezterm")),
        ]);
        let rows = r.db().recent(10).unwrap();
        assert_eq!(rows.len(), 2);
        // Newest first: wezterm 1060..1090, then zen 1000..1060.
        assert_eq!(rows[0].app_class, "wezterm");
        assert_eq!(rows[0].duration_secs(), 30);
        assert_eq!(rows[1].app_class, "zen");
        assert_eq!(rows[1].duration_secs(), 60);
        assert_eq!(rows[1].device_id, "testbox");
    }

    #[test]
    fn flicker_shorter_than_minimum_is_deleted() {
        let r = run(&[
            (1000, Some("zen")),
            (1030, Some("zen")),
            // Passed over for under a second.
            (1030, Some("steam")),
            (1030, Some("wezterm")),
            (1060, Some("wezterm")),
        ]);
        let classes: Vec<_> = r
            .db()
            .recent(10)
            .unwrap()
            .into_iter()
            .map(|i| i.app_class)
            .collect();
        assert_eq!(classes, vec!["wezterm", "zen"]);
    }

    #[test]
    fn flicker_away_and_back_merges_into_one_row() {
        let r = run(&[
            (1000, Some("zen")),
            (1030, Some("zen")),
            // Passes over another window for under a second, then back.
            (1060, Some("steam")),
            (1060, Some("zen")),
            (1090, Some("zen")),
        ]);
        let rows = r.db().recent(10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].app_class, "zen");
        assert_eq!(rows[0].duration_secs(), 90);
    }

    #[test]
    fn a_real_gap_is_not_merged() {
        let r = run(&[
            (1000, Some("zen")),
            (1060, None), // no window focused for 10 seconds
            (1070, Some("zen")),
            (1100, Some("zen")),
        ]);
        let rows = r.db().recent(10).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].duration_secs(), 30);
        assert_eq!(rows[1].duration_secs(), 60);
    }

    #[test]
    fn different_windows_are_never_merged() {
        let r = run(&[
            (1000, Some("zen")),
            (1060, Some("wezterm")),
            (1120, Some("wezterm")),
        ]);
        assert_eq!(r.db().recent(10).unwrap().len(), 2);
    }

    #[test]
    fn sleep_gap_is_not_counted() {
        let r = run(&[
            (1000, Some("zen")),
            (1030, Some("zen")),
            (5000, Some("zen")), // woke up an hour later
            (5030, Some("zen")),
        ]);
        let rows = r.db().recent(10).unwrap();
        assert_eq!(rows.len(), 2);
        let total: i64 = rows.iter().map(|i| i.duration_secs()).sum();
        assert_eq!(total, 30 + 30);
    }

    #[test]
    fn going_away_pulls_the_end_back_to_the_last_input() {
        use crate::presence::PresenceState;
        let mut recorder = Recorder::new(Db::open_in_memory("testbox").unwrap());
        let mut tracker = Tracker::new();
        // Heartbeats keep extending the interval up to 1060...
        for now in [1000, 1030, 1060] {
            recorder
                .apply(&tracker.observe(now, Some(snap("zen"))))
                .unwrap();
        }
        // ...but idle is only noticed at 1100, and the last input was at 1040.
        recorder
            .apply(&tracker.set_presence(1100, PresenceState::Away { since: 1040 }))
            .unwrap();
        let rows = recorder.db().recent(10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].duration_secs(), 40);
    }

    #[test]
    fn totals_group_by_app_and_clip_to_window_start() {
        let r = run(&[
            (1000, Some("zen")),
            (1060, Some("zen")),
            (1060, Some("wezterm")),
            (1090, Some("wezterm")),
        ]);
        let totals = r.db().totals_since(1030).unwrap();
        // zen counts only 1030..1060 = 30s; wezterm counts all 30s.
        assert_eq!(totals.len(), 2);
        assert!(totals.contains(&("zen".to_string(), 30)));
        assert!(totals.contains(&("wezterm".to_string(), 30)));
    }

    /// Insert a finished interval directly (for cleanup tests).
    fn add_row(db: &Db, start: i64, end: i64, class: &str, title: &str) {
        let snapshot = WindowSnapshot {
            app_class: class.into(),
            title: title.into(),
            workspace: "1".into(),
        };
        let id = db.insert_open(start, &snapshot).unwrap();
        db.finish(id, end).unwrap();
    }

    #[test]
    fn rewrite_titles_changes_only_what_the_closure_changes() {
        let db = Db::open_in_memory("box").unwrap();
        add_row(&db, 100, 110, "discord", "#tcg");
        add_row(&db, 110, 120, "discord", "#general");
        add_row(&db, 120, 130, "wezterm", "fish");
        let changed = db
            .rewrite_titles(|class, title| {
                if class == "discord" {
                    String::new()
                } else {
                    title.to_string()
                }
            })
            .unwrap();
        assert_eq!(changed, 2);
        let titles: Vec<_> = db
            .recent(10)
            .unwrap()
            .into_iter()
            .map(|r| r.title)
            .collect();
        assert_eq!(titles, vec!["fish", "", ""]);
    }

    #[test]
    fn merge_adjacent_joins_identical_neighbours() {
        let db = Db::open_in_memory("box").unwrap();
        // Three Discord rows that became identical, then another app, then one more row.
        add_row(&db, 100, 110, "discord", "");
        add_row(&db, 110, 120, "discord", "");
        add_row(&db, 121, 130, "discord", "");
        add_row(&db, 130, 150, "zen", "");
        add_row(&db, 150, 160, "discord", "");
        let removed = db.merge_adjacent(2).unwrap();
        assert_eq!(removed, 2);
        let rows = db.recent(10).unwrap();
        assert_eq!(rows.len(), 3);
        // Oldest first: the three discord rows became one covering 100..130.
        let first = rows.last().unwrap();
        assert_eq!((first.start_ts, first.end_ts), (100, 130));
    }

    #[test]
    fn merge_adjacent_respects_the_gap_and_never_touches_the_newest_row() {
        let db = Db::open_in_memory("box").unwrap();
        add_row(&db, 100, 110, "discord", "");
        add_row(&db, 200, 210, "discord", ""); // far away: separate
        add_row(&db, 210, 220, "discord", ""); // newest row: protected, so it stays
        let removed = db.merge_adjacent(2).unwrap();
        assert_eq!(removed, 0);
        assert_eq!(db.recent(10).unwrap().len(), 3);
    }

    #[test]
    fn backup_to_writes_a_readable_copy() {
        let dir = std::env::temp_dir().join(format!("timesink-vacuum-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("live.db"), "box").unwrap();
        add_row(&db, 100, 110, "zen", "");

        let backup_path = dir.join("copy.db");
        db.backup_to(&backup_path).unwrap();
        let copy = Db::open(&backup_path, "box").unwrap();
        assert_eq!(copy.recent(10).unwrap().len(), 1);
        // A second backup to the same path must fail rather than overwrite.
        assert!(db.backup_to(&backup_path).is_err());

        drop((db, copy));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn backup_copies_the_file_with_version_suffix() {
        let dir = std::env::temp_dir().join(format!("timesink-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("t.db");
        std::fs::write(&db_path, b"pretend database").unwrap();

        let backup = backup_database(&db_path, 1).unwrap();
        assert!(backup.to_string_lossy().ends_with("t.db.bak-v1"));
        assert_eq!(std::fs::read(&backup).unwrap(), b"pretend database");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn reopening_a_current_database_makes_no_backup() {
        let dir = std::env::temp_dir().join(format!("timesink-reopen-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("t.db");
        drop(Db::open(&db_path, "box").unwrap());
        drop(Db::open(&db_path, "box").unwrap());
        let backups = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".bak"))
            .count();
        assert_eq!(backups, 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

#[cfg(test)]
mod merge_regression {
    use super::*;

    /// Per-app total seconds, straight from the table.
    fn totals(db: &Db) -> std::collections::BTreeMap<String, i64> {
        db.totals_since(0).unwrap().into_iter().collect()
    }

    #[test]
    fn merging_never_loses_time_for_any_app() {
        let db = Db::open_in_memory("box").unwrap();
        // Pseudo-random but repeatable rows: a few apps, small gaps, short durations.
        let apps = ["discord", "zen", "nautilus", "wezterm"];
        let mut seed: u64 = 12345;
        let mut next = |n: u64| {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) % n
        };
        let mut t = 1000;
        for _ in 0..600 {
            let app = apps[next(4) as usize];
            let dur = 1 + next(40) as i64;
            let gap = [0, 0, 1, 2, 5, 30][next(6) as usize];
            let snapshot = WindowSnapshot {
                app_class: app.into(),
                title: String::new(),
                workspace: "1".into(),
            };
            let id = db.insert_open(t, &snapshot).unwrap();
            db.finish(id, t + dur).unwrap();
            t += dur + gap;
        }
        let before = totals(&db);
        let removed = db.merge_adjacent(MERGE_GAP_SECS).unwrap();
        let after = totals(&db);
        assert!(removed > 0, "the test data should contain mergeable rows");
        for (app, secs) in &before {
            assert!(
                after[app] >= *secs,
                "{app}: {secs}s before, {}s after merging",
                after[app]
            );
        }
    }
}

#[cfg(test)]
mod listening_tests {
    use super::*;

    /// Shorthand for a list of effects building one listening session.
    fn open(start: i64, player: &str) -> ListenEffect {
        ListenEffect::Opened {
            start_ts: start,
            player: player.into(),
        }
    }

    #[test]
    fn records_and_totals_listening() {
        let mut r = Recorder::new(Db::open_in_memory("box").unwrap());
        r.apply_listen(&[open(1000, "cider")]).unwrap();
        r.apply_listen(&[ListenEffect::Extended { end_ts: 1030 }])
            .unwrap();
        r.apply_listen(&[ListenEffect::Closed { end_ts: 1100 }])
            .unwrap();
        r.apply_listen(&[open(2000, "mpv"), ListenEffect::Closed { end_ts: 2040 }])
            .unwrap();
        let totals = r.db().listening_since(0).unwrap();
        assert_eq!(totals, vec![("cider".into(), 100), ("mpv".into(), 40)]);
    }

    #[test]
    fn totals_only_count_time_inside_the_window() {
        let mut r = Recorder::new(Db::open_in_memory("box").unwrap());
        r.apply_listen(&[open(1000, "cider"), ListenEffect::Closed { end_ts: 1100 }])
            .unwrap();
        // Only the last 40 seconds of the row are after 1060.
        assert_eq!(
            r.db().listening_since(1060).unwrap(),
            vec![("cider".into(), 40)]
        );
    }

    #[test]
    fn short_pause_between_tracks_joins_the_same_row() {
        let mut r = Recorder::new(Db::open_in_memory("box").unwrap());
        r.apply_listen(&[open(1000, "cider"), ListenEffect::Closed { end_ts: 1100 }])
            .unwrap();
        // Same player resumes 6 seconds later: one row, not two.
        r.apply_listen(&[open(1106, "cider"), ListenEffect::Closed { end_ts: 1200 }])
            .unwrap();
        // A different player right after does not join.
        r.apply_listen(&[open(1201, "mpv"), ListenEffect::Closed { end_ts: 1260 }])
            .unwrap();
        let rows: i64 = r
            .db()
            .conn
            .query_row("SELECT COUNT(*) FROM listening", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 2);
        assert_eq!(
            r.db().listening_since(0).unwrap(),
            vec![("cider".into(), 200), ("mpv".into(), 59)]
        );
    }

    #[test]
    fn one_second_blips_are_dropped() {
        let mut r = Recorder::new(Db::open_in_memory("box").unwrap());
        r.apply_listen(&[open(1000, "cider"), ListenEffect::Closed { end_ts: 1000 }])
            .unwrap();
        assert!(r.db().listening_since(0).unwrap().is_empty());
    }

    #[test]
    fn upgrading_a_v1_database_backs_it_up_and_keeps_the_data() {
        let dir = std::env::temp_dir().join(format!("timesink-mig-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.db");

        // Build a version 1 database by hand, as the older program did.
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE intervals (
                     id INTEGER PRIMARY KEY, device_id TEXT NOT NULL,
                     start_ts INTEGER NOT NULL, end_ts INTEGER NOT NULL,
                     app_class TEXT NOT NULL, title TEXT NOT NULL,
                     workspace TEXT NOT NULL);
                 INSERT INTO intervals VALUES (1,'box',100,160,'zen','','1');",
            )
            .unwrap();
            conn.pragma_update(None, "user_version", 1).unwrap();
        }

        let db = Db::open(&path, "box").unwrap();
        // The old data is untouched and the new table exists and is empty.
        assert_eq!(db.totals_since(0).unwrap(), vec![("zen".into(), 60)]);
        assert!(db.listening_since(0).unwrap().is_empty());
        // A backup of the version 1 file was written first.
        assert!(dir.join("t.db.bak-v1").exists());
        assert_eq!(
            read_user_version(&Connection::open(&path).unwrap()).unwrap(),
            SCHEMA_VERSION
        );
        drop(db);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn range_queries_return_overlapping_rows_only() {
        let mut r = Recorder::new(Db::open_in_memory("box").unwrap());
        r.apply_listen(&[open(100, "a"), ListenEffect::Closed { end_ts: 200 }])
            .unwrap();
        r.apply_listen(&[open(500, "b"), ListenEffect::Closed { end_ts: 600 }])
            .unwrap();
        let rows = r.db().listening_between(150, 300, None).unwrap();
        assert_eq!(rows, vec![("a".to_string(), 100, 200)]);
        assert_eq!(r.db().listening_between(0, 1000, None).unwrap().len(), 2);
        assert!(r.db().intervals_between(0, 1000, None).unwrap().is_empty());
    }

    #[test]
    fn site_reports_extend_start_and_close_stretches() {
        let db = Db::open_in_memory("test").unwrap();
        db.record_domain(100, Some("github.com"), "a").unwrap();
        db.record_domain(130, Some("github.com"), "a").unwrap();
        db.record_domain(160, Some("github.com"), "a").unwrap();
        // Switching sites closes the first stretch at the switch time.
        db.record_domain(190, Some("wikipedia.org"), "a").unwrap();
        db.record_domain(220, None, "a").unwrap();
        // The same site again after a "none" starts a fresh stretch, not a bridge.
        db.record_domain(250, Some("wikipedia.org"), "a").unwrap();
        let rows = db.domains_between(0, 1000, None).unwrap();
        assert_eq!(
            rows,
            vec![
                ("github.com".to_string(), 100, 190),
                ("wikipedia.org".to_string(), 190, 220),
                ("wikipedia.org".to_string(), 250, 250),
            ]
        );
    }

    #[test]
    fn a_long_silence_does_not_invent_time() {
        let db = Db::open_in_memory("test").unwrap();
        db.record_domain(100, Some("github.com"), "a").unwrap();
        db.record_domain(130, Some("github.com"), "a").unwrap();
        // The extension went quiet (browser closed, machine asleep) for 10 minutes.
        db.record_domain(730, Some("github.com"), "a").unwrap();
        let rows = db.domains_between(0, 2000, None).unwrap();
        assert_eq!(rows[0], ("github.com".to_string(), 100, 130));
        assert_eq!(rows[1].1, 730);
    }

    #[test]
    fn forgetting_one_site_or_all() {
        let db = Db::open_in_memory("test").unwrap();
        db.record_domain(10, Some("a.com"), "a").unwrap();
        db.record_domain(50, Some("b.com"), "a").unwrap();
        assert_eq!(db.forget_domains(Some("a.com")).unwrap(), 1);
        assert_eq!(db.domains_between(0, 100, None).unwrap().len(), 1);
        assert_eq!(db.forget_domains(None).unwrap(), 1);
        assert!(db.domains_between(0, 100, None).unwrap().is_empty());
    }

    #[test]
    fn two_browsers_do_not_close_each_others_stretches() {
        let db = Db::open_in_memory("box").unwrap();
        // Browser A shows github; browser B (another helper) reports "nothing" while unfocused.
        db.record_domain(100, Some("github.com"), "a").unwrap();
        db.record_domain(110, None, "b").unwrap();
        db.record_domain(130, Some("github.com"), "a").unwrap();
        db.record_domain(140, None, "b").unwrap();
        db.record_domain(160, Some("github.com"), "a").unwrap();
        // One unbroken stretch from A.
        assert_eq!(
            db.domains_between(0, 1000, None).unwrap(),
            vec![("github.com".to_string(), 100, 160)]
        );
        // B then takes over while A goes quiet.
        db.record_domain(170, None, "a").unwrap();
        db.record_domain(170, Some("wikipedia.org"), "b").unwrap();
        let rows = db.domains_between(0, 1000, None).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].0, "wikipedia.org");
        assert_eq!(
            db.latest_domain_report().unwrap().unwrap().0,
            "wikipedia.org"
        );
    }

    #[test]
    fn a_v3_database_gains_the_source_column_with_a_backup() {
        let dir = std::env::temp_dir().join(format!("timesink-v3-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE intervals (id INTEGER PRIMARY KEY, device_id TEXT NOT NULL,
                     start_ts INTEGER NOT NULL, end_ts INTEGER NOT NULL,
                     app_class TEXT NOT NULL, title TEXT NOT NULL, workspace TEXT NOT NULL);
                 CREATE TABLE listening (id INTEGER PRIMARY KEY, device_id TEXT NOT NULL,
                     start_ts INTEGER NOT NULL, end_ts INTEGER NOT NULL, player TEXT NOT NULL);
                 CREATE TABLE domains (id INTEGER PRIMARY KEY, device_id TEXT NOT NULL,
                     start_ts INTEGER NOT NULL, end_ts INTEGER NOT NULL, domain TEXT NOT NULL,
                     closed INTEGER NOT NULL DEFAULT 0);
                 INSERT INTO domains VALUES (1,'box',100,160,'github.com',1);",
            )
            .unwrap();
            conn.pragma_update(None, "user_version", 3).unwrap();
        }
        let db = Db::open(&path, "box").unwrap();
        assert!(dir.join("t.db.bak-v3").exists());
        assert_eq!(
            db.domains_between(0, 1000, None).unwrap(),
            vec![("github.com".to_string(), 100, 160)]
        );
        db.record_domain(500, Some("a.com"), "x").unwrap();
        assert_eq!(db.domains_between(0, 1000, None).unwrap().len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A database for the computer called `name`, with the schema applied.
    fn named(name: &str) -> Db {
        Db::open_in_memory(name).unwrap()
    }

    #[test]
    fn own_changes_mark_their_day_but_downloads_do_not() {
        use crate::syncdoc::{DayRows, DAY_SECS};
        let db = named("mangrove");
        assert_eq!(db.take_dirty_days().unwrap(), Vec::<i64>::new());

        // Recording something marks its UTC day.
        let snap = WindowSnapshot {
            app_class: "zen".into(),
            title: String::new(),
            workspace: "1".into(),
        };
        let id = db.insert_open(3 * DAY_SECS + 50, &snap).unwrap();
        assert_eq!(db.take_dirty_days().unwrap(), vec![3]);
        // Taking clears them; extending the row marks the day again.
        assert!(db.take_dirty_days().unwrap().is_empty());
        db.set_end(id, 3 * DAY_SECS + 90).unwrap();
        assert_eq!(db.take_dirty_days().unwrap(), vec![3]);

        // Listening and site rows mark their days too.
        db.insert_listen_open(5 * DAY_SECS, "spotify").unwrap();
        db.record_domain(7 * DAY_SECS, Some("github.com"), "a")
            .unwrap();
        assert_eq!(db.take_dirty_days().unwrap(), vec![5, 7]);

        // Deleting (forgetting) marks the day as changed.
        db.forget_domains(None).unwrap();
        assert_eq!(db.take_dirty_days().unwrap(), vec![7]);

        // Another computer's downloaded rows never mark anything (so uploads cannot echo).
        let rows = DayRows {
            intervals: vec![(9 * DAY_SECS, 9 * DAY_SECS + 60, "steam".into(), "1".into())],
            listening: vec![(9 * DAY_SECS, 9 * DAY_SECS + 60, "spotify".into())],
            domains: vec![(9 * DAY_SECS, 9 * DAY_SECS + 60, "gog.com".into())],
        };
        db.replace_day("deck", 9, &rows).unwrap();
        assert!(db.take_dirty_days().unwrap().is_empty());
        db.replace_day("deck", 9, &DayRows::default()).unwrap();
        assert!(db.take_dirty_days().unwrap().is_empty());
    }

    #[test]
    fn replacing_a_day_swaps_only_that_computers_rows_for_that_day() {
        use crate::syncdoc::{DayRows, DAY_SECS};
        let db = named("mangrove");
        let snap = WindowSnapshot {
            app_class: "zen".into(),
            title: String::new(),
            workspace: "1".into(),
        };
        db.insert_open(10 * DAY_SECS, &snap).unwrap();
        let day = |n: i64, app: &str| DayRows {
            intervals: vec![(n * DAY_SECS + 5, n * DAY_SECS + 65, app.into(), "2".into())],
            ..DayRows::default()
        };
        db.replace_day("deck", 10, &day(10, "steam")).unwrap();
        db.replace_day("deck", 11, &day(11, "steam")).unwrap();
        db.replace_day("laptop", 10, &day(10, "kitty")).unwrap();
        // A second download of the same day replaces, it does not add.
        db.replace_day("deck", 10, &day(10, "heroic")).unwrap();

        let all = db.intervals_between(0, 20 * DAY_SECS, None).unwrap();
        let apps: Vec<(&str, &str)> = all
            .iter()
            .map(|r| (r.device_id.as_str(), r.app_class.as_str()))
            .collect();
        assert_eq!(
            apps,
            vec![
                ("mangrove", "zen"),
                // Same start time as the deck's: the older row (laptop) comes first.
                ("laptop", "kitty"),
                ("deck", "heroic"),
                ("deck", "steam")
            ]
        );
        // Downloaded rows carry no title, and the device filter works.
        assert!(all
            .iter()
            .filter(|r| r.device_id == "deck")
            .all(|r| r.title.is_empty()));
        let deck = db
            .intervals_between(0, 20 * DAY_SECS, Some("deck"))
            .unwrap();
        assert_eq!(deck.len(), 2);
        assert_eq!(db.devices().unwrap(), vec!["deck", "laptop", "mangrove"]);
        // Own rows are never overwritten by a download.
        assert!(db.replace_day("mangrove", 10, &day(10, "x")).is_err());
        // day_rows returns only this computer's rows for that day.
        let mine = db.day_rows(10).unwrap();
        assert_eq!(mine.intervals.len(), 1);
        assert_eq!(mine.intervals[0].2, "zen");
    }

    #[test]
    fn revisions_and_state_are_remembered() {
        let db = named("mangrove");
        assert_eq!(db.doc_rev("day:mangrove:2026-11-10").unwrap(), None);
        db.set_doc_rev("day:mangrove:2026-11-10", Some("1-a"))
            .unwrap();
        db.set_doc_rev("day:mangrove:2026-11-10", Some("2-b"))
            .unwrap();
        assert_eq!(
            db.doc_rev("day:mangrove:2026-11-10").unwrap().as_deref(),
            Some("2-b")
        );
        db.set_doc_rev("day:mangrove:2026-11-10", None).unwrap();
        assert_eq!(db.doc_rev("day:mangrove:2026-11-10").unwrap(), None);
        assert_eq!(db.sync_state("seq").unwrap(), None);
        db.set_sync_state("seq", "12-xyz").unwrap();
        assert_eq!(db.sync_state("seq").unwrap().as_deref(), Some("12-xyz"));
        db.mark_dirty(4).unwrap();
        db.mark_dirty(4).unwrap();
        assert_eq!(db.dirty_count().unwrap(), 1);
    }

    #[test]
    fn a_v4_database_gets_sync_tables_backup_and_marks_old_days() {
        let dir = std::env::temp_dir().join(format!("timesink-v4-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE intervals (id INTEGER PRIMARY KEY, device_id TEXT NOT NULL,
                     start_ts INTEGER NOT NULL, end_ts INTEGER NOT NULL,
                     app_class TEXT NOT NULL, title TEXT NOT NULL, workspace TEXT NOT NULL);
                 CREATE TABLE listening (id INTEGER PRIMARY KEY, device_id TEXT NOT NULL,
                     start_ts INTEGER NOT NULL, end_ts INTEGER NOT NULL, player TEXT NOT NULL);
                 CREATE TABLE domains (id INTEGER PRIMARY KEY, device_id TEXT NOT NULL,
                     start_ts INTEGER NOT NULL, end_ts INTEGER NOT NULL, domain TEXT NOT NULL,
                     closed INTEGER NOT NULL DEFAULT 0, source TEXT NOT NULL DEFAULT '');
                 INSERT INTO intervals VALUES (1,'box',86500,86560,'zen','t','1');
                 INSERT INTO intervals VALUES (2,'box',260000,260060,'zen','t','1');
                 INSERT INTO intervals VALUES (3,'other',500000,500060,'zen','t','1');
                 INSERT INTO listening VALUES (1,'box',432000,432060,'spotify');",
            )
            .unwrap();
            conn.pragma_update(None, "user_version", 4).unwrap();
        }
        let db = Db::open(&path, "box").unwrap();
        assert!(dir.join("t.db.bak-v4").exists());
        // Every old day of THIS computer is waiting for its first upload; others are not.
        assert_eq!(db.take_dirty_days().unwrap(), vec![1, 3, 5]);
        assert_eq!(db.intervals_between(0, 10_000_000, None).unwrap().len(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn other_computers_lists_are_kept_and_forgotten() {
        let db = Db::open_in_memory("pixel").unwrap();
        let lists = crate::syncdoc::ConfLists {
            apps: "zen zen Browser\n".into(),
            apps_updated: 3,
            domains: String::new(),
            domains_updated: 0,
        };
        assert!(db.remote_confs().unwrap().is_empty());
        db.set_remote_conf("mangrove", Some(&lists)).unwrap();
        assert_eq!(
            db.remote_confs().unwrap(),
            vec![("mangrove".to_string(), lists.clone())]
        );
        db.set_remote_conf("mangrove", None).unwrap();
        assert!(db.remote_confs().unwrap().is_empty());
        db.set_own_conf_uploaded(&lists).unwrap();
        assert_eq!(db.own_conf_uploaded().unwrap(), Some(lists));
    }
}
