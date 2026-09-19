/**
 * End-to-end: the whole download path in a real Firefox (KEI-60, KEI-82).
 *
 * `tests/e2e/smoke.test.mjs` stops at the browser and says so: "FFmpeg and the
 * native messaging host are not involved". This file is the other half. It
 * drives the path nothing else covers —
 *
 *   content script's `document.title` -> popup's `download-media` message ->
 *   background -> `runtime.connectNative` -> the Rust host -> FFmpeg -> a file
 *
 * — because the output naming rule's acceptance criterion lives at the end of
 * it. By default (KEI-84) a generically named playlist becomes `video.<ext>`
 * and repeats rename to `video_2.mp4`; with the "name downloads after the page
 * title" setting on, the page's own title names the file instead. Every layer
 * below the browser is exercised by unit and integration tests; only a real
 * Gecko can show that the setting, and the title, survive the whole trip.
 *
 * It cannot run in CI (no native host, and AGENTS.md keeps FFmpeg out of CI
 * deliberately), and it needs setup a plain checkout does not have, so it skips
 * out loud rather than failing or — worse — passing vacuously:
 *
 *     make extension-browser extension-ffmpeg extension-install
 *     make extension-e2e-native
 *
 * The playlists come from `tests/fixtures/protected_site.py`, whose
 * `/media/index.m3u8` route exists precisely so this path is reachable: every
 * other playlist it serves has a distinctive stem and would never reach the
 * title-naming code at all.
 */

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";

import { browserAvailable, extensionDir, extensionUrl, launchBrowser } from "./browser.mjs";
import {
  announceSkip,
  missingPrerequisites,
  outputDirectory,
  resolveFfmpeg,
  startFixtureOrigins
} from "./native-environment.mjs";

const ffmpeg = resolveFfmpeg();
const missing = missingPrerequisites({ browserAvailable: browserAvailable(), ffmpeg });
const skip = missing.length > 0;
if (skip) announceSkip("native-download.test.mjs", missing);

/** Opens a page in a background tab and returns the content script's scan of it. */
const SCAN = `
  const url = arguments[0];
  const tab = await browser.tabs.create({ url, active: false });
  const deadline = Date.now() + 25000;
  let last = "the tab never became scannable";
  while (Date.now() < deadline) {
    const current = await browser.tabs.get(tab.id);
    if (current.status === "complete") {
      try {
        return { tabId: tab.id, scan: await browser.tabs.sendMessage(tab.id, { type: "scan-media" }) };
      } catch (error) { last = String(error); }
    }
    await new Promise((resolve) => setTimeout(resolve, 250));
  }
  throw new Error(last);
`;

/** Starts a download exactly as the popup does, and waits for a terminal state. */
const DOWNLOAD = `
  const [url, sourceUrl, title, tabId] = arguments;
  const started = await browser.runtime.sendMessage({
    type: "download-media", url, sourceUrl, title, tabId
  });
  if (!started?.ok) return { state: "not-started", error: JSON.stringify(started) };
  const deadline = Date.now() + 120000;
  while (Date.now() < deadline) {
    const { jobs } = await browser.runtime.sendMessage({ type: "get-download-statuses" });
    const job = jobs.find((candidate) => candidate.id === started.jobId);
    if (job && ["completed", "failed", "cancelled", "interrupted"].includes(job.state)) return job;
    await new Promise((resolve) => setTimeout(resolve, 400));
  }
  return { state: "timed-out" };
`;

const SET_POLICY = `await browser.storage.local.set({ onConflict: arguments[0] }); return true;`;
const SET_TITLE_NAMING = `await browser.storage.local.set({ nameFromTitle: arguments[0] }); return true;`;

test("the extension downloads through the native host", { skip, concurrency: 1 }, async (t) => {
  // The native host discovers FFmpeg through this, and geckodriver inherits it,
  // so the browser it launches passes it down to the host it starts.
  process.env.DOWNER_FFMPEG = ffmpeg.path;

  const fixtures = await startFixtureOrigins(ffmpeg.path);
  const output = outputDirectory();
  const browser = await launchBrowser();
  const listing = () => fs.readdirSync(output).sort();
  const sizes = () =>
    Object.fromEntries(listing().map((name) => [name, fs.statSync(path.join(output, name)).size]));

  /** Scan a fixture page and download its generically named playlist. */
  async function downloadFrom(origin) {
    const { tabId, scan } = await browser.evaluate(SCAN, [`${origin.origin}/`]);
    const candidate = (scan.candidates || []).find((entry) =>
      entry.url.endsWith("/media/index.m3u8")
    );
    assert.ok(candidate, `no index.m3u8 candidate at ${origin.origin}`);
    assert.equal(scan.title, origin.title, "the content script reads the real document.title");
    return browser.evaluate(DOWNLOAD, [candidate.url, scan.sourceUrl, scan.title, tabId]);
  }

  try {
    await browser.installAddon(extensionDir());
    await browser.openExtensionPage(extensionUrl("options.html"));
    await browser.evaluate(
      `await browser.storage.local.set({ outputDir: arguments[0], onConflict: "rename" }); return true;`,
      [output]
    );

    const [first, second] = fixtures.origins;

    await t.test("a generically named playlist is video.mp4 by default", async () => {
      // The shipped default: the page has a perfectly good title and it is
      // deliberately not used, because naming from it is opt-in (KEI-84).
      const job = await downloadFrom(first);
      assert.equal(job.state, "completed", `download failed: ${job.error || "(no error)"}`);
      assert.equal(job.path, path.join(output, "video.mp4"));
      // A name is not enough: an empty file would satisfy that assertion while
      // meaning the download never happened.
      assert.ok(fs.statSync(job.path).size > 1000, "video.mp4 holds real media");
    });

    await t.test("a repeat renames to video_2.mp4 rather than failing", async () => {
      const before = fs.statSync(path.join(output, "video.mp4")).size;
      const job = await downloadFrom(second);
      assert.equal(job.state, "completed");
      assert.equal(job.path, path.join(output, "video_2.mp4"));
      assert.equal(
        fs.statSync(path.join(output, "video.mp4")).size,
        before,
        "the first download is left alone"
      );
    });

    await t.test("the title names the file once the user opts in", async () => {
      await browser.evaluate(SET_TITLE_NAMING, [true]);
      const results = [];
      for (const origin of fixtures.origins) {
        const job = await downloadFrom(origin);
        assert.equal(job.state, "completed", `download failed: ${job.error || "(no error)"}`);
        results.push(job.path);
      }

      const expected = fixtures.origins.map((origin) =>
        path.join(output, `${origin.title.replace(/:/g, "_")}.mp4`)
      );
      assert.deepEqual(results, expected);
      assert.notEqual(results[0], results[1], "the two pages must not collide");
      for (const file of results) {
        assert.ok(fs.statSync(file).size > 1000, `${file} holds real media`);
      }

      await browser.evaluate(SET_TITLE_NAMING, [false]);
    });

    await t.test("the Settings policy reaches the host", async () => {
      await browser.evaluate(SET_POLICY, ["fail"]);
      const before = sizes();
      const failed = await downloadFrom(first);
      assert.equal(failed.state, "failed");
      assert.match(failed.error, /already exists/);
      assert.deepEqual(sizes(), before, "fail writes nothing and changes nothing");

      await browser.evaluate(SET_POLICY, ["overwrite"]);
      const names = listing();
      const overwritten = await downloadFrom(first);
      assert.equal(overwritten.state, "completed");
      assert.equal(overwritten.path, path.join(output, "video.mp4"));
      assert.deepEqual(listing(), names, "overwrite replaces in place, adding no numbered file");
    });

    await t.test("the Settings page shows the stored policy after a reload", async () => {
      await browser.evaluate(SET_POLICY, ["fail"]);
      await browser.openExtensionPage(extensionUrl("options.html"));
      // `options.js` assigns the value inside `storage.local.get(...).then(...)`,
      // so a synchronous read can beat it. Polling is the difference between
      // testing the page and testing the scheduler.
      const shown = await browser.evaluate(`
        const element = document.getElementById("on-conflict");
        const deadline = Date.now() + 5000;
        while (Date.now() < deadline) {
          if (element.value === arguments[0]) return element.value;
          await new Promise((resolve) => setTimeout(resolve, 50));
        }
        return element.value;
      `, ["fail"]);
      assert.equal(shown, "fail");
    });

    await t.test("a download that fails before FFmpeg writes leaves no file behind", async () => {
      await browser.evaluate(SET_POLICY, ["rename"]);
      const { tabId, scan } = await browser.evaluate(SCAN, [`${second.origin}/`]);
      const url = scan.candidates.find((entry) => entry.url.endsWith("/media/index.m3u8")).url;

      // Take the origin away so FFmpeg cannot open the input at all. The
      // reservation this creates is the thing that must not survive.
      second.child.kill("SIGTERM");
      await new Promise((resolve) => setTimeout(resolve, 1500));

      const before = listing();
      const job = await browser.evaluate(DOWNLOAD, [url, scan.sourceUrl, scan.title, tabId]);
      assert.equal(job.state, "failed");
      assert.deepEqual(listing(), before, "no file is added for a download that wrote nothing");
      assert.ok(
        Object.values(sizes()).every((size) => size > 0),
        "no zero-byte reservation is left behind"
      );
    });
  } finally {
    await browser.close();
    fixtures.stop();
    fs.rmSync(output, { recursive: true, force: true });
  }
});
