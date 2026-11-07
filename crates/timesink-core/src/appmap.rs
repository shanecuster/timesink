//! Friendly names and categories for applications.
//!
//! The database always stores the raw window class that the desktop reports
//! (for example `org.wezfurlong.wezterm`). This module turns that into a short
//! display name ("wezterm") and a category ("Terminal") only when something is
//! shown. Because nothing is rewritten in the database, you can change the
//! mapping at any time and all of your history is re-sorted automatically.
//!
//! File format (`apps.conf`), one app per line:
//!
//! ```text
//! # raw class                display   category
//! org.wezfurlong.wezterm     wezterm   Terminal
//! io.github.lgse.Strata*     strata    Files
//! ```
//!
//! * Fields are separated by spaces or tabs; the category is everything after
//!   the display name, so it may contain spaces.
//! * Lines starting with `#` and blank lines are ignored.
//! * A raw class ending in `*` matches every class that starts with the text
//!   before the `*`. An exact entry always beats a `*` entry.
//! * Matching ignores upper/lower case, because desktops are not consistent
//!   about it (`Hexchat` versus `hexchat`).
//! * If the same class appears twice, the later line wins.

use std::collections::HashMap;

/// The category given to any application that is not listed.
pub const OTHER: &str = "Other";

/// How one application should be shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppInfo {
    /// Short name to display.
    pub display: String,
    /// Category the app belongs to.
    pub category: String,
    /// False when the app was not listed and the defaults were used.
    pub mapped: bool,
}

/// One parsed line of `apps.conf`.
#[derive(Debug, Clone)]
struct Entry {
    display: String,
    category: String,
}

/// The parsed contents of `apps.conf`.
#[derive(Debug, Clone, Default)]
pub struct AppMap {
    /// Exact entries, keyed by lower-cased raw class.
    exact: HashMap<String, Entry>,
    /// Prefix entries (`something*`), as (lower-cased prefix, entry).
    /// Kept in file order; the longest matching prefix wins.
    prefixes: Vec<(String, Entry)>,
    /// Categories in the order they first appear in the file ("Other" excluded).
    category_order: Vec<String>,
}

impl AppMap {
    /// Parse the text of an `apps.conf` file. Lines that cannot be understood
    /// (fewer than three fields) are skipped rather than treated as errors, so
    /// one typo never stops the daemon or the report.
    pub fn from_conf(text: &str) -> Self {
        let mut map = AppMap::default();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut parts = line.split_whitespace();
            let (Some(raw), Some(display)) = (parts.next(), parts.next()) else {
                continue;
            };
            // Everything left over is the category (it may have spaces).
            let category = parts.collect::<Vec<_>>().join(" ");
            if category.is_empty() {
                continue;
            }
            if category != OTHER && !map.category_order.contains(&category) {
                map.category_order.push(category.clone());
            }
            let entry = Entry {
                display: display.to_string(),
                category,
            };
            let raw = raw.to_lowercase();
            match raw.strip_suffix('*') {
                Some(prefix) => {
                    // A repeated prefix replaces the earlier one.
                    map.prefixes.retain(|(p, _)| p != prefix);
                    map.prefixes.push((prefix.to_string(), entry));
                }
                None => {
                    map.exact.insert(raw, entry);
                }
            }
        }
        map
    }

    /// Look up how an application should be shown.
    pub fn lookup(&self, raw_class: &str) -> AppInfo {
        let key = raw_class.to_lowercase();
        if let Some(entry) = self.exact.get(&key) {
            return Self::info(entry);
        }
        // Longest matching prefix wins, so `io.github.lgse.Strata.Special*`
        // can override a general `io.github.lgse.Strata*`.
        let best = self
            .prefixes
            .iter()
            .filter(|(prefix, _)| key.starts_with(prefix.as_str()))
            .max_by_key(|(prefix, _)| prefix.len());
        match best {
            Some((_, entry)) => Self::info(entry),
            None => AppInfo {
                display: raw_class.to_string(),
                category: OTHER.to_string(),
                mapped: false,
            },
        }
    }

    /// Every category named in the config, in the order each first appears in
    /// the file. "Other" is not included; it exists only for apps that are not
    /// listed. A category whose lines were all replaced by later lines is left out.
    ///
    /// Used to give each category a fixed colour that does not change between
    /// screens. Because the order is the file's order, you choose which
    /// categories get the most distinct colours by putting them first.
    pub fn categories(&self) -> Vec<String> {
        let in_use = |name: &String| {
            self.exact.values().any(|e| &e.category == name)
                || self.prefixes.iter().any(|(_, e)| &e.category == name)
        };
        self.category_order
            .iter()
            .filter(|name| in_use(name))
            .cloned()
            .collect()
    }

    /// Turn a parsed entry into the public answer.
    fn info(entry: &Entry) -> AppInfo {
        AppInfo {
            display: entry.display.clone(),
            category: entry.category.clone(),
            mapped: true,
        }
    }
}

/// Time spent in one application after names have been applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppTotal {
    /// Short display name.
    pub display: String,
    /// Category of the app.
    pub category: String,
    /// Seconds spent.
    pub secs: i64,
}

impl AppMap {
    /// Combine per-raw-class totals into per-display-name totals and
    /// per-category totals, both sorted from most to least time.
    ///
    /// Several raw classes can share one display name (for example an app and
    /// its file chooser); they are added together. If two classes share a
    /// display name but have different categories, the first one seen keeps it.
    pub fn group(&self, totals: &[(String, i64)]) -> (Vec<AppTotal>, Vec<(String, i64)>) {
        let mut apps: Vec<AppTotal> = Vec::new();
        let mut categories: Vec<(String, i64)> = Vec::new();
        for (raw, secs) in totals {
            let info = self.lookup(raw);
            match apps.iter_mut().find(|a| a.display == info.display) {
                Some(app) => app.secs += secs,
                None => apps.push(AppTotal {
                    display: info.display,
                    category: info.category.clone(),
                    secs: *secs,
                }),
            }
            match categories.iter_mut().find(|(c, _)| *c == info.category) {
                Some((_, total)) => *total += secs,
                None => categories.push((info.category, *secs)),
            }
        }
        // Biggest first; ties broken by name so the order is stable.
        apps.sort_by(|a, b| b.secs.cmp(&a.secs).then(a.display.cmp(&b.display)));
        categories.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        (apps, categories)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small map shaped like a real config.
    fn sample() -> AppMap {
        AppMap::from_conf(
            "# comment\n\
             \n\
             org.wezfurlong.wezterm   wezterm  Terminal\n\
             zen                      zen      Browser\n\
             hexchat                  hexchat  Chat\n\
             io.github.lgse.Strata*   strata   Files\n\
             some.app                 thing    Deep Work\n",
        )
    }

    #[test]
    fn exact_match_gives_display_name_and_category() {
        let info = sample().lookup("org.wezfurlong.wezterm");
        assert_eq!(info.display, "wezterm");
        assert_eq!(info.category, "Terminal");
        assert!(info.mapped);
    }

    #[test]
    fn matching_ignores_case() {
        let info = sample().lookup("Hexchat");
        assert_eq!(info.display, "hexchat");
        assert_eq!(info.category, "Chat");
    }

    #[test]
    fn star_matches_every_class_with_that_start() {
        let map = sample();
        assert_eq!(map.lookup("io.github.lgse.Strata").category, "Files");
        assert_eq!(
            map.lookup("io.github.lgse.Strata.FileChooser").display,
            "strata"
        );
    }

    #[test]
    fn exact_beats_star_and_longer_star_beats_shorter() {
        let map = AppMap::from_conf(
            "io.github.lgse.Strata*              strata   Files\n\
             io.github.lgse.Strata.FileChooser*  picker   Dialogs\n\
             io.github.lgse.Strata.Special       special  Work\n",
        );
        assert_eq!(map.lookup("io.github.lgse.Strata").display, "strata");
        assert_eq!(
            map.lookup("io.github.lgse.Strata.FileChooser").display,
            "picker"
        );
        assert_eq!(
            map.lookup("io.github.lgse.Strata.Special").display,
            "special"
        );
    }

    #[test]
    fn category_may_contain_spaces() {
        assert_eq!(sample().lookup("some.app").category, "Deep Work");
    }

    #[test]
    fn unknown_app_keeps_raw_name_under_other() {
        let info = sample().lookup("brand.new.App");
        assert_eq!(info.display, "brand.new.App");
        assert_eq!(info.category, OTHER);
        assert!(!info.mapped);
    }

    #[test]
    fn later_line_wins_and_bad_lines_are_skipped() {
        let map = AppMap::from_conf(
            "zen zen Browser\n\
             onlyonefield\n\
             two fields\n\
             zen zen Research\n",
        );
        assert_eq!(map.lookup("zen").category, "Research");
        assert!(!map.lookup("onlyonefield").mapped);
        assert!(!map.lookup("two").mapped);
    }

    #[test]
    fn empty_config_maps_nothing() {
        let info = AppMap::default().lookup("zen");
        assert!(!info.mapped);
        assert_eq!(info.display, "zen");
    }

    #[test]
    fn group_merges_shared_names_and_sorts() {
        let map = sample();
        let totals = vec![
            ("zen".to_string(), 100),
            ("io.github.lgse.Strata".to_string(), 30),
            ("io.github.lgse.Strata.FileChooser".to_string(), 5),
            ("org.wezfurlong.wezterm".to_string(), 50),
            ("mystery".to_string(), 7),
        ];
        let (apps, cats) = map.group(&totals);
        assert_eq!(apps[0].display, "zen");
        let strata = apps.iter().find(|a| a.display == "strata").unwrap();
        assert_eq!(strata.secs, 35);
        assert_eq!(
            cats,
            vec![
                ("Browser".to_string(), 100),
                ("Terminal".to_string(), 50),
                ("Files".to_string(), 35),
                ("Other".to_string(), 7),
            ]
        );
    }

    #[test]
    fn categories_follow_file_order_without_duplicates() {
        assert_eq!(
            sample().categories(),
            vec!["Terminal", "Browser", "Chat", "Files", "Deep Work"]
        );
        assert!(AppMap::default().categories().is_empty());
    }

    #[test]
    fn a_fully_replaced_category_is_dropped_from_the_list() {
        let map = AppMap::from_conf("zen zen Browser\nzen zen Research\nvim vim Editors\n");
        assert_eq!(map.categories(), vec!["Research", "Editors"]);
    }
}
