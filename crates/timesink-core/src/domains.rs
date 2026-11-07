//! Website (domain) tracking for the optional Zen/Firefox extension.
//!
//! Privacy first: the extension only ever reports a *host name*; this file reduces it to the
//! registered site ("gist.github.com" -> "github.com") and nothing else about the page (path,
//! query, title) ever reaches timesink. Sites you list under `block` are never stored.
//!
//! How the time is worked out: the extension says "the active tab's site is X" (or "none")
//! roughly every 30 seconds; that is stored as stretches in the `domains` table. A site's time
//! is the overlap of those stretches with the times Zen was the focused, non-idle window
//! (the ordinary `intervals` data), so idle time, a locked screen, another app in front, or
//! many background tabs never count.
//!
//! Everything here is pure (no files, no database) so it is easy to test.

use crate::appmap::AppMap;
use crate::stats::Span;

/// A stored site stretch is extended by a heartbeat only if the previous report was at most
/// this many seconds ago; otherwise a new stretch starts. The extension reports every 30s, so
/// 90s tolerates a couple of missed beats (sleep, a busy browser) without inventing time.
pub const DOMAIN_GAP_SECS: i64 = 90;

/// The browser window classes whose focus time site time is measured against, if the config
/// does not say otherwise.
pub const DEFAULT_BROWSERS: &[&str] = &["zen", "firefox"];

/// Second-level suffixes that take three labels to make a registered site
/// (`bbc.co.uk`, not `co.uk`). This is a short built-in list, not the full public-suffix
/// list; an unlisted multi-part suffix is shown as e.g. `co.xx` (harmless, just less tidy).
const TWO_PART_SUFFIXES: &[&str] = &[
    "co.uk", "org.uk", "ac.uk", "gov.uk", "me.uk", "co.jp", "ne.jp", "or.jp", "co.kr", "co.nz",
    "org.nz", "co.za", "co.in", "com.au", "net.au", "org.au", "edu.au", "gov.au", "com.br",
    "com.mx", "com.ar", "com.tr", "com.cn", "com.hk", "com.sg", "com.tw", "co.il", "co.id",
];

/// Reduce a host name to its registered site, or `None` if it should not be tracked at all
/// (empty, `localhost`, an IP address, a name with no dot, or something that is not a host).
///
/// ```text
/// gist.github.com   -> github.com
/// www.bbc.co.uk     -> bbc.co.uk
/// 192.168.1.5       -> None
/// ```
pub fn registered_domain(host: &str) -> Option<String> {
    let host = host.trim().trim_end_matches('.').to_lowercase();
    // A host is letters, digits, dashes and dots only; anything else (spaces, slashes, a
    // colon from an IPv6 literal) is rejected rather than guessed at.
    if host.is_empty()
        || !host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
    {
        return None;
    }
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() < 2 || labels.iter().any(|l| l.is_empty()) {
        return None;
    }
    // IPv4 addresses are all-numeric labels.
    if labels.iter().all(|l| l.chars().all(|c| c.is_ascii_digit())) {
        return None;
    }
    let n = labels.len();
    let last_two = format!("{}.{}", labels[n - 2], labels[n - 1]);
    let take = if n >= 3 && TWO_PART_SUFFIXES.contains(&last_two.as_str()) {
        3
    } else {
        2
    };
    Some(labels[n - take..].join("."))
}

/// The user's `domains.conf`: which browser to measure against, which sites to never record,
/// and friendly names and categories for sites.
///
/// ```text
/// browser = zen
/// block = bank.example
/// github.com   github   Research
/// ```
///
/// Site lines have the same `site display category` layout as `apps.conf`.
#[derive(Debug, Clone)]
pub struct DomainConfig {
    /// Window classes (lower-case) that count as "the browser".
    pub browsers: Vec<String>,
    /// Registered sites that are never recorded.
    pub blocked: Vec<String>,
    /// Friendly names and categories for sites.
    pub sites: AppMap,
}

impl Default for DomainConfig {
    fn default() -> Self {
        DomainConfig {
            browsers: DEFAULT_BROWSERS.iter().map(|b| b.to_string()).collect(),
            blocked: Vec::new(),
            sites: AppMap::default(),
        }
    }
}

impl DomainConfig {
    /// Parse the text of a `domains.conf`. Bad lines are skipped, never fatal.
    pub fn from_conf(text: &str) -> Self {
        let mut config = DomainConfig::default();
        let mut browsers: Vec<String> = Vec::new();
        // Lines that are not `key = value` settings are site mappings.
        let mut site_lines = String::new();
        for line in text.lines() {
            let trimmed = line.trim();
            match trimmed.split_once('=') {
                Some((key, value)) if !trimmed.starts_with('#') => {
                    let value = value.trim().to_lowercase();
                    match key.trim() {
                        "browser" if !value.is_empty() => {
                            browsers.extend(
                                value
                                    .split(',')
                                    .map(|b| b.trim().to_string())
                                    .filter(|b| !b.is_empty()),
                            );
                        }
                        "block" => {
                            // Accept a full host name and reduce it, like the extension's data.
                            if let Some(site) = registered_domain(&value) {
                                config.blocked.push(site);
                            }
                        }
                        _ => {}
                    }
                }
                _ => {
                    site_lines.push_str(line);
                    site_lines.push('\n');
                }
            }
        }
        if !browsers.is_empty() {
            config.browsers = browsers;
        }
        config.sites = AppMap::from_conf(&site_lines);
        config
    }

    /// Is this (already reduced) site on the block list?
    pub fn is_blocked(&self, site: &str) -> bool {
        self.blocked.iter().any(|b| b == site)
    }

    /// Is this window class one of the configured browsers?
    pub fn is_browser(&self, app_class: &str) -> bool {
        let class = app_class.to_lowercase();
        self.browsers.contains(&class)
    }
}

/// The time each site was really in front of you: the overlap of the site stretches with the
/// browser's focus stretches. Both lists must be sorted by start time; the result is too.
/// `domain_spans` carry the site in `key`; `focus_spans` can carry anything.
pub fn intersect(domain_spans: &[Span], focus_spans: &[Span]) -> Vec<Span> {
    let mut out = Vec::new();
    let mut f = 0;
    for d in domain_spans {
        // Skip focus stretches that ended before this site stretch began (both lists are
        // sorted, so they cannot matter to any later site stretch either).
        while f < focus_spans.len() && focus_spans[f].end <= d.start {
            f += 1;
        }
        let mut g = f;
        while g < focus_spans.len() && focus_spans[g].start < d.end {
            let start = d.start.max(focus_spans[g].start);
            let end = d.end.min(focus_spans[g].end);
            if end > start {
                out.push(Span {
                    start,
                    end,
                    key: d.key.clone(),
                });
            }
            g += 1;
        }
    }
    out
}

/// Add up spans per key, biggest first (ties by name).
pub fn totals(spans: &[Span]) -> Vec<(String, i64)> {
    let mut map: Vec<(String, i64)> = Vec::new();
    for s in spans {
        match map.iter_mut().find(|(k, _)| *k == s.key) {
            Some((_, t)) => *t += s.end - s.start,
            None => map.push((s.key.clone(), s.end - s.start)),
        }
    }
    map.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(start: i64, end: i64, key: &str) -> Span {
        Span {
            start,
            end,
            key: key.into(),
        }
    }

    #[test]
    fn subdomains_collapse_to_the_main_site() {
        assert_eq!(
            registered_domain("gist.github.com").as_deref(),
            Some("github.com")
        );
        assert_eq!(
            registered_domain("WWW.GitHub.com.").as_deref(),
            Some("github.com")
        );
        assert_eq!(
            registered_domain("github.com").as_deref(),
            Some("github.com")
        );
        assert_eq!(
            registered_domain("www.bbc.co.uk").as_deref(),
            Some("bbc.co.uk")
        );
        assert_eq!(
            registered_domain("a.b.c.example.org").as_deref(),
            Some("example.org")
        );
    }

    #[test]
    fn things_that_are_not_sites_are_dropped() {
        for bad in [
            "",
            "localhost",
            "192.168.1.5",
            "10.0.0.1",
            "[::1]",
            "a b.com",
            "foo/bar.com",
            "..",
            "a..b",
            ".com",
            "com",
        ] {
            assert_eq!(registered_domain(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn config_reads_settings_and_site_lines() {
        let c = DomainConfig::from_conf(
            "# my sites\nbrowser = zen, Zen-Browser\nblock = Login.Bank.example\n\
             github.com github Research\nbbc.co.uk bbc News\n",
        );
        assert_eq!(c.browsers, vec!["zen", "zen-browser"]);
        assert!(c.is_blocked("bank.example"));
        assert!(!c.is_blocked("github.com"));
        assert!(c.is_browser("Zen-Browser"));
        let info = c.sites.lookup("github.com");
        assert_eq!(
            (info.display.as_str(), info.category.as_str()),
            ("github", "Research")
        );
        // A setting line must never be read as a site mapping.
        assert!(!c.sites.lookup("browser").mapped);
    }

    #[test]
    fn empty_config_defaults_to_zen() {
        let c = DomainConfig::from_conf("");
        assert!(c.is_browser("zen"));
        assert!(c.blocked.is_empty());
    }

    #[test]
    fn site_time_is_only_what_overlaps_browser_focus() {
        // The site was open 0..100, but the browser was focused only 10..40 and 60..70.
        let sites = vec![span(0, 100, "github.com")];
        let focus = vec![span(10, 40, "zen"), span(60, 70, "zen")];
        let t = totals(&intersect(&sites, &focus));
        assert_eq!(t, vec![("github.com".to_string(), 40)]);
    }

    #[test]
    fn intersect_handles_several_sites_and_no_overlap() {
        let sites = vec![
            span(0, 30, "a.com"),
            span(30, 60, "b.com"),
            span(200, 260, "a.com"),
        ];
        let focus = vec![span(20, 50, "zen"), span(500, 600, "zen")];
        let out = intersect(&sites, &focus);
        assert_eq!(out, vec![span(20, 30, "a.com"), span(30, 50, "b.com")],);
        assert!(intersect(&sites, &[]).is_empty());
        assert!(intersect(&[], &focus).is_empty());
    }

    #[test]
    fn totals_sort_biggest_first() {
        let t = totals(&[
            span(0, 5, "b.com"),
            span(0, 9, "a.com"),
            span(10, 15, "b.com"),
        ]);
        assert_eq!(
            t,
            vec![("a.com".to_string(), 9), ("b.com".to_string(), 10)]
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
        );
    }
}
