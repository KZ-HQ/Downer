"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

const {
  matchingVariant,
  highestBandwidthVariant,
  parentPlaylistUrl,
  parseHlsInfo
} = require("../../extension/hls.js");

// The same fixtures back the Rust tests in src/scraper.rs, so both parsers stay
// pinned to identical inputs until the heuristics are consolidated.
const fixture = (name) =>
  fs.readFileSync(path.join(__dirname, "..", "fixtures", "hls", name), "utf8");

const SEGMENTS_PLAYLIST = fixture("segments.m3u8");
const TOLERANT_PLAYLIST = fixture("segments-tolerant.m3u8");
const MASTER_PLAYLIST = fixture("master.m3u8");

test("highestBandwidthVariant picks the highest BANDWIDTH and resolves it", () => {
  assert.equal(
    highestBandwidthVariant(MASTER_PLAYLIST, "https://example.test/master.m3u8"),
    "https://example.test/high/video.m3u8"
  );
});

test("highestBandwidthVariant returns null without variants", () => {
  assert.equal(
    highestBandwidthVariant(SEGMENTS_PLAYLIST, "https://example.test/index.m3u8"),
    null
  );
});

test("matchingVariant prefers the requested variant over the highest bandwidth", () => {
  assert.equal(
    matchingVariant(
      MASTER_PLAYLIST,
      "https://example.test/master.m3u8",
      "https://example.test/low/video.m3u8"
    ),
    "https://example.test/low/video.m3u8"
  );
});

test("matchingVariant falls back to the highest bandwidth when nothing matches", () => {
  assert.equal(
    matchingVariant(
      MASTER_PLAYLIST,
      "https://example.test/master.m3u8",
      "https://example.test/other/video.m3u8"
    ),
    "https://example.test/high/video.m3u8"
  );
});

test("matchingVariant only reads BANDWIDTH from a stream attribute list", () => {
  const playlist = [
    "#EXTM3U",
    "#EXT-X-STREAM-INF:RESOLUTION=640x360,BANDWIDTH=900",
    "mid/video.m3u8",
    "#EXT-X-STREAM-INF:AVERAGE-BANDWIDTH=9000",
    "average/video.m3u8"
  ].join("\n");
  assert.equal(
    highestBandwidthVariant(playlist, "https://example.test/master.m3u8"),
    "https://example.test/mid/video.m3u8"
  );
});

test("parentPlaylistUrl walks one directory up to playlist.m3u8", () => {
  assert.equal(
    parentPlaylistUrl("https://example.test/media/720p/index.m3u8"),
    "https://example.test/media/playlist.m3u8"
  );
});

test("parentPlaylistUrl returns null for an unparseable URL", () => {
  assert.equal(parentPlaylistUrl("not a url"), null);
});

test("parseHlsInfo counts segments and total duration", () => {
  assert.deepEqual(parseHlsInfo(SEGMENTS_PLAYLIST), {
    totalSegments: 2,
    totalDurationMs: 10506
  });
});

test("parseHlsInfo tolerates indentation and skips bad durations", () => {
  assert.deepEqual(parseHlsInfo(TOLERANT_PLAYLIST), {
    totalSegments: 2,
    totalDurationMs: 5500
  });
});

test("parseHlsInfo handles CRLF playlists", () => {
  assert.deepEqual(parseHlsInfo(SEGMENTS_PLAYLIST.replace(/\n/g, "\r\n")), {
    totalSegments: 2,
    totalDurationMs: 10506
  });
});

test("parseHlsInfo returns null when no segments are present", () => {
  assert.equal(parseHlsInfo(MASTER_PLAYLIST), null);
});
