//! User settings read from `~/.config/timesink/config.conf`.
//!
//! The file uses the same simple format as `hosts.conf`: one `key = value`
//! per line, `#` starts a comment. Unknown keys and bad values are ignored,
//! so a typo can never stop the daemon from starting.
//!
//! ```text
//! # minutes without input before time stops counting
//! idle_timeout_minutes = 5
//!
//! # how much of a terminal window to record: app, program or program_dir
//! terminal_detail = program
//!
//! # window classes (comma separated) whose full titles may be recorded.
//! # Empty by default: every other app is recorded as just the app.
//! keep_titles =
//! ```

/// How long with no input before you count as away, if not configured.
pub const DEFAULT_IDLE_TIMEOUT_SECS: i64 = 5 * 60;

/// How much of a terminal window's title is recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TerminalDetail {
    /// Only the terminal app itself.
    App,
    /// The program that is running, e.g. `fish`, `tuxwire` or `ssh: proxmox`.
    #[default]
    Program,
    /// The program and its directory, e.g. `fish @ ~/Projects`.
    ProgramDir,
}

impl TerminalDetail {
    /// Parse the value of the `terminal_detail` setting.
    fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "app" => Some(Self::App),
            "program" => Some(Self::Program),
            "program_dir" => Some(Self::ProgramDir),
            _ => None,
        }
    }
}

/// The settings timesink understands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Seconds without keyboard or mouse input before you count as away.
    pub idle_timeout_secs: i64,
    /// How much of a terminal window to record.
    pub terminal_detail: TerminalDetail,
    /// Lowercase window classes whose full title may be recorded (usually none).
    pub keep_titles: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            idle_timeout_secs: DEFAULT_IDLE_TIMEOUT_SECS,
            terminal_detail: TerminalDetail::default(),
            keep_titles: Vec::new(),
        }
    }
}

impl Config {
    /// Parse the text of a `config.conf` file.
    pub fn from_conf(text: &str) -> Self {
        let mut config = Self::default();
        for line in text.lines().map(str::trim) {
            // Skip blank lines and comments.
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            match key.trim() {
                "idle_timeout_minutes" => {
                    // Accept whole minutes of at least 1; anything else keeps the default.
                    if let Ok(minutes) = value.trim().parse::<i64>() {
                        if minutes >= 1 {
                            config.idle_timeout_secs = minutes * 60;
                        }
                    }
                }
                "terminal_detail" => {
                    if let Some(detail) = TerminalDetail::parse(value) {
                        config.terminal_detail = detail;
                    }
                }
                "keep_titles" => {
                    config.keep_titles = value
                        .split(',')
                        .map(|class| class.trim().to_lowercase())
                        .filter(|class| !class.is_empty())
                        .collect();
                }
                _ => {}
            }
        }
        config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults() {
        let c = Config::default();
        assert_eq!(c.idle_timeout_secs, 300);
        assert_eq!(c.terminal_detail, TerminalDetail::Program);
        assert!(c.keep_titles.is_empty());
        assert_eq!(Config::from_conf(""), c);
    }

    #[test]
    fn reads_idle_timeout() {
        let c = Config::from_conf("# comment\nidle_timeout_minutes = 10\n");
        assert_eq!(c.idle_timeout_secs, 600);
    }

    #[test]
    fn reads_terminal_detail() {
        for (text, expected) in [
            ("terminal_detail = app", TerminalDetail::App),
            ("terminal_detail = program", TerminalDetail::Program),
            ("terminal_detail = program_dir", TerminalDetail::ProgramDir),
        ] {
            assert_eq!(Config::from_conf(text).terminal_detail, expected, "{text}");
        }
    }

    #[test]
    fn reads_keep_titles_as_lowercase_list() {
        let c = Config::from_conf("keep_titles = Org.Gnome.Nautilus, , Obsidian\n");
        assert_eq!(c.keep_titles, vec!["org.gnome.nautilus", "obsidian"]);
        assert!(Config::from_conf("keep_titles =").keep_titles.is_empty());
    }

    #[test]
    fn bad_values_and_unknown_keys_are_ignored() {
        for text in [
            "idle_timeout_minutes = abc",
            "idle_timeout_minutes = 0",
            "idle_timeout_minutes = -3",
            "terminal_detail = everything",
            "something_else = 9",
            "no equals sign",
        ] {
            assert_eq!(Config::from_conf(text), Config::default(), "{text}");
        }
    }
}
