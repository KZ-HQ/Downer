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
 * Load `options.html` with the real `redact.js`, `job-logs.js` and `options.js`,
 * wired to a stubbed background. Returns readers for what the Settings page
 * shows and a `receive` driver for the messages the background script sends it.
 */
async function loadOptions({ jobs = [], logs = {}, settings = {} } = {}) {
  const dom = new JSDOM(extensionSource("options.html"), {
    url: "moz-extension://downer-test/options.html",
    runScripts: "outside-only",
    resources: undefined,
    virtualConsole: strictConsole()
  });

  const sent = [];
  const listeners = [];
  dom.window.browser = {
    storage: {
      local: {
        get: async (defaults) => ({ ...defaults, ...settings }),
        set: async () => undefined
      }
    },
    runtime: {
      lastError: null,
      onMessage: { addListener: (listener) => listeners.push(listener) },
      sendMessage: async (message) => {
        sent.push(plain(message));
        if (message?.type === "get-download-statuses") return { jobs, sessionJobIds: [] };
        if (message?.type === "get-download-logs") return { logs };
        if (message?.type === "clear-download-logs") return { ok: true };
        return undefined;
      }
    }
  };

  // Loaded in options.html order.
  for (const file of ["redact.js", "job-logs.js", "options.js"]) {
    dom.window.eval(extensionSource(file));
  }
  await settle();

  const document = dom.window.document;
  const filter = document.getElementById("job-filter");
  return {
    dom,
    sent,
    settle,
    /** The text of the log pane, exactly as the user reads it. */
    logText: () => document.getElementById("logs").textContent,
    /** The per-download filter's options, as `value: label` pairs. */
    filterOptions: () => [...filter.options].map((option) => `${option.value}: ${option.textContent}`),
    /** Choose a download in the filter, as the user would. */
    selectJob(jobId) {
      filter.value = jobId;
      filter.dispatchEvent(new dom.window.Event("change"));
    },
    /** Press "Clear logs". */
    clearLogs() {
      document.getElementById("clear-logs").dispatchEvent(new dom.window.Event("click"));
      return settle();
    },
    /** Deliver a message as the background script would broadcast it. */
    async receive(message) {
      for (const listener of listeners) await listener(message);
      await settle();
    }
  };
}

/**
 * A stand-in for a native messaging port, so a test can drive the real
 * `NativeTaskChannel` without a host process. It answers the `hello` handshake
 * exactly as `docs/protocol.md` specifies and then hands the test `emit`, which
 * delivers any event the host could send.
 */
function nativePortStub({ protocolVersion = 1 } = {}) {
  const messageListeners = [];
  const disconnectListeners = [];
  const posted = [];
  const emit = (event) => {
    for (const listener of [...messageListeners]) listener(event);
  };
  const port = {
    onMessage: { addListener: (listener) => messageListeners.push(listener) },
    onDisconnect: { addListener: (listener) => disconnectListeners.push(listener) },
    postMessage: (message) => {
      posted.push(message);
      if (message?.command === "hello") {
        queueMicrotask(() =>
          emit({
            protocol_version: protocolVersion,
            type: "hello",
            ok: true,
            state: "ready",
            request_id: message.request_id,
            host_version: "test",
            capabilities: { pause_resume: true, hls_info: true }
          })
        );
      }
    },
    disconnect: () => {
      for (const listener of [...disconnectListeners]) listener();
    }
  };
  return { port, posted, emit };
}

/**
 * Load the real background script with a stubbed `browser`, simulating a browser
 * start: `storage.local` already holds `downloadJobs` from a previous session.
 *
 * Returns the storage the script sees, so a test can assert what was persisted
 * back, plus a driver for the `runtime.onMessage` handler the popup talks to.
 * `storage` is the whole of `storage.local`, not just `downloadJobs`, because
 * logs live under their own `downloadLogs:<jobId>` keys (KEI-55); `writes()`
 * counts `set` calls, which is what the batching test asserts a bound on.
 */
async function loadBackground({ downloadJobs = [], storage: initialStorage = {}, native = false } = {}) {
  const dom = new JSDOM("<!doctype html><html><body></body></html>", {
    url: "moz-extension://downer-test/background.html",
    runScripts: "outside-only",
    virtualConsole: strictConsole()
  });

  const storage = JSON.parse(JSON.stringify({ downloadJobs, ...initialStorage }));
  const listeners = [];
  const warnings = [];
  const broadcasts = [];
  const ports = [];
  let writes = 0;
  dom.window.console.warn = (...args) => warnings.push(args.join(" "));
  dom.window.browser = {
    storage: {
      local: {
        // Firefox's `get` takes a defaults object, a key, an array of keys, or
        // null for everything. The background script asks for everything on
        // start, so it can find log keys left by jobs that have since fallen
        // off the end of the history.
        get: async (query) => {
          if (query === null || query === undefined) return JSON.parse(JSON.stringify(storage));
          if (typeof query === "string") return { [query]: storage[query] };
          if (Array.isArray(query)) {
            return Object.fromEntries(
              query.filter((key) => key in storage).map((key) => [key, storage[key]])
            );
          }
          return { ...query, ...storage };
        },
        set: async (values) => {
          writes += 1;
          Object.assign(storage, JSON.parse(JSON.stringify(values)));
        },
        remove: async (keys) => {
          for (const key of [].concat(keys)) delete storage[key];
        }
      }
    },
    runtime: {
      lastError: null,
      onMessage: { addListener: (listener) => listeners.push(listener) },
      sendMessage: async (message) => {
        broadcasts.push(plain(message));
        return undefined;
      },
      ...(native
        ? {
            connectNative: () => {
              const stub = nativePortStub();
              ports.push(stub);
              return stub.port;
            }
          }
        : {})
    },
    cookies: { getAll: async () => [] },
    notifications: { create: async () => undefined }
  };

  // Loaded in manifest order.
  for (const file of [
    "job-state.js",
    "redact.js",
    "job-logs.js",
    "task-protocol.js",
    "hls.js",
    "background.js"
  ]) {
    dom.window.eval(extensionSource(file));
  }
  await settle();

  return {
    dom,
    warnings,
    broadcasts,
    settle,
    /** What the background script has persisted back to storage.local. */
    stored: () => plain(storage.downloadJobs),
    /** The whole of storage.local, including the `downloadLogs:` keys. */
    storage: () => plain(storage),
    /** How many `storage.local.set` calls the script has made. */
    writes: () => writes,
    /** The most recent native port the script opened. */
    nativePort: () => ports[ports.length - 1],
    /** Wait out the background script's write-coalescing window. */
    async flush(ms = 500) {
      await new Promise((resolve) => setTimeout(resolve, ms));
      await settle();
    },
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
  loadOptions,
  nativePortStub,
  loadPopup,
  loadBackground,
  pageFixture,
  extensionSource,
  settle,
  plain
};
