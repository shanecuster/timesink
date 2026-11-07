//! SQLite storage for focus intervals.
//!
//! One row per interval. Rows are written as soon as an interval opens and their
//! end time is moved forward on every heartbeat, so a crash or power loss only
//! loses a few seconds, never a whole session.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::{params, Connection};

use crate::model::{StoredInterval, WindowSnapshot};
use crate::tracker::Effect;

/// Version of the database layout. Bump it and add a migration step when the
/// layout changes. Stored in SQLite's built-in `user_version` field.
pub const SCHEMA_VERSION: i32 = 1;

/// Intervals shorter than this many seconds are deleted when they close.
/// This drops the flicker from quickly passing over windows.
pub const MIN_INTERVAL_SECS: i64 = 1;

/// A new interval that starts within this many seconds of the previous one
/// ending, with the exact same window, is joined to it instead of being a new row.
pub const MERGE_GAP_SECS: i64 = 2;

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
}

impl Recorder {
    /// Wrap a database.
    pub fn new(db: Db) -> Self {
        Self { db, open_id: None }
    }

    /// Read access to the database (for reports).
    pub fn db(&self) -> &Db {
        &self.db
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
