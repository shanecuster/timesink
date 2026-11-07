// timesink sites: reports the HOST NAME of the active tab to the local helper program.
//
// What is sent, and nothing else:   { "host": "gist.github.com" }   or   { "host": null }
//   - never the path, query string, page title or page content
//   - null whenever nothing trackable is in front: the browser is not the focused window,
//     a private window is focused (this extension does not run there at all), or the tab
//     is an internal page (about:, moz-extension:, file: ...)
// The helper (timesink-zen-host) reduces the host to the main site, applies your block
// list and stores it locally. There is no network access in this extension.

"use strict";

// The helper's registered name (see `timesink-zen-host install`).
const HOST = "timesink_zen";

// How often to repeat the current state. The helper treats a gap of 90s or more as "the
// browser went away", so 30s keeps a stretch alive through a couple of missed beats.
const HEARTBEAT_MS = 30 * 1000;

// The open connection to the helper, or null when it is not connected.
let port = null;

// Connect to the helper if we are not already. Returns the port or null on failure.
function connect() {
  if (port) return port;
  try {
    port = browser.runtime.connectNative(HOST);
    // If the helper exits or is not installed, forget the port; the next report retries.
    port.onDisconnect.addListener(() => {
      const err = port && port.error ? port.error.message : browser.runtime.lastError;
      console.warn("timesink: helper disconnected", err || "");
      port = null;
    });
  } catch (e) {
    console.warn("timesink: cannot start helper", e);
    port = null;
  }
  return port;
}

// The host name of the active tab in the focused normal window, or null.
async function currentHost() {
  // Only normal browser windows count (not popups or devtools).
  const win = await browser.windows.getLastFocused({
    populate: true,
    windowTypes: ["normal"],
  });
  // Not focused: you are in another application (or a private window we cannot see).
  if (!win || !win.focused || win.incognito) return null;
  const tab = (win.tabs || []).find((t) => t.active);
  if (!tab || !tab.url) return null;
  let url;
  try {
    url = new URL(tab.url);
  } catch (e) {
    return null;
  }
  // Only ordinary web pages. Everything else (about:, file:, extension pages) is ignored.
  if (url.protocol !== "http:" && url.protocol !== "https:") return null;
  // Only the host name leaves this function.
  return url.hostname;
}

// Work out the current host and send it to the helper.
async function report() {
  let host = null;
  try {
    host = await currentHost();
  } catch (e) {
    host = null; // when in doubt, report "nothing", never a stale site
  }
  const p = connect();
  if (p) p.postMessage({ host });
}

// Report whenever what is in front could have changed...
browser.tabs.onActivated.addListener(report);
browser.tabs.onUpdated.addListener((id, change, tab) => {
  if (tab.active && (change.url || change.status === "complete")) report();
});
browser.windows.onFocusChanged.addListener(report);
// ...and repeat the current state regularly so a long stay on one site keeps counting.
setInterval(report, HEARTBEAT_MS);
report();
