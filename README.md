# timesink

A screen-time monitor for Hyprland, built as a TUI project like tuxwire and zerohour.
Work in progress: the collector and storage are done. The TUI, categories, exports and
the Zen extension come in later steps.

## What it does

`timesink-daemon` watches which window is focused and records how long each one stays
focused, in a local SQLite database. Only the focused window counts, so a terminal sitting
beside your browser gets no time while you type in the browser.

Time stops counting when you are away:

- **Idle**: no keyboard or mouse input for 5 minutes (configurable). Time is cut back to the
  moment of your last input, not the moment idle was noticed.
- **Playing media**: audio or video that is playing counts as present, so idle does not
  cut off time spent watching. Videos that hold the screen awake are covered by the
  compositor; music players are detected through MPRIS.
- **Suspend and lock**: time stops at suspend or lock (`loginctl lock-session`). A screen
  locker started without logind is not seen directly; the idle timer covers it.

Back-to-back intervals of the same window within 2 seconds are merged into one.

## Privacy defaults

- **Browsers** (Zen, Firefox, Chromium family): only the app is recorded, never the page title.
- **Terminals**: the command line is never stored (it can contain passwords or tokens).
  Only the program name and directory are kept, e.g. `fish @ ~/Projects`.
- **SSH**: stored as `ssh: <label>`. Labels come from `~/.config/timesink/hosts.conf`:

  ```
  # host or IP = label shown in timesink
  192.0.2.10 = proxmox
  ```

  Unlisted hosts are stored as `ssh: other`. No IP addresses or user names are recorded.
- **Other apps**: the window title is stored as-is.

## Settings

`~/.config/timesink/config.conf` (optional):

```
# minutes without input before time stops counting (whole minutes, minimum 1)
idle_timeout_minutes = 5
```

## Run it

Run it as a background service so closing a terminal can never stop it:

```fish
cargo install --path crates/timesink-daemon
mkdir -p ~/.config/systemd/user
cp contrib/timesink-daemon.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now timesink-daemon
journalctl --user -u timesink-daemon -f     # logs
```

Or run it by hand while testing:

```fish
cargo build --release
./target/release/timesink-daemon run -v     # record; -v prints each focus change
./target/release/timesink-daemon report     # totals for the last 24h and recent intervals
```

The daemon finds Hyprland and the Wayland display by itself, waits for them if they are not
up yet, and reconnects if they go away. The database is
`~/.local/share/timesink/timesink.db`; set `TIMESINK_DB` to use a different file.

## Layout

- `crates/timesink-core`: data model, title parsing, privacy rules, presence, tracker, SQLite storage.
- `crates/timesink-daemon`: Hyprland, idle, logind and media watchers plus the `run` / `report` commands.
- `contrib/`: the systemd user service file.

## Checks

```fish
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
```
