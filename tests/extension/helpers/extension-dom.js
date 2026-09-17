"use strict";

/**
 * Loads the real, shipped extension files into a jsdom window.
 *
 * `extension/content.js` and `extension/popup.js` touch the `browser` global and
 * the DOM at load, so they cannot be `require()`d the way `hls.js`,
 * `media-scan.js`, and `job-view.js` can. They can, however, be evaluated inside
 * a window that already has a document and a stubbed `browser` — which is what
 * this does. Nothing here re-implements or copies extension code: the files
 * under test are read from `extension/` exactly as they ship.
 *
 * This is not Firefox. It does not exercise real WebExtension APIs, real
 * content-script injection, or native messaging over a real port.
 */

const fs = require("node:fs");
const path = require("node:path");
const { JSDOM, VirtualConsole } = require("jsdom");

const EXTENSION_DIR = path.join(__dirname, "..", "..", "..", "extension");
const PAGES_DIR = path.join(__dirname, "..", "..", "fixtures", "pages");

function extensionSource(file) {
  return fs.readFileSync(path.join(EXTENSION_DIR, file), "utf8");
}

function pageFixture(name) {
  return fs.readFileSync(path.join(PAGES_DIR, `${name}.html`), "utf8");
}

/** Surface page errors instead of letting jsdom swallow them. */
function strictConsole() {
  const virtualConsole = new VirtualConsole();
  virtualConsole.on("jsdomError", (error) => {
    throw error;
  });
  return virtualConsole;
}

/**
 * Copy a value out of the jsdom realm. Objects created inside the window have
 * that realm's prototypes, so Node's strict deep-equality rejects them even when
 * the structure matches. Firefox serializes every `runtime`/`tabs` message
 * anyway, so taking a plain copy across that boundary is faithful, not a fudge.
 */
function plain(value) {
  return value === undefined ? undefined : JSON.parse(JSON.stringify(value));
}

/**
 * Let the page's async work finish before asserting. Each `setImmediate` turn
 * fully drains the microtask queue, so a chain of awaits (the popup's
 * `scanActiveTab` -> `tabs.query` -> `tabs.sendMessage` ->
 * `restoreDownloadStatuses` -> `runtime.sendMessage`) completes rather than
 * being caught half-done.
 */
async function settle(turns = 8) {
  for (let i = 0; i < turns; i += 1) {
    await new Promise((resolve) => setImmediate(resolve));
  }
}

/**
 * A minimal stand-in for the `browser` global. Only the surface the extension
 * actually calls is provided; anything else is left undefined on purpose, so a
 * new API call shows up as a test failure rather than passing silently.
 */
function browserStub({ onMessage = async () => undefined, tabs = {} } = {}) {
  const sent = [];
  const listeners = [];
  return {
    sent,
    listeners,
    api: {
      runtime: {
        lastError: null,
        onMessage: { addListener: (listener) => listeners.push(listener) },
        sendMessage: (message) => {
          sent.push(message);
          return Promise.resolve(onMessage(message));
        }
      },
      tabs: {
        query: async () => [tabs.active || { id: 1, url: "https://example.test/files/index.html" }],
        sendMessage: async (tabId, message) => {
          sent.push({ tabId, ...message });
          return tabs.onMessage ? tabs.onMessage(message) : undefined;
        }
      }
    }
  };
}

/**
 * Load the content script over a page fixture and return a `scanMedia` driver
 * that goes through the real `scan-media` message listener, exactly as the
 * popup reaches it.
 */
function loadContentScript(fixtureName, {
  url = "https://example.test/files/index.html",
  resourceEntries = []
} = {}) {
  const dom = new JSDOM(pageFixture(fixtureName), {
    url,
    runScripts: "outside-only",
    virtualConsole: strictConsole()
  });
  const stub = browserStub();
  dom.window.browser = stub.api;
  // jsdom implements `performance` but not the Resource Timing API, which
  // Firefox does. `scanMedia` reads it for URLs the page actually fetched, so
  // the harness supplies it; tests pass entries to exercise that pass.
  dom.window.performance.getEntriesByType = (type) =>
    type === "resource" ? resourceEntries.map((name) => ({ name })) : [];
  // Loaded in manifest order: media-scan.js, then content.js.
  dom.window.eval(extensionSource("media-scan.js"));
  dom.window.eval(extensionSource("content.js"));

  const listener = stub.listeners[0];
  if (!listener) throw new Error("content.js registered no runtime.onMessage listener");
  return {
    dom,
    /** Drive the real listener the popup uses. */
    async scanMedia() {
      return plain(await listener({ type: "scan-media" }));
    }
  };
}

/**
 * Load `popup.html` with the real `job-view.js` and `popup.js`, wired to a
 * stubbed background. Returns the window plus readers for what the user sees.
 */
async function loadPopup({ candidates = [], sourceUrl = "https://example.test/files/index.html", jobs = [], sessionJobIds = [], tabUrl = sourceUrl } = {}) {
  const dom = new JSDOM(extensionSource("popup.html"), {
    url: "moz-extension://downer-test/popup.html",
    runScripts: "outside-only",
    resources: undefined,
    virtualConsole: strictConsole()
  });

  const stub = browserStub({
    onMessage: (message) => {
      if (message?.type === "get-download-statuses") return { jobs, sessionJobIds };
      if (message?.type === "control-download") return { ok: true };
      if (message?.type === "download-media") return { ok: true, jobId: "new-job", state: "starting" };
      return undefined;
    },
    tabs: {
      active: { id: 1, url: tabUrl },
      onMessage: (message) =>
        message?.type === "scan-media" ? { sourceUrl, title: "fixture", candidates } : undefined
    }
  });
  dom.window.browser = stub.api;
  // Loaded in popup.html order: job-state.js, job-view.js, popup.js.
  dom.window.eval(extensionSource("job-state.js"));
  dom.window.eval(extensionSource("job-view.js"));
  dom.window.eval(extensionSource("popup.js"));
  await settle();

  const document = dom.window.document;
  const text = (id) => document.getElementById(id).textContent;

  return {
    dom,
    sent: stub.sent,
    /** The popup's headline status line. */
    status: () => text("status"),
    /** The secondary line under the list. */
    downloadStatus: () => text("download-status"),
    /** One entry per listed media candidate, as the user sees it. */
    rows: () =>
      [...document.querySelectorAll("#media-list li")].map((item) => {
        const buttons = [...item.querySelectorAll("button")];
        const [download, pause, resume, cancel] = buttons;
        return {
          label: item.querySelector(".url").textContent,
          count: item.querySelector(".segment-count").textContent,
          download: {
            text: download.textContent,
            disabled: download.disabled,
            jobId: download.dataset.jobId
          },
          pause: { hidden: pause.hidden, disabled: pause.disabled },
          resume: { hidden: resume.hidden, disabled: resume.disabled },
          cancel: { hidden: cancel.hidden, disabled: cancel.disabled }
        };
      }),
    settle
  };
}

/**
 * Load the real background script with a stubbed `browser`, simulating a browser
 * start: `storage.local` already holds `downloadJobs` from a previous session.
 *
 * Returns the storage the script sees, so a test can assert what was persisted
 * back, plus a driver for the `runtime.onMessage` handler the popup talks to.
 */
async function loadBackground({ downloadJobs = [] } = {}) {
  const dom = new JSDOM("<!doctype html><html><body></body></html>", {
    url: "moz-extension://downer-test/background.html",
    runScripts: "outside-only",
    virtualConsole: strictConsole()
  });

  const storage = { downloadJobs: JSON.parse(JSON.stringify(downloadJobs)) };
  const listeners = [];
  const warnings = [];
  dom.window.console.warn = (...args) => warnings.push(args.join(" "));
  dom.window.browser = {
    storage: {
      local: {
        get: async (defaults) => ({ ...defaults, ...storage }),
        set: async (values) => Object.assign(storage, JSON.parse(JSON.stringify(values)))
      }
    },
    runtime: {
      lastError: null,
      onMessage: { addListener: (listener) => listeners.push(listener) },
      sendMessage: async () => undefined
    },
    cookies: { getAll: async () => [] },
    notifications: { create: async () => undefined }
  };

  // Loaded in manifest order.
  for (const file of ["job-state.js", "task-protocol.js", "hls.js", "background.js"]) {
    dom.window.eval(extensionSource(file));
  }
  await settle();

  return {
    dom,
    warnings,
    /** What the background script has persisted back to storage.local. */
    stored: () => plain(storage.downloadJobs),
    /** Send a message as the popup would, and get the reply. */
    async send(message) {
      for (const listener of listeners) {
        const reply = await listener(message);
        if (reply !== undefined) return plain(reply);
      }
      return undefined;
    }
  };
}

module.exports = {
  loadContentScript,
  loadPopup,
  loadBackground,
  pageFixture,
  extensionSource,
  settle,
  plain
};
