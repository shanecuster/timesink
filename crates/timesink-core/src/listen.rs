//! "Listening" time: how long some media player was playing.
//!
//! This is tracked separately from window focus. Playing music does not change
//! which window you are using, so listening time overlaps with focus time and
//! is shown beside the totals, never added to them.
//!
//! Like `Tracker`, `ListenTracker` is a pure state machine: it never reads the
//! clock or touches the database. The caller passes in the time and the list of
//! players currently playing, and applies the returned effects.

use crate::tracker::MAX_GAP_SECS;

/// Prefix every MPRIS player's D-Bus name starts with.
const MPRIS_PREFIX: &str = "org.mpris.MediaPlayer2.";

/// Turn a D-Bus player name into a short, private player id.
///
/// `org.mpris.MediaPlayer2.cider` becomes `cider`. Browsers add a per-process
/// suffix (`firefox.instance12345`, or `firefox.instance_1_82` as Zen does), which is
/// removed so every run of the browser counts as one player. Only the player's name
/// is kept; track titles and artists are never read.
pub fn player_id(bus_name: &str) -> String {
    let name = bus_name.strip_prefix(MPRIS_PREFIX).unwrap_or(bus_name);
    // Drop an ".instance" suffix made of digits and underscores ("12345", "_1_82"),
    // if there is one. The part after "instance" must hold at least one digit, so a
    // name that merely contains "instance" is left alone.
    let name = match name.rsplit_once(".instance") {
        Some((base, rest))
            if !base.is_empty()
                && rest.chars().any(|c| c.is_ascii_digit())
                && rest.chars().all(|c| c.is_ascii_digit() || c == '_') =>
        {
            base
        }
        _ => name,
    };
    name.to_lowercase()
}

/// A change the caller must make to the stored listening data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListenEffect {
    /// Listening began. Store a new row and remember its id.
    Opened { start_ts: i64, player: String },
    /// The open row is still going; move its end time forward.
    Extended { end_ts: i64 },
    /// The open row is finished at exactly this time.
    Closed { end_ts: i64 },
}

/// Turns "who is playing right now" samples into listening rows.
#[derive(Debug, Default)]
pub struct ListenTracker {
    /// The player being timed right now, if any.
    open: Option<String>,
    /// Time of the most recent sample, used to detect sleep gaps.
    last_seen: i64,
    /// True while the machine is asleep: nothing may be opened.
    suspended: bool,
}

impl ListenTracker {
    /// Create a tracker with nothing open.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one sample: the time and the ids of all players that are playing.
    ///
    /// Call it whenever the set of players changes AND on the regular
    /// heartbeat so the open row keeps growing. If several players are playing
    /// at once only one is recorded, so the same minute is never counted twice.
    pub fn observe(&mut self, now: i64, playing: &[String]) -> Vec<ListenEffect> {
        if self.suspended {
            return Vec::new();
        }
        let mut effects = Vec::new();

        // A long silence between samples means the machine slept or the daemon
        // was frozen. Do not count the gap: end the row where we last knew.
        if self.open.is_some() && now - self.last_seen > MAX_GAP_SECS {
            effects.push(ListenEffect::Closed {
                end_ts: self.last_seen,
            });
            self.open = None;
        }
        self.last_seen = now;

        // Keep the current player while it is still playing; otherwise pick the
        // alphabetically first one so the choice is stable.
        let wanted = match &self.open {
            Some(current) if playing.contains(current) => Some(current.clone()),
            _ => playing.iter().min().cloned(),
        };

        match (&self.open, wanted) {
            (Some(_), Some(w)) if self.open.as_deref() == Some(w.as_str()) => {
                effects.push(ListenEffect::Extended { end_ts: now });
            }
            (Some(_), Some(w)) => {
                effects.push(ListenEffect::Closed { end_ts: now });
                effects.push(ListenEffect::Opened {
                    start_ts: now,
                    player: w.clone(),
                });
                self.open = Some(w);
            }
            (Some(_), None) => {
                effects.push(ListenEffect::Closed { end_ts: now });
                self.open = None;
            }
            (None, Some(w)) => {
                effects.push(ListenEffect::Opened {
                    start_ts: now,
                    player: w.clone(),
                });
                self.open = Some(w);
            }
            (None, None) => {}
        }
        effects
    }

    /// The machine is going to sleep at `at`: end any open row there and ignore
    /// samples until `resume` is called.
    pub fn suspend(&mut self, at: i64) -> Vec<ListenEffect> {
        self.suspended = true;
        match self.open.take() {
            Some(_) => vec![ListenEffect::Closed { end_ts: at }],
            None => Vec::new(),
        }
    }

    /// The machine woke up; samples count again.
    pub fn resume(&mut self) {
        self.suspended = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a player list from names.
    fn p(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn player_ids_are_short_and_private() {
        assert_eq!(player_id("org.mpris.MediaPlayer2.cider"), "cider");
        assert_eq!(
            player_id("org.mpris.MediaPlayer2.firefox.instance12345"),
            "firefox"
        );
        assert_eq!(player_id("org.mpris.MediaPlayer2.Spotify"), "spotify");
        // Zen's real bus name, with underscores in the suffix.
        assert_eq!(
            player_id("org.mpris.MediaPlayer2.firefox.instance_1_82"),
            "firefox"
        );
        // A suffix with no digits, or with letters, is not a process number.
        assert_eq!(
            player_id("org.mpris.MediaPlayer2.firefox.instance_"),
            "firefox.instance_"
        );
        assert_eq!(
            player_id("org.mpris.MediaPlayer2.firefox.instance_abc1"),
            "firefox.instance_abc1"
        );
        // A name that merely contains "instance" is left alone.
        assert_eq!(player_id("org.mpris.MediaPlayer2.instancey"), "instancey");
    }

    #[test]
    fn opens_extends_and_closes() {
        let mut t = ListenTracker::new();
        assert_eq!(
            t.observe(100, &p(&["cider"])),
            vec![ListenEffect::Opened {
                start_ts: 100,
                player: "cider".into()
            }]
        );
        assert_eq!(
            t.observe(130, &p(&["cider"])),
            vec![ListenEffect::Extended { end_ts: 130 }]
        );
        assert_eq!(
            t.observe(150, &[]),
            vec![ListenEffect::Closed { end_ts: 150 }]
        );
        assert!(t.observe(160, &[]).is_empty());
    }

    #[test]
    fn two_players_count_once() {
        let mut t = ListenTracker::new();
        t.observe(100, &p(&["mpv"]));
        // Cider starts too; mpv keeps the row so nothing is counted twice.
        assert_eq!(
            t.observe(110, &p(&["cider", "mpv"])),
            vec![ListenEffect::Extended { end_ts: 110 }]
        );
        // mpv stops; the row switches to cider.
        assert_eq!(
            t.observe(120, &p(&["cider"])),
            vec![
                ListenEffect::Closed { end_ts: 120 },
                ListenEffect::Opened {
                    start_ts: 120,
                    player: "cider".into()
                }
            ]
        );
    }

    #[test]
    fn long_gap_is_not_counted() {
        let mut t = ListenTracker::new();
        t.observe(100, &p(&["cider"]));
        t.observe(130, &p(&["cider"]));
        // Nothing for an hour (suspend), then a sample: close at 130, reopen now.
        assert_eq!(
            t.observe(3730, &p(&["cider"])),
            vec![
                ListenEffect::Closed { end_ts: 130 },
                ListenEffect::Opened {
                    start_ts: 3730,
                    player: "cider".into()
                }
            ]
        );
    }

    #[test]
    fn suspend_closes_and_blocks_until_resume() {
        let mut t = ListenTracker::new();
        t.observe(100, &p(&["cider"]));
        assert_eq!(t.suspend(140), vec![ListenEffect::Closed { end_ts: 140 }]);
        assert!(t.observe(150, &p(&["cider"])).is_empty());
        t.resume();
        assert_eq!(
            t.observe(5000, &p(&["cider"])),
            vec![ListenEffect::Opened {
                start_ts: 5000,
                player: "cider".into()
            }]
        );
    }
}
