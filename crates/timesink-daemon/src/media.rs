//! Detects playing audio or video through MPRIS, the media-player D-Bus standard.
//!
//! Players such as Spotify, mpv and browsers publish a name starting with
//! `org.mpris.MediaPlayer2.` on the session bus and expose a `PlaybackStatus`
//! of `Playing`, `Paused` or `Stopped`. If any player is playing, you count as
//! present even without touching the keyboard, and the playing time is
//! recorded as listening time. Only the player's name is used, never what it
//! is playing.

use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use zbus::blocking::fdo::DBusProxy;
use zbus::blocking::Connection;
use zbus::zvariant::OwnedValue;

use timesink_core::listen::player_id;

use crate::service::Msg;

/// How often the players are checked.
const POLL: Duration = Duration::from_secs(5);

/// The well-known name prefix every MPRIS player uses.
const MPRIS_PREFIX: &str = "org.mpris.MediaPlayer2.";

/// From (D-Bus name, playback status) pairs, the sorted ids of the players
/// whose status is exactly "Playing". Duplicates (two windows of one player)
/// collapse into one id.
pub fn playing_ids<N: AsRef<str>, S: AsRef<str>>(
    players: impl IntoIterator<Item = (N, S)>,
) -> Vec<String> {
    let mut ids: Vec<String> = players
        .into_iter()
        .filter(|(_, status)| status.as_ref() == "Playing")
        .map(|(name, _)| player_id(name.as_ref()))
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

/// Ask every MPRIS player on the session bus whether it is playing, and
/// return the ids of the ones that are.
fn poll_once(conn: &Connection) -> Result<Vec<String>> {
    let names = DBusProxy::new(conn)?
        .list_names()
        .context("listing D-Bus names")?;

    let mut players = Vec::new();
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
            players.push((name.to_string(), status));
        }
    }
    Ok(playing_ids(players))
}

/// Poll forever and report each CHANGE in the set of playing players.
fn watch(tx: &Sender<Msg>) -> Result<()> {
    let conn = Connection::session().context("connecting to the session D-Bus")?;
    let mut last: Vec<String> = Vec::new();
    loop {
        let playing = poll_once(&conn)?;
        if playing != last {
            last = playing.clone();
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
    fn playing_players_are_found_among_others() {
        let ids = playing_ids([
            ("org.mpris.MediaPlayer2.cider", "Playing"),
            ("org.mpris.MediaPlayer2.mpv", "Paused"),
            ("org.mpris.MediaPlayer2.firefox.instance99", "Playing"),
        ]);
        assert_eq!(ids, vec!["cider".to_string(), "firefox".to_string()]);
    }

    #[test]
    fn paused_stopped_or_no_players_is_not_playing() {
        assert!(playing_ids([("org.mpris.MediaPlayer2.a", "Paused")]).is_empty());
        assert!(playing_ids([("org.mpris.MediaPlayer2.a", "Stopped")]).is_empty());
        assert!(playing_ids(Vec::<(String, String)>::new()).is_empty());
        // Case matters: MPRIS defines the exact string "Playing".
        assert!(playing_ids([("org.mpris.MediaPlayer2.a", "playing")]).is_empty());
    }

    #[test]
    fn zens_underscored_instance_name_counts_as_the_browser() {
        let ids = playing_ids([
            ("org.mpris.MediaPlayer2.firefox.instance_1_82", "Playing"),
            ("org.mpris.MediaPlayer2.firefox.instance_1_90", "Playing"),
        ]);
        assert_eq!(ids, vec!["firefox".to_string()]);
    }

    #[test]
    fn two_windows_of_one_player_count_once() {
        let ids = playing_ids([
            ("org.mpris.MediaPlayer2.firefox.instance1", "Playing"),
            ("org.mpris.MediaPlayer2.firefox.instance2", "Playing"),
        ]);
        assert_eq!(ids, vec!["firefox".to_string()]);
    }
}
