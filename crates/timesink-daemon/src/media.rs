//! Detects playing audio or video through MPRIS, the media-player D-Bus standard.
//!
//! Players such as Spotify, mpv and browsers publish a name starting with
//! `org.mpris.MediaPlayer2.` on the session bus and expose a `PlaybackStatus`
//! of `Playing`, `Paused` or `Stopped`. If any player is playing, you count as
//! present even without touching the keyboard.

use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use zbus::blocking::fdo::DBusProxy;
use zbus::blocking::Connection;
use zbus::zvariant::OwnedValue;

use crate::service::Msg;

/// How often the players are checked.
const POLL: Duration = Duration::from_secs(5);

/// The well-known name prefix every MPRIS player uses.
const MPRIS_PREFIX: &str = "org.mpris.MediaPlayer2.";

/// Is any of these playback statuses "Playing"?
pub fn any_playing<S: AsRef<str>>(statuses: impl IntoIterator<Item = S>) -> bool {
    statuses.into_iter().any(|s| s.as_ref() == "Playing")
}

/// Ask every MPRIS player on the session bus whether it is playing.
fn poll_once(conn: &Connection) -> Result<bool> {
    let names = DBusProxy::new(conn)?
        .list_names()
        .context("listing D-Bus names")?;

    let mut statuses = Vec::new();
    for name in names {
        let name = name.as_str();
        if !name.starts_with(MPRIS_PREFIX) {
            continue;
        }
        // Read the property once, without subscribing to changes.
        let reply = conn.call_method(
            Some(name),
            "/org/mpris/MediaPlayer2",
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &("org.mpris.MediaPlayer2.Player", "PlaybackStatus"),
        );
        // A player that vanished or misbehaves is simply skipped.
        let Ok(reply) = reply else { continue };
        let Ok(value) = reply.body().deserialize::<OwnedValue>() else {
            continue;
        };
        if let Ok(status) = String::try_from(value) {
            statuses.push(status);
        }
    }
    Ok(any_playing(statuses))
}

/// Poll forever and report each CHANGE in "is anything playing".
fn watch(tx: &Sender<Msg>) -> Result<()> {
    let conn = Connection::session().context("connecting to the session D-Bus")?;
    let mut last = false;
    loop {
        let playing = poll_once(&conn)?;
        if playing != last {
            last = playing;
            if tx.send(Msg::Media(playing)).is_err() {
                return Ok(()); // main thread is gone
            }
        }
        thread::sleep(POLL);
    }
}

/// Run media detection in the background, retrying if the bus is not available.
pub fn spawn(tx: Sender<Msg>) {
    thread::spawn(move || {
        let mut warned = false;
        loop {
            if let Err(e) = watch(&tx) {
                // Say it once; without this, playing media simply does not
                // prevent the idle timer from ending your counted time.
                if !warned {
                    eprintln!("timesink: media detection unavailable, will retry: {e:#}");
                    warned = true;
                }
            }
            thread::sleep(Duration::from_secs(10));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playing_is_detected_among_players() {
        assert!(any_playing(["Paused", "Playing"]));
        assert!(any_playing(vec!["Playing".to_string()]));
    }

    #[test]
    fn paused_stopped_or_no_players_is_not_playing() {
        assert!(!any_playing(["Paused", "Stopped"]));
        assert!(!any_playing(Vec::<String>::new()));
        // Case matters: MPRIS defines the exact string "Playing".
        assert!(!any_playing(["playing"]));
    }
}
