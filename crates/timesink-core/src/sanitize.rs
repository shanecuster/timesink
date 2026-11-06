//! Privacy rules: decide what text from a window title is allowed into the database.
//!
//! Defaults are deliberately conservative:
//!   - Browsers: the title is dropped (a Zen extension will report domains later).
//!   - Terminals: the full command line is NEVER stored, because commands can
//!     contain passwords or tokens. Only the program name and directory are kept.
//!   - SSH sessions: stored as `ssh: <label>`, where the label comes from a local
//!     hosts file. Unknown hosts become `ssh: other`, so IP addresses and user
//!     names never reach the database.
//!   - Everything else: the title is stored as-is.

use std::collections::HashMap;

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
}

impl Sanitizer {
    /// Build a sanitizer from an explicit host-to-label map.
    pub fn new(host_labels: HashMap<String, String>) -> Self {
        Self { host_labels }
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
        Self { host_labels }
    }

    /// Produce the title that is safe to store for a window.
    pub fn clean_title(&self, app_class: &str, raw_title: &str) -> String {
        match classify(app_class) {
            // Browsers: record only the app, never the page title.
            AppKind::Browser => String::new(),
            AppKind::Terminal => self.clean_terminal_title(raw_title),
            // Other apps: keep the title, minus a terminal-style tab prefix if any.
            AppKind::Other => split_tab_prefix(raw_title).1.to_string(),
        }
    }

    /// Reduce a terminal title to `ssh: <label>` or `<program> @ <directory>`.
    fn clean_terminal_title(&self, raw_title: &str) -> String {
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

        // Anything else: program name and directory only, never the command line.
        match (process, parsed.cwd) {
            (Some(process), Some(cwd)) => format!("{process} @ {cwd}"),
            (Some(process), None) => process.to_string(),
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

    #[test]
    fn program_paths_are_reduced_to_the_file_name() {
        let s = sanitizer();
        assert_eq!(
            s.clean_title(
                WEZTERM,
                "~/Projects/timesink: ./target/release/timesink-daemon - ./target/release/timesink-daemon"
            ),
            "timesink-daemon @ ~/Projects/timesink"
        );
        // A full path to ssh is still recognised as an ssh session.
        assert_eq!(
            s.clean_title(WEZTERM, "~: /usr/bin/ssh admin@192.0.2.10 - /usr/bin/ssh"),
            "ssh: proxmox"
        );
    }

    /// A sanitizer that knows one example host (TEST-NET address, not a real machine).
    fn sanitizer() -> Sanitizer {
        Sanitizer::from_hosts_conf("# my servers\n192.0.2.10 = proxmox\n\nbox=lab\n")
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
    fn shell_prompt_keeps_program_and_directory() {
        let t = sanitizer().clean_title(WEZTERM, "[4/4] ~/Projects - fish");
        assert_eq!(t, "fish @ ~/Projects");
    }

    #[test]
    fn commands_are_never_stored() {
        let t = sanitizer().clean_title(
            WEZTERM,
            "~: curl -H 'Authorization: Bearer abc123' https://example.com - curl",
        );
        assert_eq!(t, "curl @ ~");
        assert!(!t.contains("abc123"));
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
    fn named_tui_tab_is_kept() {
        let t = sanitizer().clean_title(WEZTERM, "~: tuxwire - tuxwire");
        assert_eq!(t, "tuxwire @ ~");
    }

    #[test]
    fn unparseable_terminal_title_stores_nothing_specific() {
        assert_eq!(sanitizer().clean_title(WEZTERM, "???"), "terminal");
    }

    #[test]
    fn other_apps_keep_their_title() {
        assert_eq!(sanitizer().clean_title("steam", "Library"), "Library");
    }

    #[test]
    fn hosts_conf_ignores_comments_and_bad_lines() {
        let s = Sanitizer::from_hosts_conf("# c\nnoequals\n = nolabel\nhost =\nok = yes\n");
        assert_eq!(s.host_labels.len(), 1);
        assert_eq!(s.host_labels.get("ok").map(String::as_str), Some("yes"));
    }
}
