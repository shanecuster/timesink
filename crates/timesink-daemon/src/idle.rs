//! Idle detection through the Wayland `ext-idle-notify-v1` protocol.
//!
//! We ask the compositor (Hyprland) to tell us when there has been no keyboard
//! or mouse input for the configured time ("idled"), and again when input
//! returns ("resumed"). The v1 protocol honours idle inhibitors, so while a
//! browser or video player is holding the screen awake for a video, the
//! compositor never reports idle and that time keeps counting.

use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols::ext::idle_notify::v1::client::ext_idle_notification_v1::{
    self, ExtIdleNotificationV1,
};
use wayland_protocols::ext::idle_notify::v1::client::ext_idle_notifier_v1::ExtIdleNotifierV1;

use crate::service::{now_ts, Msg};

/// Find a Wayland socket in the runtime folder: the lowest-numbered `wayland-N`.
///
/// Needed when `WAYLAND_DISPLAY` is not set, as in a systemd user service.
pub fn find_wayland_socket(runtime: &Path) -> Option<PathBuf> {
    let mut sockets: Vec<PathBuf> = std::fs::read_dir(runtime)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            // Match `wayland-<digits>` exactly; this skips `wayland-1.lock`.
            path.file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.strip_prefix("wayland-"))
                .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
        })
        .collect();
    sockets.sort();
    sockets.into_iter().next()
}

/// Connect to the compositor, with or without `WAYLAND_DISPLAY`.
fn connect() -> Result<Connection> {
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        return Connection::connect_to_env().context("connecting to the Wayland display");
    }
    let runtime = std::env::var("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is not set")?;
    let socket = find_wayland_socket(Path::new(&runtime))
        .context("no Wayland socket found (is a compositor running?)")?;
    let stream = UnixStream::connect(&socket)
        .with_context(|| format!("connecting to {}", socket.display()))?;
    Connection::from_socket(stream).context("starting the Wayland connection")
}

/// State handed to the Wayland event callbacks.
struct State {
    tx: Sender<Msg>,
    /// The idle timeout, used to work out when the last input happened.
    timeout_secs: i64,
}

// The next three impls are required by wayland-client but we ignore these events.
impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for State {
    fn event(
        _: &mut Self,
        _: &wl_seat::WlSeat,
        _: wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtIdleNotifierV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &ExtIdleNotifierV1,
        _: <ExtIdleNotifierV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

// The one that matters: the compositor tells us when idle starts and ends.
impl Dispatch<ExtIdleNotificationV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ExtIdleNotificationV1,
        event: ext_idle_notification_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            // "Idled" arrives AFTER the timeout has passed, so the last input
            // was `timeout` seconds ago.
            ext_idle_notification_v1::Event::Idled => {
                let since = now_ts() - state.timeout_secs;
                let _ = state.tx.send(Msg::Idle { since });
            }
            ext_idle_notification_v1::Event::Resumed => {
                let _ = state.tx.send(Msg::Active);
            }
            _ => {}
        }
    }
}

/// Connect, subscribe and process idle events until the connection fails.
fn watch(tx: &Sender<Msg>, timeout_secs: i64) -> Result<()> {
    let conn = connect()?;
    let (globals, mut queue) =
        registry_queue_init::<State>(&conn).context("reading the Wayland globals")?;
    let qh = queue.handle();

    // We need a seat (the keyboard and mouse group) and the idle notifier.
    let seat: wl_seat::WlSeat = globals
        .bind(&qh, 1..=1, ())
        .context("the compositor has no wl_seat")?;
    let notifier: ExtIdleNotifierV1 = globals
        .bind(&qh, 1..=1, ())
        .context("the compositor does not support ext-idle-notify-v1")?;

    // Ask to be told after `timeout_secs` without input.
    let _notification =
        notifier.get_idle_notification((timeout_secs * 1000) as u32, &seat, &qh, ());

    // A fresh subscription always starts in the "active" state.
    let _ = tx.send(Msg::Active);

    let mut state = State {
        tx: tx.clone(),
        timeout_secs,
    };
    loop {
        queue
            .blocking_dispatch(&mut state)
            .context("reading Wayland events")?;
    }
}

/// Run idle detection in the background, retrying if the compositor is not
/// ready yet or the connection drops.
pub fn spawn(tx: Sender<Msg>, timeout_secs: i64) {
    thread::spawn(move || {
        let mut warned = false;
        loop {
            if let Err(e) = watch(&tx, timeout_secs) {
                // Say it once; without idle detection time simply keeps counting.
                if !warned {
                    eprintln!("timesink: idle detection unavailable, will retry: {e:#}");
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
    use std::os::unix::net::UnixListener;

    #[test]
    fn finds_the_lowest_numbered_wayland_socket() {
        let dir = std::env::temp_dir().join(format!("timesink-wl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Real socket files plus noise that must be ignored.
        let _s1 = UnixListener::bind(dir.join("wayland-1")).unwrap();
        let _s0 = UnixListener::bind(dir.join("wayland-0")).unwrap();
        std::fs::write(dir.join("wayland-0.lock"), "").unwrap();
        std::fs::write(dir.join("wayland-extra"), "").unwrap();
        std::fs::write(dir.join("bus"), "").unwrap();

        let found = find_wayland_socket(&dir).unwrap();
        assert!(found.ends_with("wayland-0"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn no_wayland_socket_means_none() {
        let dir = std::env::temp_dir().join(format!("timesink-wl-none-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("wayland-1.lock"), "").unwrap();
        assert_eq!(find_wayland_socket(&dir), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
