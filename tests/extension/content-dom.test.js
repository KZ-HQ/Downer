"use strict";

/**
 * Drives the real `extension/content.js` over a real DOM (jsdom), through the
 * same `scan-media` message the popup sends.
 *
 * `tests/extension/media-scan.test.js` covers the pure scanner. This covers the
 * part that was previously only verifiable by opening Firefox: the DOM adapter,
 * `MEDIA_SELECTOR`, and the message listener that connects them.
 */

const test = require("node:test");
const assert = require("node:assert/strict");

const { loadContentScript } = require("./helpers/extension-dom.js");

const PAGE_URL = "https://example.test/files/index.html";

async function scan(fixture, url = PAGE_URL) {
  const { scanMedia } = loadContentScript(fixture, { url });
  return scanMedia();
}

test("the scan-media listener answers with the page URL, title and candidates", async () => {
  const result = await scan("anchor-absolute");
  assert.equal(result.sourceUrl, PAGE_URL);
  assert.equal(typeof result.title, "string");
  assert.ok(Array.isArray(result.candidates));
});

test("KEI-74: a page linking media only with an absolute href yields those candidates", async () => {
  const { candidates } = await scan("anchor-absolute");
  assert.deepEqual(candidates.map((candidate) => candidate.url).sort(), [
    "https://cdn.example.test/movies/movie.mp4",
    "https://cdn.example.test/streams/live.m3u8"
  ]);
});

test("KEI-74: a page linking media only with a relative href yields those candidates", async () => {
  // The original report: a `python3 -m http.server` directory listing, which is
  // a page of relative anchors and nothing else. This used to yield nothing.
  const { candidates } = await scan("anchor-relative");
  assert.deepEqual(candidates.map((candidate) => candidate.url).sort(), [
    "https://cdn.example.test/promo.mp4",
    "https://example.test/archive/talk.webm",
    "https://example.test/files/media/clip.m3u8",
    "https://example.test/files/movie.mp4"
  ]);
});

test("KEI-74: a page of non-media links yields nothing", async () => {
  const { candidates } = await scan("anchor-non-media");
  assert.deepEqual(candidates, []);
});

test("KEI-74: media reached by both src and href is listed once", async () => {
  const { candidates } = await scan("anchor-and-src");
  assert.deepEqual(candidates.map((candidate) => candidate.url), [
    "https://example.test/media/feature.mp4"
  ]);
});

test("MEDIA_SELECTOR matches every attribute through a real querySelectorAll", async () => {
  // A selector that failed to parse, or missed an attribute, would return zero
  // candidates here while every pure media-scan test stayed green.
  const { candidates } = await scan("scraper-attributes");
  assert.equal(candidates.length, 9, JSON.stringify(candidates, null, 2));
});

test("HLS candidates are listed first, as the popup's help text promises", async () => {
  const { candidates } = await scan("anchor-absolute");
  assert.equal(candidates[0].type, "hls");
});

test("URLs the page actually fetched are scanned alongside the markup", async () => {
  // jsdom has no Resource Timing API; the harness supplies the entries Firefox
  // would report, so this pass is exercised rather than silently skipped.
  const { scanMedia } = loadContentScript("anchor-non-media", {
    url: PAGE_URL,
    resourceEntries: [
      "https://cdn.example.test/player/loaded.m3u8",
      "https://cdn.example.test/player/app.css"
    ]
  });
  const { candidates } = await scanMedia();
  assert.deepEqual(candidates.map((candidate) => candidate.url), [
    "https://cdn.example.test/player/loaded.m3u8"
  ]);
});
