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

test("the FFmpeg path setting round-trips and is saved trimmed", async () => {
  const page = await loadOptions({ settings: { ffmpegPath: "/opt/homebrew/bin/ffmpeg" } });
  assert.equal(page.field("ffmpeg-path").value, "/opt/homebrew/bin/ffmpeg");

  page.field("ffmpeg-path").value = "  /usr/local/bin/ffmpeg  ";
  await page.save();
  assert.equal(page.saved().at(-1).ffmpegPath, "/usr/local/bin/ffmpeg");
});

test("Check setup renders every check with its outcome and remedy", async () => {
  const page = await loadOptions({
    setupResponse: {
      ok: true,
      status: {
        host_version: "0.5.0",
        protocol_version: 1,
        platform: "linux",
        checks: [
          { name: "host_registration", title: "Native host registered with Firefox", outcome: "pass", detail: "/home/u/.mozilla/x.json" },
          { name: "ffmpeg", title: "FFmpeg available", outcome: "warn", detail: "ffmpeg is FFmpeg 6.1.1", remedy: "Upgrade FFmpeg." },
          { name: "output_directory", title: "Download directory writable", outcome: "fail", detail: "/nope cannot be written to", remedy: "Choose a directory you can write to." }
        ]
      }
    }
  });
  await page.checkSetup();

  // The host's three outcomes stay three outcomes: a warning is not folded
  // into a failure, because it means downloads still work (ADR-0006).
  assert.deepEqual(page.setupOutcomes(), ["check-pass", "check-warn", "check-fail"]);

  const text = page.setupText();
  assert.match(text, /downer 0\.5\.0 \(protocol 1\) on linux/);
  assert.match(text, /FFmpeg 6\.1\.1/);
  assert.match(text, /Upgrade FFmpeg\./);
  assert.match(text, /Choose a directory you can write to\./);
});

test("an unreachable host is itself reported as a failed check, with what to do", async () => {
  const page = await loadOptions({
    setupResponse: { ok: false, error: "No such native application com.downer.native" }
  });
  await page.checkSetup();

  assert.deepEqual(page.setupOutcomes(), ["check-fail"]);
  const text = page.setupText();
  assert.match(text, /No such native application/);
  // The remediation has to be written by the extension: there is no host to
  // ask, which is exactly the case the panel exists for.
  assert.match(text, /downer install-host/);
});

test("a protocol mismatch says to update a build, not to re-register the host", async () => {
  // The host refuses the handshake when the extension is newer; the extension
  // refuses the answer when the host is newer. Both arrive here as one
  // exception, and `downer install-host` fixes neither — the two halves ship
  // separately, so this is ordinary upgrade skew rather than a broken install.
  const page = await loadOptions({
    setupResponse: {
      ok: false,
      error: "unsupported protocol version 99; this host speaks version 1"
    }
  });
  await page.checkSetup();

  const text = page.setupText();
  assert.match(text, /unsupported protocol version 99/);
  assert.match(text, /different builds/);
  assert.doesNotMatch(
    text,
    /Install or re-register/,
    "the generic registration advice would not fix a version mismatch"
  );
});

test("KEI-65: keeping part-written files is off by default and round-trips", async () => {
  const page = await loadOptions({});
  assert.equal(
    page.field("keep-partial").checked,
    false,
    "cancelling deletes the fragment unless the user says otherwise"
  );

  page.field("keep-partial").checked = true;
  await page.save();
  assert.equal(page.saved().at(-1).keepPartial, true);

  const stored = await loadOptions({ settings: { keepPartial: true } });
  assert.equal(stored.field("keep-partial").checked, true);
});

test("KEI-90: the output directory field does not promise a folder that may not exist", async () => {
  // The placeholder used to say "Defaults to your Downloads folder". On Linux
  // without XDG user-directory configuration there is no such folder, and the
  // host was falling back to its own working directory — so the promise was
  // untrue exactly where it mattered.
  const page = await loadOptions({});
  const field = page.field("output-dir");
  assert.equal(field.placeholder, "Leave blank for the default");
  assert.ok(
    !field.placeholder.includes("Downloads folder"),
    "the placeholder must not promise a specific folder"
  );
});
