//! The terminal interface's remembered choices, kept in `tui.conf`.
//!
//! This is a separate file from `config.conf` on purpose: `config.conf` is hand-written
//! (and read by the daemon), while `tui.conf` is rewritten by the interface whenever you
//! change theme, sort order or the title banner. Keeping them apart means saving a choice
//! can never wipe the comments in a file you maintain yourself.
//!
//! Format, one `key = value` per line (blank lines and `#` comments are ignored):
//! ```text
//! theme = Everforest
//! sort = time
//! banner = on
//! ```

use std::path::Path;

use anyhow::{Context, Result};

/// Everything the interface remembers between runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// Theme name (looked up in [`crate::theme::THEMES`]; unknown names fall back to the default).
    pub theme: String,
    /// `true` = sort the lists A to Z, `false` = most time first.
    pub sort_by_name: bool,
    /// Whether the title banner (name, date, day sparkline) is shown.
    pub banner: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            theme: crate::theme::DEFAULT_THEME.to_string(),
            sort_by_name: false,
            banner: true,
        }
    }
}

impl Settings {
    /// Parse the text of a `tui.conf`. Unknown keys and bad values are ignored, so a file
    /// from a newer or older version never stops the interface from starting.
    pub fn parse(text: &str) -> Settings {
        let mut s = Settings::default();
        for line in text.lines().map(str::trim) {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim();
            match key.trim() {
                "theme" if !value.is_empty() => s.theme = value.to_string(),
                "sort" => match value.to_lowercase().as_str() {
                    "name" => s.sort_by_name = true,
                    "time" => s.sort_by_name = false,
                    _ => {}
                },
                "banner" => match value.to_lowercase().as_str() {
                    "on" | "true" | "yes" => s.banner = true,
                    "off" | "false" | "no" => s.banner = false,
                    _ => {}
                },
                _ => {}
            }
        }
        s
    }

    /// The file text for these settings.
    pub fn render(&self) -> String {
        format!(
            "# Written by the timesink interface; edit freely.\n\
             theme = {}\n\
             sort = {}\n\
             banner = {}\n",
            self.theme,
            if self.sort_by_name { "name" } else { "time" },
            if self.banner { "on" } else { "off" },
        )
    }

    /// Load from `path`. A missing file just means the defaults.
    pub fn load(path: &Path) -> Result<Settings> {
        match std::fs::read_to_string(path) {
            Ok(text) => Ok(Settings::parse(&text)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    /// Save to `path`, creating the folder if needed. Written to a temporary file and
    /// renamed, so a crash mid-write cannot leave a half-written file.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let tmp = path.with_extension("conf.tmp");
        std::fs::write(&tmp, self.render())
            .with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, path).with_context(|| format!("saving {}", path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_when_empty() {
        assert_eq!(Settings::parse(""), Settings::default());
    }

    #[test]
    fn parses_all_keys_ignoring_noise() {
        let s = Settings::parse(
            "# hi\n\ntheme = Nord\nsort=NAME\nbanner = off\nwhat = 1\nsort = sideways\n",
        );
        assert_eq!(s.theme, "Nord");
        // The later bad "sideways" value must not undo the valid "name".
        assert!(s.sort_by_name);
        assert!(!s.banner);
    }

    #[test]
    fn render_then_parse_round_trips() {
        let s = Settings {
            theme: "Catppuccin Macchiato".into(),
            sort_by_name: true,
            banner: false,
        };
        assert_eq!(Settings::parse(&s.render()), s);
    }

    #[test]
    fn save_and_load_use_a_real_file() {
        let dir = std::env::temp_dir().join(format!("timesink-settings-{}", std::process::id()));
        let path = dir.join("nested").join("tui.conf");
        assert_eq!(Settings::load(&path).unwrap(), Settings::default());
        let s = Settings {
            theme: "Dracula".into(),
            sort_by_name: true,
            banner: true,
        };
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path).unwrap(), s);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
