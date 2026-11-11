//! Presence: decides whether you are at the computer or away.
//!
//! Several independent sources can say "away": the compositor's idle timer,
//! suspend, and the lock screen. Playing audio or video overrides ONLY the idle
//! timer (you may be watching without touching the keyboard), never sleep or lock.
//! That can be switched off (`idle_ignores_media` in `config.conf`), so that being idle
//! always means away even while something plays; listening time is unaffected.
//!
//! Like the tracker, this is a pure state machine: the daemon feeds it events
//! with timestamps and asks for the current `PresenceState`.

/// Whether you count as present, and if not, since when.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresenceState {
    /// At the computer: time should be counted.
    Active,
    /// Away since this Unix time: time after it must NOT be counted.
    Away { since: i64 },
}

/// Collects the facts that decide `PresenceState`.
#[derive(Debug, Default)]
pub struct Presence {
    /// Set when the compositor reports no input; holds the time of the last input.
    idle_since: Option<i64>,
    /// Set from the moment the system starts suspending until it wakes.
    asleep_since: Option<i64>,
    /// Set while the session is locked.
    locked_since: Option<i64>,
    /// True while any media player reports "Playing".
    media_playing: bool,
    /// The last time we knew media was playing (used to end idle time correctly).
    last_media_ts: i64,
    /// When true, idle means away even while media plays (the default is false: playing
    /// media keeps you counted as present).
    ignore_media_for_idle: bool,
}

impl Presence {
    /// Create a presence tracker that starts out `Active`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Choose whether playing media keeps you present while idle. `true` means media is
    /// ignored for idle: no input for the idle timeout always counts as away.
    pub fn set_ignore_media_for_idle(&mut self, ignore: bool) {
        self.ignore_media_for_idle = ignore;
    }

    /// The compositor says there has been no input since `since`.
    pub fn idle_started(&mut self, since: i64) {
        self.idle_since = Some(since);
    }

    /// Input happened again.
    pub fn idle_ended(&mut self) {
        self.idle_since = None;
    }

    /// The system is about to suspend.
    pub fn sleep_started(&mut self, now: i64) {
        self.asleep_since.get_or_insert(now);
    }

    /// The system woke up.
    pub fn sleep_ended(&mut self) {
        self.asleep_since = None;
    }

    /// The session was locked.
    pub fn lock_started(&mut self, now: i64) {
        self.locked_since.get_or_insert(now);
    }

    /// The session was unlocked.
    pub fn lock_ended(&mut self) {
        self.locked_since = None;
    }

    /// A media player started or stopped playing.
    pub fn media_changed(&mut self, now: i64, playing: bool) {
        // Either way, media was playing up to this moment (or starts now).
        self.last_media_ts = now;
        self.media_playing = playing;
    }

    /// Call on every heartbeat so a long-running video keeps `last_media_ts` fresh.
    pub fn heartbeat(&mut self, now: i64) {
        if self.media_playing {
            self.last_media_ts = now;
        }
    }

    /// Work out the current state.
    pub fn state(&self) -> PresenceState {
        // Sleep and lock always count as away.
        let mut earliest: Option<i64> = [self.asleep_since, self.locked_since]
            .into_iter()
            .flatten()
            .min();

        // Idle counts as away unless media is playing. When it does apply, the
        // away time starts at the last input OR the last time media was playing,
        // whichever is later, so time spent watching is not thrown away.
        // If media is set to be ignored, idle always counts, from the last input.
        if let Some(idle_since) = self.idle_since {
            if self.ignore_media_for_idle {
                earliest = Some(earliest.map_or(idle_since, |e| e.min(idle_since)));
            } else if !self.media_playing {
                let since = idle_since.max(self.last_media_ts);
                earliest = Some(earliest.map_or(since, |e| e.min(since)));
            }
        }

        match earliest {
            Some(since) => PresenceState::Away { since },
            None => PresenceState::Active,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_active() {
        assert_eq!(Presence::new().state(), PresenceState::Active);
    }

    #[test]
    fn idle_means_away_since_last_input() {
        let mut p = Presence::new();
        p.idle_started(700);
        assert_eq!(p.state(), PresenceState::Away { since: 700 });
        p.idle_ended();
        assert_eq!(p.state(), PresenceState::Active);
    }

    #[test]
    fn media_playing_overrides_idle() {
        let mut p = Presence::new();
        p.media_changed(500, true);
        p.idle_started(700);
        assert_eq!(p.state(), PresenceState::Active);
    }

    #[test]
    fn media_can_be_set_to_not_override_idle() {
        let mut p = Presence::new();
        p.set_ignore_media_for_idle(true);
        p.media_changed(500, true);
        p.heartbeat(900);
        p.idle_started(700);
        // Away from the last input, even though media is still playing.
        assert_eq!(p.state(), PresenceState::Away { since: 700 });
        // Input returns: present again.
        p.idle_ended();
        assert_eq!(p.state(), PresenceState::Active);
    }

    #[test]
    fn ignoring_media_does_not_move_the_away_time_when_it_stops() {
        let mut p = Presence::new();
        p.set_ignore_media_for_idle(true);
        p.idle_started(700);
        p.media_changed(1650, true);
        p.media_changed(1700, false);
        assert_eq!(p.state(), PresenceState::Away { since: 700 });
    }

    #[test]
    fn idle_time_starts_when_media_stops() {
        let mut p = Presence::new();
        p.idle_started(700);
        p.media_changed(800, true); // started watching while idle
        p.heartbeat(1200);
        p.heartbeat(1600);
        assert_eq!(p.state(), PresenceState::Active);
        // Video ends at 1650: away counts from then, not from 700.
        p.media_changed(1650, false);
        assert_eq!(p.state(), PresenceState::Away { since: 1650 });
    }

    #[test]
    fn sleep_overrides_media() {
        let mut p = Presence::new();
        p.media_changed(500, true);
        p.sleep_started(900);
        assert_eq!(p.state(), PresenceState::Away { since: 900 });
        p.sleep_ended();
        assert_eq!(p.state(), PresenceState::Active);
    }

    #[test]
    fn lock_overrides_media() {
        let mut p = Presence::new();
        p.media_changed(500, true);
        p.lock_started(900);
        assert_eq!(p.state(), PresenceState::Away { since: 900 });
        p.lock_ended();
        assert_eq!(p.state(), PresenceState::Active);
    }

    #[test]
    fn earliest_away_source_wins() {
        let mut p = Presence::new();
        p.idle_started(600); // stopped typing at 600
        p.lock_started(900); // screen locked later
        assert_eq!(p.state(), PresenceState::Away { since: 600 });
    }

    #[test]
    fn repeated_sleep_events_keep_the_first_time() {
        let mut p = Presence::new();
        p.sleep_started(900);
        p.sleep_started(950);
        assert_eq!(p.state(), PresenceState::Away { since: 900 });
    }
}
