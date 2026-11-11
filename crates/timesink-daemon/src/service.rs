//! The long-running collector.
//!
//! Many small threads watch different things (Hyprland, and later idle, sleep,
//! lock and media). Each one sends `Msg` values into ONE channel. The main
//! thread reads that channel and owns all state, so nothing needs locking.
//! If nothing arrives for `HEARTBEAT`, it runs a heartbeat that keeps the open
//! interval's end time moving forward.
//!
//! The service never exits on its own: a vanished Hyprland, a failed query or a
//! dropped connection are logged and retried.

use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Result};
use timesink_core::db::{Db, Recorder};
use timesink_core::listen::{ListenEffect, ListenTracker};
use timesink_core::model::WindowSnapshot;
use timesink_core::presence::{Presence, PresenceState};
use timesink_core::sanitize::Sanitizer;
use timesink_core::tracker::{Effect, Tracker, MAX_GAP_SECS};

use crate::hyprland::{is_relevant_event, Hypr, Next};
use crate::{idle, logind, media, paths};

/// How often the open interval is refreshed when no events arrive.
const HEARTBEAT: Duration = Duration::from_secs(30);

/// Something a watcher thread wants the main thread to know.
#[derive(Debug)]
pub enum Msg {
    /// Hyprland is reachable (first start, or it came back).
    Connected(Hypr),
    /// Hyprland went away.
    Disconnected,
    /// One raw Hyprland event line, e.g. `activewindowv2>>abc`.
    FocusEvent(String),
    /// The compositor reports no input since this Unix time.
    Idle { since: i64 },
    /// Input is happening (also sent when idle detection first connects).
    Active,
    /// The system is about to suspend.
    Sleep,
    /// The system woke up.
    Wake,
    /// The session was locked.
    Lock,
    /// The session was unlocked.
    Unlock,
    /// Whether any media player is playing changed.
    /// Ids of the media players that are playing now (empty = none).
    Media(Vec<String>),
}

/// Current time in whole seconds since the Unix epoch.
pub fn now_ts() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Everything the main thread owns.
struct Service {
    sanitizer: Sanitizer,
    tracker: Tracker,
    recorder: Recorder,
    presence: Presence,
    /// The last presence state we acted on, so changes can be logged once.
    last_presence: PresenceState,
    /// Set while Hyprland is reachable.
    hypr: Option<Hypr>,
    /// Turns "who is playing" into listening rows.
    listen: ListenTracker,
    /// The players that were playing at the last report.
    players: Vec<String>,
    verbose: bool,
}

impl Service {
    /// React to one message from a watcher thread.
    fn handle(&mut self, msg: Msg) {
        let now = now_ts();
        match msg {
            Msg::Connected(hypr) => {
                eprintln!("timesink: connected to Hyprland");
                self.hypr = Some(hypr);
                self.sample(now);
            }
            Msg::Disconnected => {
                eprintln!("timesink: lost Hyprland, waiting for it to return");
                self.hypr = None;
                let effects = self.tracker.finish(now);
                self.apply(&effects);
            }
            Msg::FocusEvent(line) => {
                if is_relevant_event(&line) {
                    self.sample(now);
                }
            }
            Msg::Idle { since } => {
                self.presence.idle_started(since);
                self.refresh_presence(now);
            }
            Msg::Active => {
                self.presence.idle_ended();
                self.refresh_presence(now);
                // Back at the keyboard: start timing the focused window again.
                self.sample(now);
            }
            Msg::Sleep => {
                eprintln!("timesink: system going to sleep");
                // If this notice was only delivered after the machine woke up
                // (the daemon was frozen), "now" is too late: the sleep began
                // when we last saw it awake.
                let last_seen = self.tracker.last_seen();
                let since = if now - last_seen > MAX_GAP_SECS {
                    last_seen
                } else {
                    now
                };
                self.presence.sleep_started(since);
                self.refresh_presence(now);
                // Nothing is heard while asleep: end any listening row there.
                let effects = self.listen.suspend(since);
                self.apply_listen(&effects);
            }
            Msg::Wake => {
                eprintln!("timesink: system woke up");
                self.presence.sleep_ended();
                self.refresh_presence(now);
                self.listen.resume();
                self.update_listening(now);
                self.sample(now);
            }
            Msg::Lock => {
                eprintln!("timesink: screen locked");
                self.presence.lock_started(now);
                self.refresh_presence(now);
            }
            Msg::Unlock => {
                eprintln!("timesink: screen unlocked");
                self.presence.lock_ended();
                self.refresh_presence(now);
                self.sample(now);
            }
            Msg::Media(playing) => {
                if self.verbose {
                    eprintln!("timesink: media playing: {:?}", playing);
                }
                self.players = playing;
                self.presence.media_changed(now, !self.players.is_empty());
                self.refresh_presence(now);
                self.update_listening(now);
                // Media can end an idle period, so there may be a window to time.
                self.sample(now);
            }
        }
    }

    /// The 30-second tick when nothing else happened.
    fn heartbeat(&mut self) {
        let now = now_ts();
        self.presence.heartbeat(now);
        self.refresh_presence(now);
        self.update_listening(now);
        self.sample(now);
    }

    /// Re-evaluate presence and tell the tracker if it changed.
    ///
    /// Call after every message that can change presence, and on heartbeat.
    fn refresh_presence(&mut self, now: i64) {
        let state = self.presence.state();
        if state != self.last_presence {
            match state {
                PresenceState::Active => eprintln!("timesink: active"),
                PresenceState::Away { since } => {
                    eprintln!("timesink: away (since {}s ago)", now - since)
                }
            }
            self.last_presence = state;
        }
        let effects = self.tracker.set_presence(now, state);
        self.apply(&effects);
    }

    /// Ask Hyprland what is focused, clean it, and feed it to the tracker.
    fn sample(&mut self, now: i64) {
        // Nothing to count while away or while Hyprland is unreachable.
        if self.tracker.is_away() {
            return;
        }
        let Some(hypr) = &self.hypr else {
            return;
        };

        // A failed query is logged and skipped; the next event or heartbeat retries.
        let window = match hypr.active_window() {
            Ok(window) => window,
            Err(e) => {
                eprintln!("timesink: could not read the focused window: {e:#}");
                return;
            }
        };
        // Turn Hyprland's raw answer into a privacy-cleaned snapshot.
        let snapshot = window.map(|w| WindowSnapshot {
            title: self.sanitizer.clean_title(&w.class, &w.title),
            app_class: w.class,
            workspace: w.workspace,
        });

        let effects = self.tracker.observe(now, snapshot);
        if self.verbose {
            for effect in &effects {
                if let Effect::Opened { snapshot, .. } = effect {
                    eprintln!(
                        "focus: {} | {} | ws {}",
                        snapshot.app_class, snapshot.title, snapshot.workspace
                    );
                }
            }
        }
        self.apply(&effects);
    }

    /// Store tracker effects; a database error is logged, not fatal.
    /// Tell the listening tracker who is playing now and store the result.
    fn update_listening(&mut self, now: i64) {
        let effects = self.listen.observe(now, &self.players);
        self.apply_listen(&effects);
    }

    /// Store listening changes; a failure is reported but never stops the daemon.
    fn apply_listen(&mut self, effects: &[ListenEffect]) {
        if let Err(e) = self.recorder.apply_listen(effects) {
            eprintln!("timesink: could not write listening time: {e:#}");
        }
    }

    fn apply(&mut self, effects: &[Effect]) {
        if let Err(e) = self.recorder.apply(effects) {
            eprintln!("timesink: could not write to the database: {e:#}");
        }
    }
}

/// Watch Hyprland forever: find it, stream its events, reconnect when it drops.
fn spawn_hyprland_watcher(tx: Sender<Msg>) {
    thread::spawn(move || {
        // Wait 1s, then 2s, 4s ... up to 30s between attempts.
        let mut backoff = Duration::from_secs(1);
        let mut warned = false;
        loop {
            let connected = Hypr::discover().and_then(|hypr| {
                // A long timeout: we only need to notice a dead connection.
                let events = hypr.events(Duration::from_secs(60))?;
                Ok((hypr, events))
            });
            match connected {
                Ok((hypr, mut events)) => {
                    backoff = Duration::from_secs(1);
                    warned = false;
                    if tx.send(Msg::Connected(hypr)).is_err() {
                        return; // main thread is gone
                    }
                    // Forward events until the stream ends or fails.
                    loop {
                        match events.next() {
                            Ok(Next::Event(line)) => {
                                if tx.send(Msg::FocusEvent(line)).is_err() {
                                    return;
                                }
                            }
                            Ok(Next::Timeout) => {}
                            Ok(Next::Closed) => break,
                            Err(e) => {
                                eprintln!("timesink: Hyprland event stream failed: {e:#}");
                                break;
                            }
                        }
                    }
                    if tx.send(Msg::Disconnected).is_err() {
                        return;
                    }
                }
                Err(e) => {
                    // Say it once, not every few seconds.
                    if !warned {
                        eprintln!("timesink: waiting for Hyprland: {e:#}");
                        warned = true;
                    }
                }
            }
            thread::sleep(backoff);
            backoff = (backoff * 2).min(Duration::from_secs(30));
        }
    });
}

/// Run the collector. Only returns if the database cannot be opened.
pub fn run(verbose: bool) -> Result<()> {
    let config = paths::load_config()?;
    let sanitizer = paths::load_sanitizer(&config)?;
    let path = paths::db_path()?;
    // Refuse to start if another daemon is already recording; two would
    // double every total. The lock is held until this function returns.
    let _instance = crate::lock::InstanceLock::acquire(&paths::data_dir()?.join("timesink.lock"))?;
    let recorder = Recorder::new(Db::open(&path, &paths::device_id())?);
    eprintln!("timesink: recording to {}", path.display());

    let mut presence = Presence::new();
    presence.set_ignore_media_for_idle(config.idle_ignores_media);
    let mut service = Service {
        sanitizer,
        tracker: Tracker::new(),
        recorder,
        presence,
        last_presence: PresenceState::Active,
        hypr: None,
        listen: ListenTracker::new(),
        players: Vec::new(),
        verbose,
    };

    // All watcher threads send into this one channel.
    let (tx, rx) = mpsc::channel::<Msg>();
    spawn_hyprland_watcher(tx.clone());
    eprintln!(
        "timesink: idle after {} minutes without input",
        config.idle_timeout_secs / 60
    );
    if config.idle_ignores_media {
        eprintln!("timesink: idle stops focus time even while media plays");
    }
    idle::spawn(tx.clone(), config.idle_timeout_secs);
    logind::spawn(tx.clone());
    media::spawn(tx.clone());

    loop {
        match rx.recv_timeout(HEARTBEAT) {
            Ok(msg) => service.handle(msg),
            Err(RecvTimeoutError::Timeout) => service.heartbeat(),
            Err(RecvTimeoutError::Disconnected) => bail!("all watcher threads stopped"),
        }
    }
}
