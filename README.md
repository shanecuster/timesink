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

# yes: no input for the idle time stops focus time even while media is playing
# (listening time still counts in full). no (default): playing media keeps you counted.
idle_ignores_media = no

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

- `crates/timesink-core`: data model, title parsing, privacy rules, presence, tracker, SQLite storage, and the views, periods and day totals every screen is built from. It has no terminal code, so the Android app can reuse it.
- `crates/timesink-sync`: CouchDB sync (the logic and its HTTP client), also shared with the Android app.
- `crates/timesink-daemon`: Hyprland, idle, logind and media watchers plus the `run` / `report` commands.
- `contrib/`: the systemd user service and sync timer files, and example config files.

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

Well-known browsers (Firefox, Zen, Chromium, Chrome, Brave, Vivaldi, Edge and a few more) that have no line in `apps.conf` are shown under **Browser** instead of Other. A line of your own always wins, so you can still give a browser another name or category.

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
By default, playing media still counts you as present when you are away from the keyboard. Set `idle_ignores_media = yes` in `config.conf` to stop focus time at the idle timeout even while something plays; listening time is unaffected.

This needs database layout version 2. The first start after upgrading copies
your existing database to `timesink.db.bak-v1` before changing anything.

## The terminal interface

`timesink` (installed next to `timesink-daemon`) shows where your time went.
It only reads the database, so it is safe to run while the daemon records.

```
cargo install --path crates/timesink-tui
timesink
```

Eight views (a Sites panel is added when the optional browser extension has recorded website time): **Today** (category and application bars, active time by hour,
a coloured timeline of the day and listening time), **7 days** and **30 days**
(a day-by-hour heatmap plus the same category and application bars), **Year**
(the last 365 days) and **All time** (from the first day recorded). Year and All
time sum up by calendar month instead of by day, with the number of active days
and the average per active day in the panel title. **Cal. week** is the calendar week
(Monday to Sunday, up to today) and **Quarter** is the calendar quarter (Jan-Mar,
Apr-Jun, Jul-Sep, Oct-Dec, up to today); the quarter sums by month like the year. `[` steps
any view back one period (the day before, the 7 days before, last week, last quarter...) and `]`
steps forward again; the title names the days you are looking at. **Custom** shows two
dates you type (`c`, or `8` the first time): `2026-10-01 2026-10-31`, or one date for "from
then until today". A custom range of more than a month sums by month; stepping moves it by
its own length. The range is not remembered between runs. Stepping stops at
the present and at the first recorded day. Category
colours follow the order in which categories first appear in `apps.conf`
(the current theme's own accents, then deeper and lighter copies), so they stay the same on every
screen. Put your most-used categories first to keep them easy to tell apart.
The Categories and Applications lists can be sorted by time or by name (`s`).

| Key | Action |
| --- | --- |
| `←` `→`, `Tab`, `h` `l` | previous / next view |
| `1` to `8` | Today / 7 days / 30 days / Year / All time / Cal. week / Quarter / Custom |
| `c` | type the first and last day of the Custom view |
| `[` `]` | step back / forward one period (a day, a week, a quarter, ...) |
| `s` | sort the lists by time or by name |
| `a` | rename an app or move it to another category (or a new one); saved to `apps.conf` |
| `t` | next colour theme |
| `b` | show / hide the title banner |
| `p` | choose which list the scroll keys move (Applications, Categories, Sites, or the month table) |
| `↑` `↓`, `j` `k`, `PgUp` `PgDn`, `Home` `End` | scroll that list when it holds more rows than fit |
| `e` | export the view you are looking at (then `c` csv, `j` json, `m` markdown, `h` html, `Esc` cancel) |
| `d` | show all computers combined, or one computer at a time (when more than one has synced) |
| `S` | sync with CouchDB now, in the background |
| `r` | refresh now (it also refreshes every 5 seconds) |
| `?` | help |
| `q`, `Esc` | quit |

### Editing names and categories from the interface

`a` opens a small editor: pick an application, confirm or change its short name,
then pick a category or type a new one. The choice is appended as a line to your
`apps.conf` (a later line replaces an earlier one, so your hand-written file, its
comments and its order are never rewritten) and the screen updates at once. A new
category goes to the end of the file, so it takes the next free bar colour.

### Long lists

Lists show as many rows as fit. When some are hidden, the panel title says so
(`↓3` means three more below, `↑` means rows above) and the list the scroll keys
control gets an accent-coloured border. With short lists nothing changes. The
month table in the Year and All time views lists the newest month first and
scrolls down to older ones.

### Title banner and themes

The top of the screen shows the name in the middle, today's full date with the
ISO week number at the left, and a small chart of how busy each hour of today
was against the right edge (the current hour is underlined). On terminals 32 rows or taller the name is drawn large; between
24 and 31 rows it is the small spaced-out name; below 24 rows the banner hides
itself. The chart drops out rather than touch the name when the window is too narrow
for it (the big title needs about 100 columns for a one-column-per-hour chart,
about 145 for two); `b` turns it off.

`t` cycles through eight themes that match tuxwire and zerohour: Everforest
(the default), Dracula, Gruvbox, Catppuccin Mocha, Nord, Solarized Dark, Tokyo
Night and Catppuccin Macchiato. Themes change the text, borders, background
and highlights, and the bar colours too: each theme has its own accent colours
for the categories (extra ones are deeper or lighter copies). Within a theme a
category keeps its colour, and a test checks that the first twelve are clearly
different, so neighbouring bars never match. Your theme, sort order and banner choice are
remembered in `~/.config/timesink/tui.conf` (a separate file from
`config.conf`, so saving a choice never rewrites your hand-written settings).
You can also edit it by hand:

```
theme = Nord
sort = name
banner = on
```

## Website time (optional Zen / Firefox extension)

Off by default. Without the extension timesink records only *which application*
was in front, never what was in it. With it, you also get time per website, as
the **main domain only** (`gist.github.com` is recorded as `github.com`). The
page path, query, title and content are never read, sent or stored.

How it stays private and accurate:

- The extension sends only the active tab's host name, every 30 seconds and when
  the tab or window changes. It has no network access.
- It does not run in private windows at all (and reports "nothing" while one is
  in front).
- A small helper program (`timesink-zen-host`) receives the host name through
  the browser's native-messaging channel (no open port), reduces it to the main
  site, drops anything on your block list and writes it to the local database.
- Site time counts only while the browser was the focused, non-idle window, so
  background tabs, idle time and other apps never add up. It is shown in a
  separate **Sites** panel, not added to the totals.

Set it up:

```
cargo install --path crates/timesink-zen          # installs timesink-zen-host
timesink-zen-host install                         # registers it with Zen and Firefox
cp contrib/domains.conf.example ~/.config/timesink/domains.conf   # optional
```

Then load the extension from the `extension/` folder:

- To try it: open `about:debugging#/runtime/this-firefox`, choose **Load
  Temporary Add-on** and pick `extension/manifest.json`. It lasts until the
  browser restarts.
- To keep it (recommended): sign it privately and install the `.xpi`. Release
  builds of Zen and Firefox only keep signed add-ons. Signing is free and
  *unlisted*: Mozilla signs the file and hands it back; nothing is published.
  1. Make API keys at <https://addons.mozilla.org/developers/addon/api/key/>
     and put them in your environment, never in the repo:
     `set -Ux WEB_EXT_API_KEY user:...` and `set -Ux WEB_EXT_API_SECRET ...` (fish).
  2. `sh contrib/sign-extension.sh` (needs node/npm). The signed file lands in
     `web-ext-artifacts/`.
  3. In Zen: `about:addons`, gear menu, **Install Add-on From File...**, pick the `.xpi`.
  4. Repeat step 3 in Firefox if you want it there too (the same file works).
  Each new signing needs a higher `version` in `extension/manifest.json`.

Zen and Firefox each start their own helper, so both can run the extension at
once. `browser = zen, firefox` (the default) is what counts site time against.

If site time ever stops recording, run `timesink-zen-host status`: it shows
whether the browser registrations exist and still point at the installed
program, which browsers are counted, and when the last report arrived. The usual
causes are a temporary add-on that was lost on restart, the extension being
disabled in `about:addons`, or the helper having moved (run `install` again).

`domains.conf` (see `contrib/domains.conf.example`) holds the browser's window
class(es) (`browser = zen, firefox`), sites to never record (`block = site`) and friendly
names and categories for sites, in the same layout as `apps.conf`.

Delete what was stored: `timesink-zen-host forget --all`, or
`timesink-zen-host forget github.com` for one site. `timesink-zen-host uninstall`
removes the registration. This part was tested end to end against the helper and
with a simulated browser; check it in your own Zen before relying on it, since the
exact add-on loading steps can differ between Zen versions.

## Sandbox (try things on made-up data)

To test the interface, themes, exports or screenshots without touching your real
data, use the sandbox:

```
sh contrib/sandbox.sh          # opens timesink on 35 days of made-up data
sh contrib/sandbox.sh shell    # a shell inside the sandbox (try `timesink export`)
sh contrib/sandbox.sh reset    # throw it away; a fresh one is made next time
```

(From fish, run the same lines; the script is plain `sh`.) The sandbox is one
folder (`~/.cache/timesink-sandbox`, or `$TIMESINK_SANDBOX_DIR`) with its own
database, settings, theme choice, `apps.conf` and `domains.conf` copied from the
examples, and its own export folder. The real daemon is never involved and your
real files are not read or written. `timesink demo-data` itself refuses to run
unless the script has set its marker, so it cannot fill your real database.
The days always end today, so reset to refresh them.

## Exports and backups

```
timesink export                          # a week as an HTML page
timesink export -f csv -p month          # a month of every recorded row
timesink export -f json --days 90        # the last 90 days
timesink export -f md -p today -o notes/ # into a folder
timesink backup                          # a dated copy of the database
```

- **Formats:** `csv` and `json` include every recorded row (focus and
  listening, with start, end, seconds, app, category, raw class and the cleaned
  detail). `md` and `html` are summaries: categories, applications, days and
  listening. The HTML page is one self-contained file with no scripts.
- **Periods:** `-p today|week|month|year|all`, or `--days N` (1 to 3650), or exact days with `--from YYYY-MM-DD [--to YYYY-MM-DD]` (not combined with a period). Pressing `e` in the interface exports the days on screen, including a stepped-back period. `all` runs from the first recorded day, however long ago.
- **Where:** `~/Documents/timesink-exports`, or `-o PATH`. A path with a file
  extension is a file; anything else is a folder (created if needed).
- **Nothing is overwritten.** A second export on the same day becomes
  `...-2.csv`, `...-3.csv`. An explicit file that already exists needs `--force`.
- **Privacy:** exports contain only what the database holds: app names,
  categories and the cleaned detail (program names, `ssh: label`), never page,
  channel or note titles. Spreadsheet formula characters at the start of a CSV
  field are defused with a leading apostrophe.
- **Backups:** `timesink backup` writes a consistent copy with SQLite's
  `VACUUM INTO`, which is safe while the daemon is recording. To restore, stop
  the service and copy the backup over `~/.local/share/timesink/timesink.db`.

## Syncing between computers (CouchDB)

timesink can share its data between computers (mangrove, a laptop, a Steam Deck)
through a CouchDB database you run. Each computer uploads only its own rows and
downloads everyone else's, so nothing can conflict.

1. Create an empty database on the server, then copy `contrib/sync.conf.example` to
   `~/.config/timesink/sync.conf`, fill in `url`, `database`, `user` and `password`,
   and run `chmod 600 ~/.config/timesink/sync.conf`.
2. `timesink sync` uploads the days that changed and downloads the other
   computers' days. `timesink sync status` shows what is configured and what is
   waiting, without contacting the server.
3. Press `S` in the interface to sync in the background, or sync automatically with
   the systemd user timer in `contrib/` (every 5 minutes, starting 2 minutes after
   login). It is off until you enable it, on each computer you want to sync:

   ```
   mkdir -p ~/.config/systemd/user
   cp contrib/timesink-sync.service contrib/timesink-sync.timer ~/.config/systemd/user/
   systemctl --user daemon-reload
   systemctl --user enable --now timesink-sync.timer
   ```

   `journalctl --user -u timesink-sync -n 20` shows the latest runs, and
   `systemctl --user disable --now timesink-sync.timer` turns it off. A run that cannot
   reach the server (say the computer is off the tailnet) just fails and tries again
   at the next interval; nothing is lost.
4. The footer of the interface shows how the last sync went ("synced 2m ago", or "sync failed 3m ago" if the latest try did not work), whoever ran it: the timer, `S` or the command line.
5. In the interface, `d` switches between all computers combined (the default; time
   that overlaps across computers is added up) and each computer alone.

What is shared: one CouchDB document per computer per UTC day, holding app focus
intervals, listening and website time. **Window titles are never uploaded**; rows
downloaded from other computers have empty titles. Each computer also shares its
friendly names and categories (the text of `apps.conf` and the site lines of
`domains.conf`, never the `browser =` and `block =` settings) as one small document
`conf:<computer>`. Your own files are never rewritten; every computer merges all
the lists, and for the same app the list changed most recently wins. Your category
colours keep your own order. Forgetting data on one computer removes its documents from the
server, so it disappears from the others too. The database is backed up before the
first run with the new format (`timesink.db.bak-v4`); take a restic backup before the
first real sync.
