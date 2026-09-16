"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

const {
  MEDIA_ATTRIBUTES,
  MEDIA_SELECTOR,
  collectCandidates
} = require("../../extension/media-scan.js");

const PAGE_BASE = "https://example.test/files/index.html";

/**
 * HTML fixtures are shared with `src/scraper.rs`'s own tests, so the CLI and the
 * extension stay pinned to the same pages until KEI-51 consolidates detection.
 */
function page(name) {
  return fs.readFileSync(
    path.join(__dirname, "..", "fixtures", "pages", `${name}.html`),
    "utf8"
  );
}

function scan(name, baseUrl = PAGE_BASE) {
  return collectCandidates({ html: page(name), baseUrl });
}

function urls(candidates) {
  return candidates.map((candidate) => candidate.url);
}

test("an absolute href to media is listed", () => {
  const found = urls(scan("anchor-absolute"));
  assert.deepEqual(found.sort(), [
    "https://cdn.example.test/movies/movie.mp4",
    "https://cdn.example.test/streams/live.m3u8"
  ]);
});

test("a relative href is resolved against the page and listed", () => {
  // The regression this issue reports: an anchor is not a fetched resource, and
  // the absolute-URL pass never matched a relative value, so these were missed.
  const found = urls(scan("anchor-relative"));
  assert.deepEqual(found.sort(), [
    "https://cdn.example.test/promo.mp4",
    "https://example.test/archive/talk.webm",
    "https://example.test/files/media/clip.m3u8",
    "https://example.test/files/movie.mp4"
  ]);
});

test("an href to a non-media file is not listed", () => {
  assert.deepEqual(scan("anchor-non-media"), []);
});

test("blob: and data: hrefs stay excluded and only http(s) is accepted", () => {
  const found = collectCandidates({
    html: page("anchor-non-media"),
    baseUrl: PAGE_BASE,
    attributeValues: [
      "blob:https://example.test/6f1c-movie.mp4",
      "data:video/mp4;base64,AAAA",
      "ftp://example.test/movie.mp4",
      "https://example.test/ok.mp4"
    ]
  });
  assert.deepEqual(urls(found), ["https://example.test/ok.mp4"]);
});

test("the same media reached by src and by href is listed once", () => {
  const found = scan("anchor-and-src");
  assert.deepEqual(urls(found), ["https://example.test/media/feature.mp4"]);
  assert.equal(found.length, 1, "addCandidate dedupes by resolved URL");
});

test("every attribute the Rust scraper reads is read here too", () => {
  const found = urls(scan("scraper-attributes"));
  assert.deepEqual(found.sort(), [
    "https://example.test/a/eight.ogv",
    "https://example.test/a/five.mov",
    "https://example.test/a/four.mkv",
    "https://example.test/a/nine.mpd",
    "https://example.test/a/one.mp4",
    "https://example.test/a/seven.flv",
    "https://example.test/a/six.m4v",
    "https://example.test/a/three.m3u8",
    "https://example.test/a/two.webm"
  ]);
});

test("the attribute list matches src/scraper.rs::extract_media_urls", () => {
  // A difference between the two lists means the CLI and the extension find
  // different media on the same page, which is the defect this issue reports.
  const scraper = fs.readFileSync(
    path.join(__dirname, "..", "..", "src", "scraper.rs"),
    "utf8"
  );
  const pattern = scraper.match(/\(\?is\)\(\?:([a-z_|-]+)\)\\s\*=/);
  assert.ok(pattern, "found attribute_pattern in src/scraper.rs");
  assert.deepEqual(
    pattern[1].split("|").sort(),
    [...MEDIA_ATTRIBUTES].sort()
  );
});

test("the DOM selector covers every scanned attribute plus anchors", () => {
  for (const attribute of MEDIA_ATTRIBUTES) {
    assert.ok(MEDIA_SELECTOR.includes(`[${attribute}]`), `selector covers ${attribute}`);
  }
  assert.ok(MEDIA_SELECTOR.includes("a[href]"));
});

test("HLS playlists sort ahead of plain video files", () => {
  const found = collectCandidates({
    baseUrl: PAGE_BASE,
    attributeValues: ["/a/video.mp4", "/a/stream.m3u8"]
  });
  assert.deepEqual(found.map((candidate) => candidate.type), ["hls", "video"]);
});

test("escaped URLs inside inline scripts are still found", () => {
  const found = collectCandidates({
    baseUrl: PAGE_BASE,
    html: '<script>var s = "https:\\/\\/cdn.example.test/live.m3u8?a=1&amp;b=2";</script>'
  });
  assert.deepEqual(urls(found), ["https://cdn.example.test/live.m3u8?a=1&b=2"]);
});

test("DOM attribute values and fetched resources are scanned alongside the markup", () => {
  const found = collectCandidates({
    baseUrl: PAGE_BASE,
    attributeValues: ["/from-dom.mp4"],
    resourceUrls: ["https://cdn.example.test/loaded.m3u8", "https://cdn.example.test/style.css"],
    html: page("anchor-relative")
  });
  assert.ok(urls(found).includes("https://example.test/from-dom.mp4"));
  assert.ok(urls(found).includes("https://cdn.example.test/loaded.m3u8"));
  assert.ok(!urls(found).some((url) => url.endsWith(".css")));
});

test("collectCandidates tolerates being called with nothing", () => {
  assert.deepEqual(collectCandidates(), []);
  assert.deepEqual(collectCandidates({ baseUrl: PAGE_BASE }), []);
});
