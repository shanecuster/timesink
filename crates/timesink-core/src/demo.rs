//! Made-up usage data for the sandbox, so the interface, themes and exports can be tried
//! out (and screenshots taken) without touching the real database.
//!
//! Everything here is pure and deterministic: the same `seed` always gives the same days.
//! The days look like a real working week: weekday work hours with a terminal, browser,
//! notes, chat and email in a believable mix, short idle gaps, some music, and website
//! stretches that always sit inside browser focus time.

/// One made-up focus stretch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DemoInterval {
    /// Start, seconds since the epoch.
    pub start: i64,
    /// End, seconds since the epoch.
    pub end: i64,
    /// Window class, as Hyprland would report it.
    pub class: String,
    /// The cleaned detail the daemon would store (program name, `ssh: label`, or empty).
    pub detail: String,
}

/// Everything the generator makes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DemoData {
    /// Focus stretches, oldest first, never overlapping.
    pub intervals: Vec<DemoInterval>,
    /// Listening stretches: (player, start, end).
    pub listening: Vec<(String, i64, i64)>,
    /// Website stretches: (site, start, end), each inside a browser focus stretch.
    pub sites: Vec<(String, i64, i64)>,
}

/// A tiny repeatable random number generator (a 64-bit linear congruential generator).
/// Good enough for variety; not for anything security related.
struct Rng(u64);

impl Rng {
    /// The next number in `lo..hi` (hi excluded).
    fn range(&mut self, lo: i64, hi: i64) -> i64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let span = (hi - lo).max(1) as u64;
        lo + ((self.0 >> 33) % span) as i64
    }

    /// Pick an item by weight.
    fn pick<'a, T>(&mut self, items: &'a [(T, i64)]) -> &'a T {
        let total: i64 = items.iter().map(|(_, w)| w).sum();
        let mut roll = self.range(0, total);
        for (item, weight) in items {
            if roll < *weight {
                return item;
            }
            roll -= weight;
        }
        &items[0].0
    }
}

/// Applications and what they are used for: (class, detail, weight).
const APPS: &[((&str, &str), i64)] = &[
    (("zen", ""), 34),
    (("org.wezfurlong.wezterm", "fish"), 14),
    (("org.wezfurlong.wezterm", "tuxwire"), 6),
    (("org.wezfurlong.wezterm", "ssh: lab"), 8),
    (("md.obsidian.Obsidian", ""), 12),
    (("discord", ""), 6),
    (("eu.betterbird.Betterbird", ""), 5),
    (("jetbrains-studio", ""), 7),
    (("io.github.lgse.Strata", ""), 3),
    (("bitwarden", ""), 1),
    (("com.gabm.satty", ""), 1),
];

/// Websites and how often they come up: (site, weight).
const SITES: &[(&str, i64)] = &[
    ("github.com", 24),
    ("wikipedia.org", 12),
    ("stackoverflow.com", 14),
    ("claude.ai", 16),
    ("espn.com", 8),
    ("youtube.com", 10),
    ("reddit.com", 6),
    ("shanetalkstech.com", 4),
];

/// Day of the week for a local day number: 0 = Monday ... 6 = Sunday (1970-01-01 was a Thursday).
fn weekday(day: i64) -> i64 {
    (day + 3).rem_euclid(7)
}

/// Make `days` days of data ending at `now` (the last day is partial: nothing after `now`).
/// `tz_offset` is the local offset from UTC in seconds, so working hours land at local times.
pub fn generate(now: i64, days: i64, seed: u64, tz_offset: i64) -> DemoData {
    let mut rng = Rng(seed ^ 0x9e37_79b9_7f4a_7c15);
    let mut data = DemoData::default();
    let today = (now + tz_offset).div_euclid(86400);

    for day in (today - (days.max(1) - 1))..=today {
        let midnight = day * 86400 - tz_offset;
        let weekend = weekday(day) >= 5;
        // Weekdays: roughly 08:00-17:30. Weekends: a lighter late-morning and evening stint.
        let blocks: &[(i64, i64)] = if weekend {
            &[(10 * 3600, 12 * 3600 + 1800), (19 * 3600, 21 * 3600)]
        } else {
            &[(8 * 3600, 12 * 3600), (13 * 3600, 17 * 3600 + 1800)]
        };
        for &(from, to) in blocks {
            let mut t = midnight + from + rng.range(0, 1200);
            let stop = midnight + to;
            while t < stop && t < now {
                // A stretch of one app, 2 to 35 minutes.
                let (class, detail) = *rng.pick(APPS);
                let len = rng.range(2 * 60, 35 * 60);
                let end = (t + len).min(stop).min(now);
                if end - t >= 10 {
                    data.intervals.push(DemoInterval {
                        start: t,
                        end,
                        class: class.to_string(),
                        detail: detail.to_string(),
                    });
                    if class == "zen" {
                        site_stretches(&mut rng, &mut data.sites, t, end);
                    }
                }
                // Back-to-back most of the time; now and then a short break (idle).
                t = end
                    + if rng.range(0, 6) == 0 {
                        rng.range(120, 1500)
                    } else {
                        0
                    };
            }
            // Music through most working mornings and afternoons.
            if !weekend && rng.range(0, 10) < 8 {
                let mut m = midnight + from + rng.range(0, 1800);
                while m < stop && m < now {
                    let len = rng.range(15 * 60, 55 * 60);
                    let end = (m + len).min(stop).min(now);
                    if end - m >= 10 {
                        data.listening.push(("cider".to_string(), m, end));
                    }
                    m = end + rng.range(5 * 60, 25 * 60);
                }
            }
        }
    }
    // The blocks above are generated per block, so sort everything into time order.
    data.intervals.sort_by_key(|i| (i.start, i.end));
    data.listening.sort_by_key(|l| (l.1, l.2));
    data.sites.sort_by_key(|s| (s.1, s.2));
    data
}

/// Split a browser stretch `[start, end)` into site stretches of 1 to 8 minutes.
fn site_stretches(rng: &mut Rng, out: &mut Vec<(String, i64, i64)>, start: i64, end: i64) {
    let mut t = start;
    while t < end {
        let site = rng.pick(SITES);
        let e = (t + rng.range(60, 8 * 60)).min(end);
        if e - t >= 5 {
            out.push((site.to_string(), t, e));
        }
        t = e;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Wednesday afternoon, 15:00 UTC (11:00 at UTC-4).
    const NOW: i64 = 20_000 * 86400 + 15 * 3600;

    #[test]
    fn same_seed_same_data_different_seed_different_data() {
        let a = generate(NOW, 14, 1, -4 * 3600);
        assert_eq!(a, generate(NOW, 14, 1, -4 * 3600));
        assert_ne!(a, generate(NOW, 14, 2, -4 * 3600));
    }

    #[test]
    fn intervals_are_ordered_never_overlap_and_never_reach_the_future() {
        let d = generate(NOW, 35, 7, -4 * 3600);
        assert!(d.intervals.len() > 100);
        for w in d.intervals.windows(2) {
            assert!(w[0].end <= w[1].start, "{:?} overlaps {:?}", w[0], w[1]);
        }
        assert!(d.intervals.iter().all(|i| i.end <= NOW && i.end > i.start));
        assert!(d.listening.iter().all(|l| l.2 <= NOW && l.2 > l.1));
    }

    #[test]
    fn working_days_look_like_working_days() {
        let d = generate(NOW, 14, 3, -4 * 3600);
        // Total focus per local day: every full weekday is between 2 and 10 hours.
        let mut per_day = std::collections::BTreeMap::new();
        for i in &d.intervals {
            *per_day
                .entry((i.start - 4 * 3600).div_euclid(86400))
                .or_insert(0i64) += i.end - i.start;
        }
        let today = (NOW - 4 * 3600).div_euclid(86400);
        for (day, secs) in per_day {
            if day != today && weekday(day) < 5 {
                assert!((2 * 3600..=10 * 3600).contains(&secs), "day {day}: {secs}s");
            }
        }
        // The mix includes the main apps.
        for class in ["zen", "org.wezfurlong.wezterm", "md.obsidian.Obsidian"] {
            assert!(d.intervals.iter().any(|i| i.class == class), "{class}");
        }
        assert!(d.intervals.iter().any(|i| i.detail == "ssh: lab"));
    }

    #[test]
    fn every_site_stretch_sits_inside_a_browser_stretch() {
        let d = generate(NOW, 10, 5, -4 * 3600);
        assert!(!d.sites.is_empty());
        for (site, s, e) in &d.sites {
            assert!(
                d.intervals
                    .iter()
                    .any(|i| i.class == "zen" && i.start <= *s && *e <= i.end),
                "{site} {s}..{e} is outside browser focus"
            );
        }
    }

    #[test]
    fn weekdays_are_labelled_correctly() {
        // 1970-01-01 was a Thursday; 20,000 days later (2024-10-04) was a Friday.
        assert_eq!(weekday(0), 3);
        assert_eq!(weekday(20_000), 4);
    }
}
