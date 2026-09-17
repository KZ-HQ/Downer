"use strict";

/**
 * The Settings page's log view, driven through the real `extension/options.js`
 * in a jsdom window.
 *
 * KEI-55 moved logs out of the job records and out of the `download-status`
 * broadcast, so this page now assembles them from a `get-download-logs` reply
 * plus `download-log` batches. The acceptance criterion it stands for: the page
 * still shows live logs, with per-download filtering and clearing.
 *
 * This is not Firefox. It does not exercise real WebExtension APIs or a real
 * native port.
 */

const test = require("node:test");
const assert = require("node:assert/strict");

const { loadOptions } = require("./helpers/extension-dom.js");

const TOKEN = "SENTINEL-SIGNED-TOKEN-MUST-NOT-PERSIST";
const JOB_A = { id: "a", url: "https://cdn.example.test/a/index.m3u8", state: "downloading" };
const JOB_B = { id: "b", url: "https://cdn.example.test/b/index.m3u8", state: "completed" };

function line(index) {
  return `[hls @ 0x7f8] Opening 'https://cdn.example.test/hls/seg${index}.ts?…' for reading`;
}

test("logs fetched on load are rendered", async () => {
  const options = await loadOptions({
    jobs: [JOB_A],
    logs: { a: [{ at: 1, text: line(1) }, { at: 2, text: line(2) }] }
  });
  assert.ok(options.logText().includes("seg1.ts"), options.logText());
  assert.ok(options.logText().includes("seg2.ts"));
  assert.ok(
    options.sent.some((message) => message.type === "get-download-logs"),
    "the page asks for logs separately from job state"
  );
});

test("a coalesced batch of live lines is appended", async () => {
  const options = await loadOptions({ jobs: [JOB_A], logs: { a: [] } });
  assert.equal(options.logText(), "No FFmpeg logs yet.");

  await options.receive({
    type: "download-log",
    jobId: "a",
    entries: [{ at: 3, text: line(3) }, { at: 4, text: line(4) }]
  });

  assert.ok(options.logText().includes("seg3.ts"));
  assert.ok(options.logText().includes("seg4.ts"));
});

test("the per-download filter still narrows the view", async () => {
  const options = await loadOptions({
    jobs: [JOB_A, JOB_B],
    logs: { a: [{ at: 1, text: line(1) }], b: [{ at: 2, text: line(2) }] }
  });
  assert.ok(options.logText().includes("seg1.ts") && options.logText().includes("seg2.ts"));

  options.selectJob("b");
  assert.equal(options.logText().includes("seg1.ts"), false, "job a's lines are filtered out");
  assert.ok(options.logText().includes("seg2.ts"));
});

test("clearing empties the view and asks the background script to clear too", async () => {
  const options = await loadOptions({ jobs: [JOB_A], logs: { a: [{ at: 1, text: line(1) }] } });
  await options.clearLogs();

  assert.equal(options.logText(), "No FFmpeg logs yet.");
  assert.ok(options.sent.some((message) => message.type === "clear-download-logs"));
});

test("a clear from elsewhere empties this page's view too", async () => {
  const options = await loadOptions({ jobs: [JOB_A], logs: { a: [{ at: 1, text: line(1) }] } });
  await options.receive({ type: "download-logs-cleared" });
  assert.equal(options.logText(), "No FFmpeg logs yet.");
});

test("a job's own URL is redacted where the page displays it", async () => {
  // The job record stores `url` whole, because the popup matches page media by
  // it and a re-download needs the real thing. The Settings page must not print
  // it whole: it heads every log line and fills the filter dropdown.
  const options = await loadOptions({
    jobs: [{ id: "a", url: `https://cdn.example.test/a/index.m3u8?token=${TOKEN}`, state: "downloading" }],
    logs: { a: [{ at: 1, text: line(1) }] }
  });

  assert.equal(options.logText().includes(TOKEN), false, "a token was printed above a log line");
  assert.ok(options.logText().includes("https://cdn.example.test/a/index.m3u8?…"));
  assert.equal(
    options.filterOptions().some((option) => option.includes(TOKEN)),
    false,
    "a token was printed in the download filter"
  );
});

test("a job with no logs does not break the view", async () => {
  const options = await loadOptions({ jobs: [JOB_A, JOB_B], logs: { a: [{ at: 1, text: line(1) }] } });
  assert.ok(options.logText().includes("seg1.ts"));
  options.selectJob("b");
  assert.equal(options.logText(), "No FFmpeg logs yet.");
});
