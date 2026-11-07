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

Only the app is recorded, with a few deliberate exceptions:

- **Browsers** (Zen, Firefox, Chromium family): only the app, never the page title.
- **Other apps** (chat, email, notes, file manager, ...): only the app. Channel names,
  inboxes, note names and folder names are never stored.
- **Terminals**: only the program that is running, e.g. `fish`, `tuxwire`, `zerohour`. The
  command line is never stored (it can contain passwords or tokens).
- **SSH**: stored as `ssh: <label>`. Labels come from `~/.config/timesink/hosts.conf`:

  ```
  # host or IP = label shown in timesink
  192.0.2.10 = proxmox
  ```

  Unlisted hosts are stored as `ssh: other`. No IP addresses or user names are recorded.

## Settings

`~/.config/timesink/config.conf` (optional):

```
# minutes without input before time stops counting (whole minutes, minimum 1)
idle_timeout_minutes = 5

# how much of a terminal window to record: app, program (default) or program_dir
terminal_detail = program

# window classes (comma separated) allowed to record full titles. Empty by default.
keep_titles =
```

## Cleaning up older data

Rows recorded before these privacy rules (for example Discord channel names) can be cleaned
with one command. It writes a backup first, then removes the stored titles the current
settings would no longer record and merges the rows that become identical:

```fish
systemctl --user stop timesink-daemon
timesink-daemon scrub
systemctl --user start timesink-daemon
```

The backup is `timesink.db.bak-before-scrub-<time>` next to the database.

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

## Friendly names and categories

The database stores each application's raw window class, for example
`org.wezfurlong.wezterm`. To show short names and group time into
categories, copy `contrib/apps.conf.example` to
`~/.config/timesink/apps.conf` and edit it:

```
org.wezfurlong.wezterm   wezterm   Terminal
io.github.lgse.Strata*   strata    Files
```

- Fields are: raw class, display name, category (the category may contain spaces).
- A raw class ending in `*` matches everything that starts with it.
- Matching ignores upper/lower case.
- Apps that are not listed keep their raw name and appear under "Other".

Names are applied only when shown, so you can change the file at any time and
all of your history is re-sorted. `timesink-daemon apps` lists every app seen
so far and marks the ones still missing from the file; `timesink-daemon report`
shows totals by category and by application.

## Listening time

While any media player is playing (anything that speaks MPRIS: Cider, mpv,
browsers and so on), timesink also records *listening time*. It is kept apart
from window focus, because music does not change which window you are using,
so `report` lists it separately and never adds it to the totals above it.

Only the player's short name is stored (for example `cider`), never track,
artist or video titles. If the machine sleeps, listening stops at that moment.
Playing media still counts you as present when you are away from the keyboard.

This needs database layout version 2. The first start after upgrading copies
your existing database to `timesink.db.bak-v1` before changing anything.

## The terminal interface

`timesink` (installed next to `timesink-daemon`) shows where your time went.
It only reads the database, so it is safe to run while the daemon records.

```
cargo install --path crates/timesink-tui
timesink
```

Three views: **Today** (category and application bars, active time by hour,
a coloured timeline of the day and listening time), **7 days** and **30 days**
(a day-by-hour heatmap plus the same category and application bars). Category
colours follow the order in which categories first appear in `apps.conf`
(12 distinct colours; later ones repeat darker), so they stay the same on every
screen. Put your most-used categories first to keep them easy to tell apart.
The Categories and Applications lists can be sorted by time or by name (`s`).

| Key | Action |
| --- | --- |
| `←` `→`, `Tab`, `h` `l` | previous / next view |
| `1` `2` `3` | Today / 7 days / 30 days |
| `s` | sort the lists by time or by name |
| `r` | refresh now (it also refreshes every 5 seconds) |
| `?` | help |
| `q`, `Esc` | quit |
