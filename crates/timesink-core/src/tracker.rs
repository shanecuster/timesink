//! The tracker turns a stream of "what is focused right now" samples into
//! time intervals.
//!
//! It is a pure state machine: it never reads the clock, the database or
//! Hyprland. The caller passes in the current time, which makes it easy to test.
//! It answers each call with a list of `Effect`s that the caller must apply.

use crate::model::WindowSnapshot;
use crate::presence::PresenceState;

/// If two samples are further apart than this many seconds, we assume the
/// computer was asleep or the daemon was stopped, and we do NOT count the gap.
/// The daemon samples every 30 seconds, so 90 allows for a couple of missed beats.
pub const MAX_GAP_SECS: i64 = 90;

/// A change the caller must make to the stored data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// A new interval began. Store it and remember its id.
    Opened {
        start_ts: i64,
        snapshot: WindowSnapshot,
    },
    /// The currently open interval is still going; move its end time forward.
    Extended { end_ts: i64 },
    /// The currently open interval is finished at exactly this time.
    Closed { end_ts: i64 },
}

/// The interval that is currently being timed.
#[derive(Debug)]
struct OpenInterval {
    snapshot: WindowSnapshot,
    /// When it started, needed to keep an "away" end time inside the interval.
    start_ts: i64,
}

/// Turns samples into interval effects.
#[derive(Debug, Default)]
pub struct Tracker {
    /// The interval being timed right now, if any window is focused.
    open: Option<OpenInterval>,
    /// Time of the most recent sample, used to detect sleep/suspend gaps.
    last_seen: i64,
    /// True while the user is away: no interval may be opened.
    away: bool,
}

impl Tracker {
    /// Create a tracker with nothing open.
    pub fn new() -> Self {
        Self::default()
    }

    /// Is the tracker currently ignoring samples because the user is away?
    pub fn is_away(&self) -> bool {
        self.away
    }

    /// Feed one sample: the time and what is focused (`None` when no window is).
    ///
    /// Call this on every focus-related event AND on a regular heartbeat, so
    /// that the open interval's end time keeps moving forward.
    pub fn observe(&mut self, now: i64, snapshot: Option<WindowSnapshot>) -> Vec<Effect> {
        // While away nothing is counted, whatever the window is doing.
        if self.away {
            self.last_seen = now;
            return Vec::new();
        }

        let mut effects = Vec::new();

        // 1. Gap check: a long silence means the machine slept. Close the open
        //    interval at the last time we knew it was alive, not at "now".
        if self.open.is_some() && now - self.last_seen > MAX_GAP_SECS {
            effects.push(Effect::Closed {
                end_ts: self.last_seen,
            });
            self.open = None;
        }

        // 2. Compare the new sample with the open interval.
        let same_as_open = match (&self.open, &snapshot) {
            (Some(open), Some(new)) => open.snapshot == *new,
            _ => false,
        };

        if same_as_open {
            // Same activity continues: just extend it.
            effects.push(Effect::Extended { end_ts: now });
        } else {
            // Something changed: close what was open, then open the new one.
            if self.open.take().is_some() {
                effects.push(Effect::Closed { end_ts: now });
            }
            if let Some(snapshot) = snapshot {
                effects.push(Effect::Opened {
                    start_ts: now,
                    snapshot: snapshot.clone(),
                });
                self.open = Some(OpenInterval {
                    snapshot,
                    start_ts: now,
                });
            }
        }

        self.last_seen = now;
        effects
    }

    /// Tell the tracker whether the user is present.
    ///
    /// Going `Away{since}` closes the open interval at `since` (the last input),
    /// not at the moment we found out, so idle minutes are never counted.
    /// Going `Active` lets the next `observe` open a fresh interval.
    pub fn set_presence(&mut self, now: i64, state: PresenceState) -> Vec<Effect> {
        match state {
            PresenceState::Away { since } => {
                if self.away {
                    // Already away: keep the earlier end time.
                    return Vec::new();
                }
                self.away = true;
                match self.open.take() {
                    Some(open) => {
                        // Keep the end inside [start, now].
                        let end_ts = since.max(open.start_ts).min(now);
                        vec![Effect::Closed { end_ts }]
                    }
                    None => Vec::new(),
                }
            }
            PresenceState::Active => {
                // Only a real away->active change touches `last_seen`; otherwise
                // we would hide a genuine sleep gap from the gap check.
                if self.away {
                    self.away = false;
                    self.last_seen = now;
                }
                Vec::new()
            }
        }
    }

    /// Close whatever is open (used when Hyprland goes away).
    pub fn finish(&mut self, now: i64) -> Vec<Effect> {
        if self.open.take().is_some() {
            vec![Effect::Closed { end_ts: now }]
        } else {
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a snapshot quickly for tests.
    fn snap(class: &str, title: &str) -> WindowSnapshot {
        WindowSnapshot {
            app_class: class.into(),
            title: title.into(),
            workspace: "1".into(),
        }
    }

    #[test]
    fn first_sample_opens_an_interval() {
        let mut t = Tracker::new();
        let effects = t.observe(100, Some(snap("zen", "")));
        assert_eq!(
            effects,
            vec![Effect::Opened {
                start_ts: 100,
                snapshot: snap("zen", "")
            }]
        );
    }

    #[test]
    fn same_window_extends() {
        let mut t = Tracker::new();
        t.observe(100, Some(snap("zen", "")));
        let effects = t.observe(130, Some(snap("zen", "")));
        assert_eq!(effects, vec![Effect::Extended { end_ts: 130 }]);
    }

    #[test]
    fn focus_change_closes_then_opens() {
        let mut t = Tracker::new();
        t.observe(100, Some(snap("zen", "")));
        let effects = t.observe(160, Some(snap("wezterm", "fish @ ~")));
        assert_eq!(
            effects,
            vec![
                Effect::Closed { end_ts: 160 },
                Effect::Opened {
                    start_ts: 160,
                    snapshot: snap("wezterm", "fish @ ~")
                }
            ]
        );
    }

    #[test]
    fn title_change_in_same_app_starts_a_new_interval() {
        let mut t = Tracker::new();
        t.observe(100, Some(snap("wezterm", "fish @ ~")));
        let effects = t.observe(110, Some(snap("wezterm", "ssh: proxmox")));
        assert_eq!(effects.len(), 2);
        assert_eq!(effects[0], Effect::Closed { end_ts: 110 });
    }

    #[test]
    fn no_focused_window_closes_and_opens_nothing() {
        let mut t = Tracker::new();
        t.observe(100, Some(snap("zen", "")));
        let effects = t.observe(150, None);
        assert_eq!(effects, vec![Effect::Closed { end_ts: 150 }]);
        // Another empty sample does nothing.
        assert!(t.observe(180, None).is_empty());
    }

    #[test]
    fn long_gap_is_not_counted() {
        let mut t = Tracker::new();
        t.observe(100, Some(snap("zen", "")));
        t.observe(130, Some(snap("zen", "")));
        // The laptop slept for an hour, then the same window is still focused.
        let effects = t.observe(3730, Some(snap("zen", "")));
        assert_eq!(
            effects,
            vec![
                // Old interval ends when we last saw it alive...
                Effect::Closed { end_ts: 130 },
                // ...and a fresh one starts at wake-up.
                Effect::Opened {
                    start_ts: 3730,
                    snapshot: snap("zen", "")
                }
            ]
        );
    }

    #[test]
    fn finish_closes_the_open_interval_once() {
        let mut t = Tracker::new();
        t.observe(100, Some(snap("zen", "")));
        assert_eq!(t.finish(120), vec![Effect::Closed { end_ts: 120 }]);
        assert!(t.finish(121).is_empty());
    }

    #[test]
    fn going_away_closes_at_the_last_input_time() {
        let mut t = Tracker::new();
        t.observe(100, Some(snap("zen", "")));
        t.observe(130, Some(snap("zen", "")));
        // Idle was noticed at 500, but the last input was at 300.
        let effects = t.set_presence(500, PresenceState::Away { since: 300 });
        assert_eq!(effects, vec![Effect::Closed { end_ts: 300 }]);
        assert!(t.is_away());
        // While away, samples are ignored (heartbeats must not reopen anything).
        assert!(t.observe(510, Some(snap("zen", ""))).is_empty());
        // Back at 600: nothing happens until the next sample...
        assert!(t.set_presence(600, PresenceState::Active).is_empty());
        assert!(!t.is_away());
        // ...which opens a fresh interval.
        assert_eq!(
            t.observe(600, Some(snap("zen", ""))),
            vec![Effect::Opened {
                start_ts: 600,
                snapshot: snap("zen", "")
            }]
        );
    }

    #[test]
    fn away_time_is_kept_inside_the_interval() {
        // "since" before the interval started: close at the start (zero length).
        let mut t = Tracker::new();
        t.observe(100, Some(snap("zen", "")));
        assert_eq!(
            t.set_presence(120, PresenceState::Away { since: 50 }),
            vec![Effect::Closed { end_ts: 100 }]
        );
        // "since" in the future: close at now.
        let mut t = Tracker::new();
        t.observe(100, Some(snap("zen", "")));
        assert_eq!(
            t.set_presence(200, PresenceState::Away { since: 999 }),
            vec![Effect::Closed { end_ts: 200 }]
        );
    }

    #[test]
    fn going_away_with_nothing_open_is_quiet_and_keeps_the_first_time() {
        let mut t = Tracker::new();
        assert!(t
            .set_presence(100, PresenceState::Away { since: 90 })
            .is_empty());
        assert!(t
            .set_presence(110, PresenceState::Away { since: 50 })
            .is_empty());
        assert!(t.is_away());
    }

    #[test]
    fn staying_active_does_not_hide_a_sleep_gap() {
        let mut t = Tracker::new();
        t.observe(100, Some(snap("zen", "")));
        // Presence is re-checked after an unnoticed hour-long sleep.
        assert!(t.set_presence(3700, PresenceState::Active).is_empty());
        let effects = t.observe(3700, Some(snap("zen", "")));
        assert_eq!(effects[0], Effect::Closed { end_ts: 100 });
    }
}
