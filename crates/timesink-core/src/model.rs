//! Plain data types shared by the collector, the database and (later) the TUI.

/// What is focused at one moment, *after* privacy cleaning.
///
/// Two snapshots that compare equal are treated as "the same activity", so time
/// keeps accumulating on one interval instead of starting a new one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowSnapshot {
    /// The application's window class from Hyprland, e.g. `org.wezfurlong.wezterm`.
    pub app_class: String,
    /// The cleaned title. Empty for browsers (only the app is recorded for them).
    pub title: String,
    /// The workspace name the window is on.
    pub workspace: String,
}

/// One stored stretch of focused time, as read back from the database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredInterval {
    /// Database row id.
    pub id: i64,
    /// Which computer recorded it. Lets several machines share one database later.
    pub device_id: String,
    /// Start time, in seconds since the Unix epoch.
    pub start_ts: i64,
    /// End time, in seconds since the Unix epoch.
    pub end_ts: i64,
    /// Window class of the focused application.
    pub app_class: String,
    /// Cleaned window title (may be empty).
    pub title: String,
    /// Workspace name.
    pub workspace: String,
}

impl StoredInterval {
    /// Length of the interval in seconds (never negative).
    pub fn duration_secs(&self) -> i64 {
        (self.end_ts - self.start_ts).max(0)
    }
}
