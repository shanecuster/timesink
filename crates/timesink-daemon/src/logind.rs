//! Suspend and lock notifications from systemd-logind, over the system D-Bus.
//!
//! logind announces:
//!   - `PrepareForSleep(true)`  just before the machine suspends,
//!   - `PrepareForSleep(false)` right after it wakes,
//!   - `Lock` / `Unlock` on the session when `loginctl lock-session` is used.
//!
//! A screen locker started directly (without logind) is not seen here; the idle
//! timer still ends the session's counted time shortly afterwards.

use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use zbus::blocking::{Connection, MessageIterator};
use zbus::message::Type as MessageType;
use zbus::MatchRule;

use crate::service::Msg;

/// Turn one logind signal into a message for the main thread.
///
/// `flag` is the boolean payload of `PrepareForSleep` (true = going to sleep).
/// It is only looked at for that signal, and may be missing if it failed to parse.
pub fn signal_to_msg(interface: &str, member: &str, flag: Option<bool>) -> Option<Msg> {
    match (interface, member) {
        ("org.freedesktop.login1.Manager", "PrepareForSleep") => {
            Some(if flag? { Msg::Sleep } else { Msg::Wake })
        }
        ("org.freedesktop.login1.Session", "Lock") => Some(Msg::Lock),
        ("org.freedesktop.login1.Session", "Unlock") => Some(Msg::Unlock),
        _ => None,
    }
}

/// Listen for logind signals until the connection fails.
fn watch(tx: &Sender<Msg>) -> Result<()> {
    let conn = Connection::system().context("connecting to the system D-Bus")?;
    // Only signals really sent by logind are accepted.
    let rule = MatchRule::builder()
        .msg_type(MessageType::Signal)
        .sender("org.freedesktop.login1")?
        .build();
    let messages = MessageIterator::for_match_rule(rule, &conn, None)
        .context("subscribing to logind signals")?;

    for message in messages {
        let message = message.context("reading a D-Bus message")?;
        let header = message.header();
        let (Some(interface), Some(member)) = (header.interface(), header.member()) else {
            continue;
        };
        let flag = message.body().deserialize::<bool>().ok();
        if let Some(msg) = signal_to_msg(interface.as_str(), member.as_str(), flag) {
            if tx.send(msg).is_err() {
                return Ok(()); // main thread is gone
            }
        }
    }
    Ok(())
}

/// Run the logind listener in the background, retrying if the bus is not
/// available yet or the connection drops.
pub fn spawn(tx: Sender<Msg>) {
    thread::spawn(move || {
        let mut warned = false;
        loop {
            if let Err(e) = watch(&tx) {
                // Say it once; without this, the idle timer and the tracker's
                // gap check still keep sleep time from being counted.
                if !warned {
                    eprintln!("timesink: sleep and lock notices unavailable, will retry: {e:#}");
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

    /// Compare messages by their debug text (Msg does not implement PartialEq).
    fn name(msg: Option<Msg>) -> String {
        format!("{msg:?}")
    }

    #[test]
    fn prepare_for_sleep_true_and_false() {
        let manager = "org.freedesktop.login1.Manager";
        assert_eq!(
            name(signal_to_msg(manager, "PrepareForSleep", Some(true))),
            "Some(Sleep)"
        );
        assert_eq!(
            name(signal_to_msg(manager, "PrepareForSleep", Some(false))),
            "Some(Wake)"
        );
        // Without a readable flag we cannot tell which one it was.
        assert_eq!(
            name(signal_to_msg(manager, "PrepareForSleep", None)),
            "None"
        );
    }

    #[test]
    fn lock_and_unlock() {
        let session = "org.freedesktop.login1.Session";
        assert_eq!(name(signal_to_msg(session, "Lock", None)), "Some(Lock)");
        assert_eq!(name(signal_to_msg(session, "Unlock", None)), "Some(Unlock)");
    }

    #[test]
    fn other_signals_are_ignored() {
        assert_eq!(
            name(signal_to_msg(
                "org.freedesktop.login1.Manager",
                "SessionNew",
                None
            )),
            "None"
        );
        assert_eq!(name(signal_to_msg("some.other", "Lock", None)), "None");
    }
}
