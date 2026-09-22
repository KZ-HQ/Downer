"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");

const { CONFIDENCE } = require("../../extension/media-scan.js");
const { partition, collapsedSummary } = require("../../extension/candidate-view.js");

/** A candidate as `collectCandidates` builds one, trimmed to what matters here. */
function candidate(url, confidence, kind = "file") {
  return { url, confidence, kind, type: kind === "hls" ? "hls" : "video" };
}

const urls = (list) => list.map((item) => item.url);

test("the confidence order comes from the detection module, not a copy", () => {
  assert.deepEqual(CONFIDENCE, ["observed", "declared", "inferred"]);
});

test("KEI-98: a source the page never loaded is folded away", () => {
  // The reported page: <video> with an .mp4 and an .ogg <source>. Firefox
  // fetched the .mp4 and never touched the .ogg.
  const { listed, collapsed } = partition([
    candidate("https://example.test/mov_bbb.mp4", "observed"),
    candidate("https://example.test/mov_bbb.ogg", "declared")
  ]);

  assert.deepEqual(urls(listed), ["https://example.test/mov_bbb.mp4"]);
  assert.deepEqual(urls(collapsed), ["https://example.test/mov_bbb.ogg"]);
});

test("KEI-98: the rule keys off evidence, not off sibling elements or filenames", () => {
  // Two genuinely distinct videos, both loaded. Nothing is demoted: they are
  // equally good evidence, which is the whole test.
  const { listed, collapsed } = partition([
    candidate("https://example.test/one.mp4", "observed"),
    candidate("https://example.test/two.mp4", "observed")
  ]);

  assert.deepEqual(urls(listed), [
    "https://example.test/one.mp4",
    "https://example.test/two.mp4"
  ]);
  assert.deepEqual(collapsed, []);
});

test("with nothing observed, the split is what it was before this rule", () => {
  // The popup opened before playback started. Declared markup is the best
  // evidence there is, so it is listed and only page-text matches collapse.
  const { listed, collapsed } = partition([
    candidate("https://example.test/a.mp4", "declared"),
    candidate("https://example.test/b.mp4", "inferred")
  ]);

  assert.deepEqual(urls(listed), ["https://example.test/a.mp4"]);
  assert.deepEqual(urls(collapsed), ["https://example.test/b.mp4"]);
});

test("when everything is a page-text match, it is listed rather than all hidden", () => {
  const { listed, collapsed } = partition([
    candidate("https://example.test/a.mp4", "inferred"),
    candidate("https://example.test/b.mp4", "inferred")
  ]);

  assert.equal(listed.length, 2);
  assert.deepEqual(collapsed, []);
});

test("a playlist is never demoted, however good the evidence beside it", () => {
  // A declared .m3u8 next to an observed preview clip. Ranking the stream below
  // the preview would contradict the playlists-first sort in media-scan.js.
  const { listed, collapsed } = partition([
    candidate("https://example.test/master.m3u8", "declared", "hls"),
    candidate("https://example.test/preview.mp4", "observed")
  ]);

  assert.deepEqual(urls(listed), [
    "https://example.test/master.m3u8",
    "https://example.test/preview.mp4"
  ]);
  assert.deepEqual(collapsed, []);
});

test("a DASH manifest is a playlist for this purpose too", () => {
  const { listed } = partition([
    candidate("https://example.test/manifest.mpd", "inferred", "dash"),
    candidate("https://example.test/clip.mp4", "observed")
  ]);

  assert.equal(listed.length, 2);
});

test("nothing is ever dropped", () => {
  const candidates = [
    candidate("https://example.test/a.mp4", "observed"),
    candidate("https://example.test/b.mp4", "declared"),
    candidate("https://example.test/c.mp4", "inferred")
  ];
  const { listed, collapsed } = partition(candidates);

  assert.deepEqual(
    [...urls(listed), ...urls(collapsed)].sort(),
    urls(candidates).sort()
  );
});

test("an empty page partitions to two empty lists rather than throwing", () => {
  assert.deepEqual(partition([]), { listed: [], collapsed: [] });
  assert.deepEqual(partition(), { listed: [], collapsed: [] });
});

test("an unfamiliar confidence ranks last rather than winning the list", () => {
  // Defensive: a candidate from a newer content script than this popup.
  const { listed, collapsed } = partition([
    candidate("https://example.test/known.mp4", "declared"),
    candidate("https://example.test/strange.mp4", "guessed")
  ]);

  assert.deepEqual(urls(listed), ["https://example.test/known.mp4"]);
  assert.deepEqual(urls(collapsed), ["https://example.test/strange.mp4"]);
});

test("the summary says why things are hidden, and the two reasons differ", () => {
  assert.equal(
    collapsedSummary([candidate("https://example.test/a.mp4", "inferred")]),
    "1 other candidate found in the page text"
  );
  assert.equal(
    collapsedSummary([
      candidate("https://example.test/a.mp4", "declared"),
      candidate("https://example.test/b.mp4", "inferred")
    ]),
    "2 other candidates this page did not load"
  );
});
