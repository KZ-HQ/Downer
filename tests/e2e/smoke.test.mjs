/**
 * End-to-end smoke test: the shipped extension in a real Firefox.
 *
 * `tests/extension/` covers the same code in jsdom, where there is no add-on
 * installation, no content-script injection, and no `browser` APIs. What this
 * file adds is the part only a real Gecko can answer:
 *
 *   - Firefox accepts `extension/manifest.json` and loads the background
 *     scripts without an error.
 *   - The background script answers the message protocol the popup and the
 *     options page use.
 *   - The content script is injected into real pages served over HTTP, and its
 *     scan of the live DOM finds what the jsdom tests expect from the same
 *     fixtures. A difference between the two is a jsdom artefact, and the
 *     browser is the one that is right.
 *
 * Nothing here needs FFmpeg or the native messaging host: `download-media` is
 * the only message that reaches them, and it is deliberately not exercised.
 */

import test from "node:test";
import assert from "node:assert/strict";

import {
  EXTENSION_ID,
  browserAvailable,
  extensionDir,
  extensionUrl,
  launchBrowser
} from "./browser.mjs";
import { startFixtureServer } from "./fixture-server.mjs";

/**
 * Skip rather than fail when no browser is installed, so a bare `node --test`
 * stays useful on a machine without one. `make extension-e2e` checks for the
 * binaries first and fails loudly there instead.
 */
const missingBrowser = !browserAvailable();

/**
 * Opens a fixture page in a background tab and returns the content script's
 * scan of it.
 *
 * The tab is driven from the extension page with `browser.tabs`, not with
 * WebDriver's own tab commands, because that is what the popup does: the popup
 * never focuses the page it scans. The retry covers the gap between "the tab
 * finished loading" and "the content script is listening".
 */
const SCAN_TAB = `
  const url = arguments[0];
  const tab = await browser.tabs.create({ url, active: false });
  try {
    const deadline = Date.now() + 20000;
    let lastError = "the tab never finished loading";
    while (Date.now() < deadline) {
      const current = await browser.tabs.get(tab.id);
      if (current.status === "complete") {
        try {
          return await browser.tabs.sendMessage(tab.id, { type: "scan-media" });
        } catch (error) {
          lastError = String(error);
        }
      }
      await new Promise((resolve) => setTimeout(resolve, 100));
    }
    throw new Error("the content script never answered: " + lastError);
  } finally {
    await browser.tabs.remove(tab.id);
  }
`;

test(
  "the extension runs in Firefox",
  { skip: missingBrowser && "no Firefox or geckodriver; see docs/e2e-firefox.md" },
  async (t) => {
    const fixtures = await startFixtureServer();
    const browser = await launchBrowser();
    t.after(async () => {
      await browser.close();
      await fixtures.close();
    });

    const addonId = await browser.installAddon(extensionDir());
    // The extension's own pages are the only context with WebExtension APIs, so
    // every assertion below is made from one.
    await browser.openExtensionPage(extensionUrl("options.html"));

    const scan = (name) => browser.evaluate(SCAN_TAB, [fixtures.pageUrl(name)]);
    const urls = (result) => result.candidates.map((candidate) => candidate.url).sort();

    await t.test("Firefox installs the extension and loads its pages", async () => {
      assert.equal(addonId, EXTENSION_ID);
      assert.equal(await browser.evaluate("return document.title;"), "Downer settings");
    });

    await t.test("the background script answers the job protocol", async () => {
      const statuses = await browser.evaluate(
        "return await browser.runtime.sendMessage({ type: 'get-download-statuses' });"
      );
      assert.deepEqual(statuses.jobs, []);
      assert.deepEqual(statuses.sessionJobIds, []);

      const logs = await browser.evaluate(
        "return await browser.runtime.sendMessage({ type: 'get-download-logs' });"
      );
      assert.deepEqual(logs.logs, {});
    });

    await t.test("the content script scans the page it is injected into", async () => {
      const pageUrl = fixtures.pageUrl("anchor-and-src");
      const result = await scan("anchor-and-src");

      assert.equal(result.sourceUrl, pageUrl);
      assert.deepEqual(urls(result), [
        `${fixtures.origin}/media/feature.mp4`,
        "https://example.test/media/feature.mp4"
      ]);
      assert.ok(result.candidates.every((candidate) => candidate.type === "video"));
    });

    await t.test("the real DOM finds the same media as the jsdom tests", async () => {
      // The expectations mirror `tests/extension/media-scan.test.js`, against
      // the same fixture, so the two layers stay honest about each other.
      assert.deepEqual(
        urls(await scan("scraper-attributes")),
        [
          "eight.ogv",
          "five.mov",
          "four.mkv",
          "nine.mpd",
          "one.mp4",
          "seven.flv",
          "six.m4v",
          "three.m3u8",
          "two.webm"
        ].map((file) => `${fixtures.origin}/a/${file}`)
      );

      assert.deepEqual(urls(await scan("anchor-non-media")), []);
    });
  }
);
