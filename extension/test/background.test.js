// Run with:  node extension/test/background.test.js
// Loads background.js against a pretend `browser` object and checks what would be sent to
// the helper. The point: only the HOST NAME ever leaves, and "nothing" is reported whenever
// the browser is not in front.
"use strict";
const fs = require("fs");
const path = require("path");
const vm = require("vm");
const assert = require("assert");

// Run background.js once with a given pretend window and return the messages it posted.
async function run(win) {
  const sent = [];
  const listeners = [];
  const browser = {
    runtime: {
      connectNative: () => ({
        postMessage: (m) => sent.push(m),
        onDisconnect: { addListener() {} },
      }),
      lastError: null,
    },
    windows: {
      getLastFocused: async () => win,
      onFocusChanged: { addListener: (f) => listeners.push(f) },
    },
    tabs: {
      onActivated: { addListener() {} },
      onUpdated: { addListener() {} },
    },
  };
  const code = fs.readFileSync(path.join(__dirname, "..", "background.js"), "utf8");
  vm.runInNewContext(code, { browser, console, URL, setInterval: () => 0 });
  await new Promise((r) => setTimeout(r, 20)); // let the first async report finish
  // Objects made inside the vm context have a different prototype; compare plain copies.
  return JSON.parse(JSON.stringify(sent));
}

(async () => {
  const tab = (url, active = true) => ({ url, active, title: "SECRET TITLE" });

  // A normal focused window: only the host goes out, never path, query or title.
  let sent = await run({
    focused: true,
    incognito: false,
    tabs: [tab("https://other.example/", false), tab("https://gist.github.com/me/secret?token=abc#frag")],
  });
  assert.deepStrictEqual(sent, [{ host: "gist.github.com" }]);
  assert.ok(!JSON.stringify(sent).includes("secret"));

  // Browser not focused (you are in another app): report nothing.
  sent = await run({ focused: false, incognito: false, tabs: [tab("https://github.com/")] });
  assert.deepStrictEqual(sent, [{ host: null }]);

  // Private window: nothing.
  sent = await run({ focused: true, incognito: true, tabs: [tab("https://github.com/")] });
  assert.deepStrictEqual(sent, [{ host: null }]);

  // Internal pages and files: nothing.
  for (const url of ["about:preferences", "moz-extension://abc/page.html", "file:///home/me/x.txt"]) {
    sent = await run({ focused: true, incognito: false, tabs: [tab(url)] });
    assert.deepStrictEqual(sent, [{ host: null }], url);
  }
  console.log("extension tests passed");
})().catch((e) => {
  console.error(e);
  process.exit(1);
});
