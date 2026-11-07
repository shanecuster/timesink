//! Privacy rules: decide what text from a window title is allowed into the database.
//!
//! Defaults are deliberately conservative:
//!   - Browsers: the title is always dropped (a Zen extension may report domains later).
//!   - Terminals: the full command line is NEVER stored, because commands can
//!     contain passwords or tokens. By default only the program name is kept
//!     (`terminal_detail` can reduce this to the app or add the directory).
//!   - SSH sessions: stored as `ssh: <label>`, where the label comes from a local
//!     hosts file. Unknown hosts become `ssh: other`, so IP addresses and user
//!     names never reach the database.
//!   - Everything else: only the app is recorded. Titles of chat channels, email
//!     inboxes, notes and similar are dropped unless the app is listed in
//!     `keep_titles`.

use std::collections::HashMap;

use crate::config::{Config, TerminalDetail};
use crate::title::{parse_terminal_title, split_tab_prefix, ssh_target};

/// What kind of application a window class belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppKind {
    Terminal,
    Browser,
    Other,
}

/// Window-class fragments that identify a terminal emulator (matched case-insensitively).
const TERMINAL_HINTS: &[&str] = &[
    "wezterm",
    "foot",
    "kitty",
    "alacritty",
    "ghostty",
    "konsole",
    "xterm",
];

/// Window-class fragments that identify a web browser (matched case-insensitively).
const BROWSER_HINTS: &[&str] = &[
    "zen",
    "firefox",
    "librewolf",
    "floorp",
    "chrom",
    "brave",
    "vivaldi",
    "opera",
];

/// Guess the kind of application from its Hyprland window class.
pub fn classify(app_class: &str) -> AppKind {
    let class = app_class.to_lowercase();
    if TERMINAL_HINTS.iter().any(|hint| class.contains(hint)) {
        AppKind::Terminal
    } else if BROWSER_HINTS.iter().any(|hint| class.contains(hint)) {
        AppKind::Browser
    } else {
        AppKind::Other
    }
}

/// Applies the privacy rules above. Holds the user's host-to-label mapping.
#[derive(Debug, Clone, Default)]
pub struct Sanitizer {
    /// Maps an ssh host name or IP to a friendly label such as `proxmox`.
    host_labels: HashMap<String, String>,
    /// How much of a terminal window to record.
    terminal_detail: TerminalDetail,
    /// Lowercase window classes whose full title may be recorded.
    keep_titles: Vec<String>,
}

impl Sanitizer {
    /// Build a sanitizer from an explicit host-to-label map.
    pub fn new(host_labels: HashMap<String, String>) -> Self {
        Self {
            host_labels,
            ..Self::default()
        }
    }

    /// Apply the privacy-related settings from `config.conf`.
    pub fn with_config(mut self, config: &Config) -> Self {
        self.terminal_detail = config.terminal_detail;
        self.keep_titles = config.keep_titles.clone();
        self
    }

    /// Build a sanitizer from the text of a `hosts.conf` file.
    ///
    /// One mapping per line in the form `host = label`. Blank lines and lines
    /// starting with `#` are ignored.
    pub fn from_hosts_conf(text: &str) -> Self {
        let host_labels = text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .filter_map(|line| line.split_once('='))
            .map(|(host, label)| (host.trim().to_string(), label.trim().to_string()))
            .filter(|(host, label)| !host.is_empty() && !label.is_empty())
            .collect();
        Self::new(host_labels)
    }

    /// Produce the title that is safe to store for a window.
    pub fn clean_title(&self, app_class: &str, raw_title: &str) -> String {
        match classify(app_class) {
            // Browsers: record only the app, never the page title.
            AppKind::Browser => String::new(),
            AppKind::Terminal => self.clean_terminal_title(raw_title),
            // Other apps: app-only unless the user opted in for this app.
            AppKind::Other if self.keeps_title(app_class) => {
                split_tab_prefix(raw_title).1.to_string()
            }
            AppKind::Other => String::new(),
        }
    }

    /// Re-apply the current rules to a title that is ALREADY stored.
    ///
    /// Used by the cleanup command on old rows. Stored terminal titles look like
    /// `fish @ ~/dir` or `ssh: label`, so they can be shortened but never rebuilt.
    pub fn rescrub_stored(&self, app_class: &str, stored: &str) -> String {
        match classify(app_class) {
            AppKind::Browser => String::new(),
            AppKind::Terminal => match self.terminal_detail {
                TerminalDetail::App => String::new(),
                // Drop a trailing " @ directory"; `ssh: label` has none and stays.
                TerminalDetail::Program => stored.split(" @ ").next().unwrap_or(stored).to_string(),
                TerminalDetail::ProgramDir => stored.to_string(),
            },
            AppKind::Other if self.keeps_title(app_class) => stored.to_string(),
            AppKind::Other => String::new(),
        }
    }

    /// Did the user opt in to recording full titles for this app?
    fn keeps_title(&self, app_class: &str) -> bool {
        self.keep_titles
            .iter()
            .any(|class| class.eq_ignore_ascii_case(app_class))
    }

    /// Reduce a terminal title to `ssh: <label>`, `<program>` or `<program> @ <directory>`.
    fn clean_terminal_title(&self, raw_title: &str) -> String {
        // App-only: nothing from the title is recorded at all.
        if self.terminal_detail == TerminalDetail::App {
            return String::new();
        }
        let parsed = parse_terminal_title(raw_title);
        // The title shows the program as typed (`./target/release/foo`, `/usr/bin/ssh`);
        // keep only the file name so paths never end up in the database.
        let process = parsed.process.as_deref().map(program_name);

        // SSH sessions get a label instead of the real target.
        if let Some(command) = parsed.command.as_deref() {
            if process == Some("ssh") {
                let label = ssh_target(command)
                    .and_then(|host| self.host_labels.get(&host).cloned())
                    .unwrap_or_else(|| "other".to_string());
                return format!("ssh: {label}");
            }
        }

        // Anything else: the program name, plus the directory only if asked for.
        // The command line is never stored.
        match (process, parsed.cwd) {
            (Some(process), Some(cwd)) if self.terminal_detail == TerminalDetail::ProgramDir => {
                format!("{process} @ {cwd}")
            }
            (Some(process), _) => process.to_string(),
            // Unrecognised title format: store nothing rather than guess.
            (None, _) => "terminal".to_string(),
        }
    }
}

/// The file name part of a program path: `./target/release/foo` becomes `foo`.
fn program_name(process: &str) -> &str {
    process.rsplit('/').next().unwrap_or(process)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WEZTERM: &str = "org.wezfurlong.wezterm";

    /// A sanitizer with the default settings that knows one example host
    /// (a TEST-NET address, not a real machine).
    fn sanitizer() -> Sanitizer {
        Sanitizer::from_hosts_conf("# my servers\n192.0.2.10 = proxmox\n\nbox=lab\n")
    }

    /// The same sanitizer with settings parsed from a config.conf text.
    fn sanitizer_with(conf: &str) -> Sanitizer {
        sanitizer().with_config(&Config::from_conf(conf))
    }

    #[test]
    fn classifies_known_apps() {
        assert_eq!(classify(WEZTERM), AppKind::Terminal);
        assert_eq!(classify("foot"), AppKind::Terminal);
        assert_eq!(classify("zen"), AppKind::Browser);
        assert_eq!(classify("firefox"), AppKind::Browser);
        assert_eq!(classify("steam"), AppKind::Other);
    }

    #[test]
    fn browser_titles_are_dropped() {
        assert_eq!(
            sanitizer().clean_title("zen", "Inbox (3) - Private Mail"),
            ""
        );
    }

    #[test]
    fn browser_titles_are_dropped_even_when_listed_in_keep_titles() {
        let s = sanitizer_with("keep_titles = zen");
        assert_eq!(s.clean_title("zen", "Some page - Zen"), "");
    }

    #[test]
    fn other_apps_are_app_only_by_default() {
        let s = sanitizer();
        assert_eq!(s.clean_title("discord", "#tcg | Some Server - Discord"), "");
        assert_eq!(
            s.clean_title("eu.betterbird.Betterbird", "Inbox - me@example.com"),
            ""
        );
        assert_eq!(s.clean_title("steam", "Library"), "");
    }

    #[test]
    fn keep_titles_opts_one_app_in() {
        let s = sanitizer_with("keep_titles = org.gnome.Nautilus");
        assert_eq!(
            s.clean_title("org.gnome.nautilus", "[1/2] Downloads"),
            "Downloads"
        );
        // Other apps stay app-only.
        assert_eq!(s.clean_title("discord", "#tcg"), "");
    }

    #[test]
    fn terminal_default_is_program_only() {
        let t = sanitizer().clean_title(WEZTERM, "[4/4] ~/Projects - fish");
        assert_eq!(t, "fish");
    }

    #[test]
    fn terminal_program_dir_mode_adds_the_directory() {
        let s = sanitizer_with("terminal_detail = program_dir");
        assert_eq!(
            s.clean_title(WEZTERM, "[4/4] ~/Projects - fish"),
            "fish @ ~/Projects"
        );
    }

    #[test]
    fn terminal_app_mode_records_nothing_from_the_title() {
        let s = sanitizer_with("terminal_detail = app");
        assert_eq!(s.clean_title(WEZTERM, "~: ssh admin@192.0.2.10 - ssh"), "");
        assert_eq!(s.clean_title(WEZTERM, "~/Projects - fish"), "");
    }

    #[test]
    fn program_paths_are_reduced_to_the_file_name() {
        let s = sanitizer_with("terminal_detail = program_dir");
        assert_eq!(
            s.clean_title(
                WEZTERM,
                "~/Projects/timesink: ./target/release/timesink-daemon - ./target/release/timesink-daemon"
            ),
            "timesink-daemon @ ~/Projects/timesink"
        );
        // Without the directory setting only the file name remains.
        assert_eq!(
            sanitizer().clean_title(
                WEZTERM,
                "~/Projects/timesink: ./target/release/timesink-daemon - ./target/release/timesink-daemon"
            ),
            "timesink-daemon"
        );
        // A full path to ssh is still recognised as an ssh session.
        assert_eq!(
            sanitizer().clean_title(WEZTERM, "~: /usr/bin/ssh admin@192.0.2.10 - /usr/bin/ssh"),
            "ssh: proxmox"
        );
    }

    #[test]
    fn commands_are_never_stored() {
        let title = "~: curl -H 'Authorization: Bearer abc123' https://example.com - curl";
        for conf in ["", "terminal_detail = program_dir"] {
            let t = sanitizer_with(conf).clean_title(WEZTERM, title);
            assert!(t.starts_with("curl"), "{t}");
            assert!(!t.contains("abc123"), "{t}");
        }
    }

    #[test]
    fn ssh_uses_label_when_known() {
        let t = sanitizer().clean_title(WEZTERM, "[4/5] ~/Projects: ssh admin@192.0.2.10 - ssh");
        assert_eq!(t, "ssh: proxmox");
    }

    #[test]
    fn ssh_unknown_host_becomes_other_and_hides_the_address() {
        let t = sanitizer().clean_title(WEZTERM, "~: ssh admin@203.0.113.7 - ssh");
        assert_eq!(t, "ssh: other");
    }

    #[test]
    fn named_tui_tab_is_kept_by_program_name() {
        let t = sanitizer().clean_title(WEZTERM, "~: tuxwire - tuxwire");
        assert_eq!(t, "tuxwire");
    }

    #[test]
    fn unparseable_terminal_title_stores_nothing_specific() {
        assert_eq!(sanitizer().clean_title(WEZTERM, "???"), "terminal");
    }

    #[test]
    fn rescrub_shortens_and_blanks_stored_titles() {
        let s = sanitizer();
        // Terminal: directory is dropped, ssh label and fallback stay.
        assert_eq!(s.rescrub_stored(WEZTERM, "fish @ ~/Projects"), "fish");
        assert_eq!(s.rescrub_stored(WEZTERM, "ssh: proxmox"), "ssh: proxmox");
        assert_eq!(s.rescrub_stored(WEZTERM, "terminal"), "terminal");
        // Other apps and browsers are blanked.
        assert_eq!(s.rescrub_stored("discord", "#tcg | Some Server"), "");
        assert_eq!(s.rescrub_stored("zen", "anything"), "");
        // Opt-in apps keep what was stored; program_dir keeps the directory.
        let keep = sanitizer_with("keep_titles = discord");
        assert_eq!(keep.rescrub_stored("discord", "#tcg"), "#tcg");
        let dir = sanitizer_with("terminal_detail = program_dir");
        assert_eq!(
            dir.rescrub_stored(WEZTERM, "fish @ ~/Projects"),
            "fish @ ~/Projects"
        );
        let app = sanitizer_with("terminal_detail = app");
        assert_eq!(app.rescrub_stored(WEZTERM, "fish @ ~/Projects"), "");
    }

    #[test]
    fn hosts_conf_ignores_comments_and_bad_lines() {
        let s = Sanitizer::from_hosts_conf("# c\nnoequals\n = nolabel\nhost =\nok = yes\n");
        assert_eq!(s.host_labels.len(), 1);
        assert_eq!(s.host_labels.get("ok").map(String::as_str), Some("yes"));
    }
}
