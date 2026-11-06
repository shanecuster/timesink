//! The tracker turns a stream of "what is focused right now" samples into
//! time intervals.
//!
//! It is a pure state machine: it never reads the clock, the database or
//! Hyprland. The caller passes in the current time, which makes it easy to test.
//! It answers each sample with a list of `Effect`s that the caller must apply.

use crate::model::WindowSnapshot;

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
    /// The currently open interval is finished at this time.
    Closed { end_ts: i64 },
}

/// The interval that is currently being timed.
#[derive(Debug)]
struct OpenInterval {
    snapshot: WindowSnapshot,
}

/// Turns samples into interval effects.
#[derive(Debug, Default)]
pub struct Tracker {
    /// The interval being timed right now, if any window is focused.
    open: Option<OpenInterval>,
    /// Time of the most recent sample, used to detect sleep/suspend gaps.
    last_seen: i64,
}

impl Tracker {
    /// Create a tracker with nothing open.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one sample: the time and what is focused (`None` when no window is).
    ///
    /// Call this on every focus-related event AND on a regular heartbeat, so
    /// that the open interval's end time keeps moving forward.
    pub fn observe(&mut self, now: i64, snapshot: Option<WindowSnapshot>) -> Vec<Effect> {
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
                self.open = Some(OpenInterval { snapshot });
            }
        }

        self.last_seen = now;
        effects
    }

    /// Close whatever is open (used on shutdown or when Hyprland goes away).
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
}
