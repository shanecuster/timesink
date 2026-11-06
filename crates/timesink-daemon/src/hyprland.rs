//! A tiny Hyprland IPC client using only the standard library.
//!
//! Hyprland exposes two Unix sockets in `$XDG_RUNTIME_DIR/hypr/<instance>/`:
//!   - `.socket.sock`  : request/response. We send `j/activewindow`, get JSON back.
//!   - `.socket2.sock` : a one-way stream of events, one per line: `name>>data`.
//!
//! Strategy: the event stream only tells us that SOMETHING changed. Each time a
//! relevant event arrives we ask `.socket.sock` what is focused now. That avoids
//! depending on the exact data format of each event type.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context, Result};

/// The focused window as Hyprland reports it, before privacy cleaning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawWindow {
    pub class: String,
    pub title: String,
    pub workspace: String,
}

/// Parse the JSON answer to `j/activewindow`.
///
/// Returns `Ok(None)` when nothing is focused (Hyprland answers `{}` or, on
/// older versions, a plain-text message).
pub fn parse_active_window(reply: &str) -> Result<Option<RawWindow>> {
    let reply = reply.trim();
    // Anything that is not a JSON object means "no active window".
    if !reply.starts_with('{') {
        return Ok(None);
    }
    let value: serde_json::Value =
        serde_json::from_str(reply).context("parsing activewindow JSON")?;

    let class = value["class"].as_str().unwrap_or_default();
    // An empty class means no real window is focused.
    if class.is_empty() {
        return Ok(None);
    }
    // The workspace is an object like {"id": 1, "name": "1"}.
    let workspace = value["workspace"]["name"]
        .as_str()
        .map(str::to_string)
        .or_else(|| value["workspace"]["id"].as_i64().map(|id| id.to_string()))
        .unwrap_or_default();

    Ok(Some(RawWindow {
        class: class.to_string(),
        title: value["title"].as_str().unwrap_or_default().to_string(),
        workspace,
    }))
}

/// Event names that can change what is focused. Anything else is ignored.
const RELEVANT_EVENTS: &[&str] = &[
    "activewindow",
    "activewindowv2",
    "windowtitle",
    "windowtitlev2",
    "workspace",
    "workspacev2",
    "focusedmon",
    "focusedmonv2",
    "openwindow",
    "closewindow",
    "movewindow",
    "movewindowv2",
    "fullscreen",
];

/// Is this event line one that should trigger a re-check of the focused window?
pub fn is_relevant_event(line: &str) -> bool {
    let name = line.split_once(">>").map_or(line, |(name, _)| name);
    RELEVANT_EVENTS.contains(&name)
}

/// Splits a byte stream into text lines, even when a line arrives in pieces.
#[derive(Debug, Default)]
pub struct LineSplitter {
    /// Bytes received so far that do not yet end in a newline.
    pending: Vec<u8>,
}

impl LineSplitter {
    /// Add newly received bytes and return every line that is now complete.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<String> {
        self.pending.extend_from_slice(bytes);
        let mut lines = Vec::new();
        // Repeatedly cut off everything up to the next newline.
        while let Some(pos) = self.pending.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=pos).collect();
            lines.push(String::from_utf8_lossy(&line[..pos]).into_owned());
        }
        lines
    }
}

/// Where Hyprland's sockets live for the current session.
pub struct Hypr {
    dir: PathBuf,
}

impl Hypr {
    /// Find the socket folder from the environment Hyprland sets up.
    pub fn from_env() -> Result<Self> {
        let runtime = std::env::var("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is not set")?;
        let Ok(signature) = std::env::var("HYPRLAND_INSTANCE_SIGNATURE") else {
            bail!("HYPRLAND_INSTANCE_SIGNATURE is not set (is Hyprland running in this session?)");
        };
        Ok(Self {
            dir: PathBuf::from(runtime).join("hypr").join(signature),
        })
    }

    /// Ask Hyprland which window is focused right now.
    pub fn active_window(&self) -> Result<Option<RawWindow>> {
        let mut stream = UnixStream::connect(self.dir.join(".socket.sock"))
            .context("connecting to Hyprland command socket")?;
        // Do not hang forever if Hyprland stops answering.
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        stream.write_all(b"j/activewindow")?;
        let mut reply = String::new();
        stream.read_to_string(&mut reply)?;
        parse_active_window(&reply)
    }

    /// Open the event stream. `timeout` is how long to wait for an event before
    /// reporting `Next::Timeout` (used as the heartbeat).
    pub fn events(&self, timeout: Duration) -> Result<EventStream> {
        let stream = UnixStream::connect(self.dir.join(".socket2.sock"))
            .context("connecting to Hyprland event socket")?;
        stream.set_read_timeout(Some(timeout))?;
        Ok(EventStream {
            stream,
            splitter: LineSplitter::default(),
            queue: VecDeque::new(),
        })
    }
}

/// What `EventStream::next` produced.
#[derive(Debug, PartialEq, Eq)]
pub enum Next {
    /// One event line, e.g. `activewindow>>wezterm,title`.
    Event(String),
    /// Nothing happened for the whole timeout.
    Timeout,
    /// Hyprland closed the connection (usually: it exited).
    Closed,
}

/// A stream of Hyprland events.
pub struct EventStream {
    stream: UnixStream,
    splitter: LineSplitter,
    /// Complete lines that were read but not yet handed out.
    queue: VecDeque<String>,
}

impl EventStream {
    /// Wait for the next event, a timeout, or the end of the stream.
    pub fn next(&mut self) -> Result<Next> {
        loop {
            // Hand out any line we already have.
            if let Some(line) = self.queue.pop_front() {
                return Ok(Next::Event(line));
            }
            // Otherwise read more bytes from the socket.
            let mut buf = [0u8; 4096];
            match self.stream.read(&mut buf) {
                Ok(0) => return Ok(Next::Closed),
                Ok(n) => self.queue.extend(self.splitter.push(&buf[..n])),
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    return Ok(Next::Timeout)
                }
                Err(e) => return Err(e).context("reading Hyprland events"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    #[test]
    fn parses_a_normal_reply() {
        let json = r#"{"address":"0x1","class":"org.wezfurlong.wezterm","title":"[4/4] ~/Projects - fish","pid":6440,"workspace":{"id":1,"name":"1"}}"#;
        let w = parse_active_window(json).unwrap().unwrap();
        assert_eq!(w.class, "org.wezfurlong.wezterm");
        assert_eq!(w.title, "[4/4] ~/Projects - fish");
        assert_eq!(w.workspace, "1");
    }

    #[test]
    fn no_window_means_none() {
        assert_eq!(parse_active_window("{}").unwrap(), None);
        assert_eq!(parse_active_window("Invalid").unwrap(), None);
        assert_eq!(parse_active_window("").unwrap(), None);
    }

    #[test]
    fn workspace_falls_back_to_id() {
        let w = parse_active_window(r#"{"class":"a","title":"b","workspace":{"id":7}}"#)
            .unwrap()
            .unwrap();
        assert_eq!(w.workspace, "7");
    }

    #[test]
    fn relevant_events_are_recognised() {
        assert!(is_relevant_event("activewindowv2>>abc"));
        assert!(is_relevant_event("windowtitlev2>>abc,New title"));
        assert!(!is_relevant_event("monitoradded>>DP-1"));
        assert!(!is_relevant_event("submap>>"));
    }

    #[test]
    fn splitter_handles_partial_and_multiple_lines() {
        let mut s = LineSplitter::default();
        assert_eq!(s.push(b"activewindow>>zen,Tit"), Vec::<String>::new());
        assert_eq!(
            s.push(b"le\nworkspace>>2\nfocus"),
            vec!["activewindow>>zen,Title", "workspace>>2"]
        );
        assert_eq!(s.push(b"edmon>>DP-1\n"), vec!["focusedmon>>DP-1"]);
    }

    #[test]
    fn event_stream_reads_lines_then_times_out_then_closes() {
        // Fake Hyprland event socket in a temp folder.
        let dir = std::env::temp_dir().join(format!("timesink-hypr-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join(".socket2.sock");
        let listener = UnixListener::bind(&sock).unwrap();

        let hypr = Hypr { dir: dir.clone() };
        let mut events = hypr.events(Duration::from_millis(200)).unwrap();
        let (mut server, _) = listener.accept().unwrap();

        server
            .write_all(b"workspace>>1\nactivewindow>>a,b\n")
            .unwrap();
        assert_eq!(events.next().unwrap(), Next::Event("workspace>>1".into()));
        assert_eq!(
            events.next().unwrap(),
            Next::Event("activewindow>>a,b".into())
        );
        // Nothing more is sent, so we expect the heartbeat timeout.
        assert_eq!(events.next().unwrap(), Next::Timeout);
        // Closing the server side ends the stream.
        drop(server);
        assert_eq!(events.next().unwrap(), Next::Closed);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn active_window_query_round_trip() {
        let dir = std::env::temp_dir().join(format!("timesink-query-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let listener = UnixListener::bind(dir.join(".socket.sock")).unwrap();

        // Fake Hyprland: read the request, answer with JSON, close.
        let handle = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            let mut buf = [0u8; 64];
            let n = conn.read(&mut buf).unwrap();
            assert_eq!(&buf[..n], b"j/activewindow");
            conn.write_all(br#"{"class":"zen","title":"x","workspace":{"id":2,"name":"2"}}"#)
                .unwrap();
        });

        let hypr = Hypr { dir: dir.clone() };
        let w = hypr.active_window().unwrap().unwrap();
        assert_eq!(w.class, "zen");
        assert_eq!(w.workspace, "2");
        handle.join().unwrap();

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
