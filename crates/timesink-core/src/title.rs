//! Parsing of terminal window titles.
//!
//! Wezterm (as seen through Hyprland) titles look like this:
//!
//! ```text
//! [4/5] ~/Projects: ssh user@host - ssh
//!  ^^^^  ^^^^^^^^^^  ^^^^^^^^^^^^^   ^^^
//!  tab   directory   command         process
//! ```
//!
//! The pieces are optional. An idle shell prompt looks like `~/Projects - fish`
//! (no command), and the tab prefix `[4/5]` can be missing.

/// The pieces of a terminal title. Every field is optional because titles vary.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParsedTitle {
    /// `(tab number, tab count)` from the `[n/m]` prefix.
    pub tab: Option<(u32, u32)>,
    /// Working directory shown before the colon.
    pub cwd: Option<String>,
    /// The full command line currently running. May contain secrets, so the
    /// sanitizer never stores it as-is.
    pub command: Option<String>,
    /// The program name after the final ` - `, e.g. `fish` or `ssh`.
    pub process: Option<String>,
}

/// Split the optional `[n/m] ` prefix off the front of a title.
///
/// Returns the tab info (if present and valid) and the rest of the title.
pub fn split_tab_prefix(raw: &str) -> (Option<(u32, u32)>, &str) {
    // The prefix must start with '[' and contain a closing ']'.
    let Some(rest) = raw.strip_prefix('[') else {
        return (None, raw);
    };
    let Some(close) = rest.find(']') else {
        return (None, raw);
    };
    // Inside the brackets we expect "number/number".
    let inside = &rest[..close];
    let Some((a, b)) = inside.split_once('/') else {
        return (None, raw);
    };
    match (a.parse::<u32>(), b.parse::<u32>()) {
        // Valid prefix: drop it plus the single space that follows.
        (Ok(n), Ok(m)) => (Some((n, m)), rest[close + 1..].trim_start()),
        // Looks like brackets but not a tab prefix (e.g. "[draft] notes"), so keep it.
        _ => (None, raw),
    }
}

/// Parse a terminal window title into its pieces.
pub fn parse_terminal_title(raw: &str) -> ParsedTitle {
    let (tab, rest) = split_tab_prefix(raw);

    // The program name is whatever follows the LAST " - ". If there is no
    // " - " at all we cannot tell what the title means, so we return only the tab.
    let Some((body, process)) = rest.rsplit_once(" - ") else {
        return ParsedTitle {
            tab,
            ..ParsedTitle::default()
        };
    };

    // The body is either "directory: command" or just "directory".
    // Split at the FIRST ": " so commands that contain colons stay intact.
    let (cwd, command) = match body.split_once(": ") {
        Some((cwd, command)) => (cwd, Some(command.trim().to_string())),
        None => (body, None),
    };

    ParsedTitle {
        tab,
        cwd: non_empty(cwd),
        command: command.filter(|c| !c.is_empty()),
        process: non_empty(process),
    }
}

/// Options of `ssh` that consume the next word as their value, e.g. `-p 2222`.
/// Needed so that the value is not mistaken for the host name.
const SSH_OPTIONS_WITH_VALUE: &[&str] = &[
    "-B", "-b", "-c", "-D", "-E", "-e", "-F", "-I", "-i", "-J", "-L", "-l", "-m", "-O", "-o", "-p",
    "-Q", "-R", "-S", "-W", "-w",
];

/// Extract the host from an `ssh ...` command line, without the user name.
///
/// Returns `None` if the command is not `ssh` or no host can be found.
pub fn ssh_target(command: &str) -> Option<String> {
    let mut words = command.split_whitespace();
    // The command must start with the ssh program.
    if words.next()? != "ssh" {
        return None;
    }
    while let Some(word) = words.next() {
        if SSH_OPTIONS_WITH_VALUE.contains(&word) {
            // Skip this option's value as well.
            words.next();
        } else if word.starts_with('-') {
            // A flag without a value, such as -v or -A.
            continue;
        } else {
            // First plain word is the destination: [user@]host.
            let host = word.rsplit_once('@').map_or(word, |(_, host)| host);
            return non_empty(host);
        }
    }
    None
}

/// Turn an empty string into `None`, otherwise an owned `Some`.
fn non_empty(s: &str) -> Option<String> {
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_prompt_has_cwd_and_process_only() {
        let p = parse_terminal_title("[4/4] ~/Projects - fish");
        assert_eq!(p.tab, Some((4, 4)));
        assert_eq!(p.cwd.as_deref(), Some("~/Projects"));
        assert_eq!(p.command, None);
        assert_eq!(p.process.as_deref(), Some("fish"));
    }

    #[test]
    fn ssh_title_has_command() {
        let p = parse_terminal_title("[4/5] ~/Projects: ssh admin@192.0.2.10 - ssh");
        assert_eq!(p.tab, Some((4, 5)));
        assert_eq!(p.cwd.as_deref(), Some("~/Projects"));
        assert_eq!(p.command.as_deref(), Some("ssh admin@192.0.2.10"));
        assert_eq!(p.process.as_deref(), Some("ssh"));
    }

    #[test]
    fn named_tui_tab() {
        let p = parse_terminal_title("~: tuxwire - tuxwire");
        assert_eq!(p.tab, None);
        assert_eq!(p.cwd.as_deref(), Some("~"));
        assert_eq!(p.command.as_deref(), Some("tuxwire"));
        assert_eq!(p.process.as_deref(), Some("tuxwire"));
    }

    #[test]
    fn command_with_pipes_and_quotes_keeps_last_process() {
        let p = parse_terminal_title(
            "~/Projects: wezterm cli list --format json | jq '.[0]' - wezterm",
        );
        assert_eq!(p.process.as_deref(), Some("wezterm"));
        assert_eq!(p.cwd.as_deref(), Some("~/Projects"));
    }

    #[test]
    fn title_without_separator_is_unparsed() {
        let p = parse_terminal_title("something odd");
        assert_eq!(p.cwd, None);
        assert_eq!(p.process, None);
    }

    #[test]
    fn brackets_that_are_not_a_tab_prefix_are_kept() {
        let (tab, rest) = split_tab_prefix("[draft] notes - vim");
        assert_eq!(tab, None);
        assert_eq!(rest, "[draft] notes - vim");
    }

    #[test]
    fn ssh_target_variants() {
        assert_eq!(
            ssh_target("ssh host.example").as_deref(),
            Some("host.example")
        );
        assert_eq!(
            ssh_target("ssh root@192.0.2.10").as_deref(),
            Some("192.0.2.10")
        );
        assert_eq!(
            ssh_target("ssh -p 2222 -i key.pem admin@srv.example").as_deref(),
            Some("srv.example")
        );
        assert_eq!(
            ssh_target("ssh -v -J jump.example admin@inner.example").as_deref(),
            Some("inner.example")
        );
        assert_eq!(
            ssh_target("ssh -o StrictHostKeyChecking=no box").as_deref(),
            Some("box")
        );
    }

    #[test]
    fn ssh_target_rejects_other_commands() {
        assert_eq!(ssh_target("scp file host:/tmp"), None);
        assert_eq!(ssh_target("ssh"), None);
        assert_eq!(ssh_target("ssh -v"), None);
    }
}
