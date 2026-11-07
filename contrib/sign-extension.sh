#!/bin/sh
# Sign the timesink extension privately ("unlisted") so Zen/Firefox keep it across restarts.
# Nothing is published: Mozilla only signs it and gives you a .xpi file for yourself.
#
# One-time: make API keys at https://addons.mozilla.org/developers/addon/api/key/
# then put them in your environment (NOT in this repo), e.g. in fish:
#   set -Ux WEB_EXT_API_KEY    user:12345:67
#   set -Ux WEB_EXT_API_SECRET abcdef...
#
# Run from the repo root:  sh contrib/sign-extension.sh
# Each signing needs a new "version" in extension/manifest.json (Mozilla refuses repeats).
set -eu

if [ -z "${WEB_EXT_API_KEY:-}" ] || [ -z "${WEB_EXT_API_SECRET:-}" ]; then
    echo "Set WEB_EXT_API_KEY and WEB_EXT_API_SECRET first (see the comments in this file)." >&2
    exit 1
fi
if [ ! -f extension/manifest.json ]; then
    echo "Run this from the timesink repo root." >&2
    exit 1
fi

npx --yes web-ext sign \
    --channel=unlisted \
    --source-dir extension \
    --artifacts-dir web-ext-artifacts \
    --ignore-files 'test/**'

echo
echo "Signed file(s):"
ls -1 web-ext-artifacts/*.xpi
echo "Install: open about:addons in Zen, gear menu > Install Add-on From File..."
