#!/bin/sh
# timesink sandbox: try the interface, themes and exports on made-up data, without touching
# your real database, settings or exports.
#
#   sh contrib/sandbox.sh              open the interface on demo data (made on first use)
#   sh contrib/sandbox.sh reset        throw the sandbox away (a fresh one is made next time)
#   sh contrib/sandbox.sh shell        a shell inside the sandbox (run `timesink export` etc.)
#   sh contrib/sandbox.sh run CMD...   run one command inside the sandbox
#
# Works from fish as well: `sh contrib/sandbox.sh`. Needs `timesink` installed
# (cargo install --path crates/timesink-tui).
#
# Everything lives in one folder (default ~/.cache/timesink-sandbox, or $TIMESINK_SANDBOX_DIR):
# its own HOME, database, settings (theme, sort) and export folder. The real daemon is never
# involved.

set -eu

# Where this script lives, so the example config files can be found.
HERE=$(cd "$(dirname "$0")" && pwd)
SANDBOX=${TIMESINK_SANDBOX_DIR:-$HOME/.cache/timesink-sandbox}

# Find the program BEFORE changing HOME (so cargo's bin folder is still on the PATH).
TIMESINK=$(command -v timesink || true)
if [ -z "$TIMESINK" ] && [ "${1:-}" != reset ]; then
    echo "timesink is not installed: run  cargo install --path crates/timesink-tui" >&2
    exit 1
fi

# `reset` deletes the folder, so be strict: only a folder this script made (it holds a
# marker file), and never an empty, root or home path.
if [ "${1:-}" = reset ]; then
    case "$SANDBOX" in
        "" | / | "$HOME") echo "refusing to delete '$SANDBOX'" >&2; exit 1 ;;
    esac
    if [ -e "$SANDBOX" ] && [ ! -e "$SANDBOX/.timesink-sandbox" ]; then
        echo "refusing to delete $SANDBOX: it was not made by this script" >&2
        exit 1
    fi
    rm -rf "$SANDBOX"
    echo "sandbox removed ($SANDBOX)"
    exit 0
fi

# Build the folder layout and mark it as ours.
mkdir -p "$SANDBOX/home" "$SANDBOX/config/timesink" "$SANDBOX/data"
: > "$SANDBOX/.timesink-sandbox"

# Point everything at the sandbox. TIMESINK_SANDBOX=1 is the marker that allows demo data.
export HOME="$SANDBOX/home"
export XDG_CONFIG_HOME="$SANDBOX/config"
export XDG_DATA_HOME="$SANDBOX/data"
export TIMESINK_DB="$SANDBOX/data/timesink.db"
export TIMESINK_DEVICE=sandbox
export TIMESINK_SANDBOX=1

# Example settings on first use, so names, categories and sites look right.
[ -e "$XDG_CONFIG_HOME/timesink/apps.conf" ] || cp "$HERE/apps.conf.example" "$XDG_CONFIG_HOME/timesink/apps.conf"
[ -e "$XDG_CONFIG_HOME/timesink/domains.conf" ] || cp "$HERE/domains.conf.example" "$XDG_CONFIG_HOME/timesink/domains.conf"

# Demo data on first use.
if [ ! -e "$TIMESINK_DB" ]; then
    "$TIMESINK" demo-data --days "${TIMESINK_SANDBOX_DAYS:-35}"
fi

echo "== timesink SANDBOX: made-up data in $SANDBOX; your real data is not touched ==" >&2

case "${1:-}" in
    "") exec "$TIMESINK" ;;
    shell) exec "${SHELL:-/bin/sh}" ;;
    run) shift; exec "$@" ;;
    *) echo "unknown command '$1' (try: reset, shell, run)" >&2; exit 1 ;;
esac
