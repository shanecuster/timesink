# timesink

A screen-time monitor for Hyprland, built as a TUI project like tuxwire and zerohour.
Work in progress: step 1 is the collector and storage only. The TUI, idle handling,
categories, exports and the Zen extension come in later steps.

## What step 1 does

`timesink-daemon` watches which window is focused and records how long each one stays
focused, in a local SQLite database. Only the focused window counts, so a terminal sitting
beside your browser gets no time while you type in the browser.

## Privacy defaults

- **Browsers** (Zen, Firefox, Chromium family): only the app is recorded, never the page title.
- **Terminals**: the command line is never stored (it can contain passwords or tokens).
  Only the program and directory are kept, e.g. `fish @ ~/Projects`.
- **SSH**: stored as `ssh: <label>`. Labels come from `~/.config/timesink/hosts.conf`:

  ```
  # host or IP = label shown in timesink
  192.0.2.10 = proxmox
  ```

  Unlisted hosts are stored as `ssh: other`. No IP addresses or user names are recorded.
- **Other apps**: the window title is stored as-is.

## Run it

```fish
cargo build --release
./target/release/timesink-daemon run -v     # record; -v prints each focus change
./target/release/timesink-daemon report     # totals for the last 24h and recent intervals
```

Run the daemon inside a Hyprland session (it needs `HYPRLAND_INSTANCE_SIGNATURE`).
The database is `~/.local/share/timesink/timesink.db`. Set `TIMESINK_DB` to use a
different file while testing.

## Layout

- `crates/timesink-core`: data model, title parsing, privacy rules, tracker, SQLite storage.
- `crates/timesink-daemon`: Hyprland socket client and the `run` / `report` commands.

## Checks

```fish
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
```
