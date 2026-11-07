//! User settings read from `~/.config/timesink/config.conf`.
//!
//! The file uses the same simple format as `hosts.conf`: one `key = value`
//! per line, `#` starts a comment. Unknown keys and bad values are ignored,
//! so a typo can never stop the daemon from starting.
//!
//! ```text
//! # minutes without input before time stops counting
//! idle_timeout_minutes = 5
//! ```

/// How long with no input before you count as away, if not configured.
pub const DEFAULT_IDLE_TIMEOUT_SECS: i64 = 5 * 60;

/// The settings timesink understands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Seconds without keyboard or mouse input before you count as away.
    pub idle_timeout_secs: i64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            idle_timeout_secs: DEFAULT_IDLE_TIMEOUT_SECS,
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
            if key.trim() == "idle_timeout_minutes" {
                // Accept whole minutes of at least 1; anything else keeps the default.
                if let Ok(minutes) = value.trim().parse::<i64>() {
                    if minutes >= 1 {
                        config.idle_timeout_secs = minutes * 60;
                    }
                }
            }
        }
        config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_five_minutes() {
        assert_eq!(Config::default().idle_timeout_secs, 300);
        assert_eq!(Config::from_conf("").idle_timeout_secs, 300);
    }

    #[test]
    fn reads_idle_timeout() {
        let c = Config::from_conf("# comment\nidle_timeout_minutes = 10\n");
        assert_eq!(c.idle_timeout_secs, 600);
    }

    #[test]
    fn bad_values_and_unknown_keys_are_ignored() {
        for text in [
            "idle_timeout_minutes = abc",
            "idle_timeout_minutes = 0",
            "idle_timeout_minutes = -3",
            "something_else = 9",
            "no equals sign",
        ] {
            assert_eq!(Config::from_conf(text).idle_timeout_secs, 300, "{text}");
        }
    }
}
