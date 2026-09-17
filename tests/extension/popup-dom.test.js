"use strict";

/**
 * Drives the real `extension/popup.html` + `extension/popup.js` against a real
 * DOM (jsdom) with a stubbed background script.
 *
 * `tests/extension/job-view.test.js` covers the pure "which jobs should render"
 * decision. This covers what previously needed Firefox: that the popup acts on
 * that decision — what the headline says, and what the row's buttons do.
 */

const test = require("node:test");
const assert = require("node:assert/strict");

const { loadContentScript, loadPopup } = require("./helpers/extension-dom.js");

const PAGE_URL = "https://example.test/files/index.html";
const FEATURE = "https://example.test/media/feature.mp4";

/** The candidates a real content script produces for a fixture page. */
async function candidatesFor(fixture) {
  const { scanMedia } = loadContentScript(fixture, { url: PAGE_URL });
  return (await scanMedia()).candidates;
}

test("KEI-74: anchor-linked media found by the content script is listed in the popup", async () => {
  // End to end across the two halves: the real content script scans a page of
  // relative anchors, and the real popup renders what it returns. This is the
  // acceptance criterion that previously read "needs Firefox".
  const candidates = await candidatesFor("anchor-relative");
  const popup = await loadPopup({ candidates });

  assert.equal(popup.status(), "4 media URLs found.");
  const labels = popup.rows().map((row) => row.label);
  assert.deepEqual(labels.sort(), [
    "HLS: https://example.test/files/media/clip.m3u8",
    "VIDEO: https://cdn.example.test/promo.mp4",
    "VIDEO: https://example.test/archive/talk.webm",
    "VIDEO: https://example.test/files/movie.mp4"
  ]);
  for (const row of popup.rows()) {
    assert.equal(row.count, "Not started");
    assert.equal(row.download.disabled, false);
  }
});

test("a single candidate is announced in the singular", async () => {
  const candidates = await candidatesFor("anchor-and-src");
  const popup = await loadPopup({ candidates });
  assert.equal(popup.status(), "1 media URL found.");
});

test("a page with no media says so and lists nothing", async () => {
  const popup = await loadPopup({ candidates: [] });
  assert.equal(popup.status(), "No media URL found.");
  assert.deepEqual(popup.rows(), []);
});

test("KEI-75: a terminal job from an earlier session for another page leaves the headline alone", async () => {
  // The reported bug: this used to show "Download cancelled." as the headline on
  // a page where nothing had ever been downloaded.
  const popup = await loadPopup({
    candidates: [{ url: FEATURE, type: "video" }],
    jobs: [{
      id: "old-1",
      url: "https://other.test/yesterday.mp4",
      state: "cancelled",
      error: "Download cancelled."
    }],
    sessionJobIds: []
  });

  assert.equal(popup.status(), "1 media URL found.");
  assert.equal(popup.downloadStatus(), "");
  assert.equal(popup.rows()[0].count, "Not started");
});

test("KEI-75: several stored jobs from earlier sessions still leave the headline alone", async () => {
  // Previously the last job in the list won, whatever page it belonged to.
  const popup = await loadPopup({
    candidates: [{ url: FEATURE, type: "video" }],
    jobs: [
      { id: "old-1", url: "https://other.test/a.mp4", state: "completed", path: "/tmp/a.mp4" },
      { id: "old-2", url: "https://other.test/b.mp4", state: "failed", error: "boom" },
      { id: "old-3", url: "https://other.test/c.mp4", state: "cancelled" }
    ]
  });
  assert.equal(popup.status(), "1 media URL found.");
  assert.equal(popup.downloadStatus(), "");
});

test("KEI-75: a job persisted as downloading does not present as live", async () => {
  // The second failure mode: its URL matches a candidate, so the row used to
  // render "Downloading…" with Download disabled and Pause/Cancel wired to a
  // native task that no longer exists.
  const popup = await loadPopup({
    candidates: [{ url: FEATURE, type: "video" }],
    jobs: [{ id: "interrupted-1", url: FEATURE, state: "downloading", percent: 40 }],
    sessionJobIds: []
  });

  const [row] = popup.rows();
  assert.equal(row.count, "Interrupted — not running");
  assert.equal(row.download.disabled, false, "Download must be usable again");
  assert.equal(row.download.text, "Download");
  assert.equal(row.download.jobId, undefined, "nothing may be wired to a dead task");
  assert.equal(row.pause.hidden, true);
  assert.equal(row.resume.hidden, true);
  assert.equal(row.cancel.hidden, true);
  assert.equal(popup.status(), "1 media URL found.", "and it must not set the headline");
  assert.equal(popup.downloadStatus(), "");
});

test("KEI-75: a terminal job from an earlier session for THIS page does not reclaim the row", async () => {
  const popup = await loadPopup({
    candidates: [{ url: FEATURE, type: "video" }],
    jobs: [{ id: "old-1", url: FEATURE, state: "completed", path: "/tmp/feature.mp4" }],
    sessionJobIds: []
  });
  const [row] = popup.rows();
  assert.equal(row.count, "Not started");
  assert.equal(row.download.disabled, false);
  assert.equal(popup.status(), "1 media URL found.");
});

test("a job from this session for this page does render and does set the headline", async () => {
  // The path that must keep working: the fix must not mute live status.
  const popup = await loadPopup({
    candidates: [{ url: FEATURE, type: "video" }],
    jobs: [{ id: "live-1", url: FEATURE, state: "completed", path: "/tmp/feature.mp4" }],
    sessionJobIds: ["live-1"]
  });

  assert.equal(popup.status(), "Download complete: /tmp/feature.mp4");
  assert.equal(popup.downloadStatus(), "The media file is ready.");
  const [row] = popup.rows();
  assert.equal(row.download.text, "Downloaded");
  assert.equal(row.download.disabled, true);
});

test("a live download in this session shows progress and offers Pause and Cancel", async () => {
  const popup = await loadPopup({
    candidates: [{ url: FEATURE, type: "video" }],
    jobs: [{
      id: "live-1",
      url: FEATURE,
      state: "downloading",
      completedSegments: 3,
      totalSegments: 10,
      percent: 30
    }],
    sessionJobIds: ["live-1"]
  });

  const [row] = popup.rows();
  assert.equal(row.count, "3 / 10 segments");
  assert.equal(row.download.disabled, true);
  assert.equal(row.download.jobId, "live-1");
  assert.equal(row.pause.hidden, false);
  assert.equal(row.cancel.hidden, false);
  assert.equal(row.resume.hidden, true);
});

test("a page that cannot be scanned explains itself instead of throwing", async () => {
  const popup = await loadPopup({ candidates: [], tabUrl: "about:debugging" });
  assert.equal(popup.status(), "This page cannot be scanned.");
});

test("KEI-56: a job interrupted by a browser restart explains itself in the popup", async () => {
  // What the user sees after reconciliation: the row is usable again and the
  // headline says why, instead of "Downloading…" with dead controls forever.
  const { INTERRUPTED_ERROR } = require("../../extension/job-state.js");
  const popup = await loadPopup({
    candidates: [{ url: FEATURE, type: "video" }],
    jobs: [{
      id: "interrupted-1",
      url: FEATURE,
      state: "interrupted",
      error: INTERRUPTED_ERROR,
      finishedAt: 123,
      completedSegments: 3,
      totalSegments: 10
    }],
    sessionJobIds: []
  });

  assert.equal(popup.status(), INTERRUPTED_ERROR);
  assert.equal(popup.downloadStatus(), "Start it again to download the rest.");
  const [row] = popup.rows();
  assert.equal(row.download.disabled, false, "the row must be usable again");
  assert.equal(row.download.text, "Download again");
  assert.equal(row.pause.hidden, true);
  assert.equal(row.resume.hidden, true);
  assert.equal(row.cancel.hidden, true);
});

test("KEI-56: an interrupted job for another page still never sets the headline", async () => {
  // KEI-75's rule survives the new state: relevance is decided before rendering.
  const popup = await loadPopup({
    candidates: [{ url: FEATURE, type: "video" }],
    jobs: [{
      id: "interrupted-1",
      url: "https://other.test/yesterday.mp4",
      state: "interrupted",
      error: "Interrupted by browser restart; partial file kept."
    }]
  });
  assert.equal(popup.status(), "1 media URL found.");
  assert.equal(popup.downloadStatus(), "");
});

test("KEI-56: the newest job for a URL owns the row, and an older one cannot take it back", async () => {
  // Two jobs for one URL used to share a row, so whichever rendered last won.
  const popup = await loadPopup({
    candidates: [{ url: FEATURE, type: "video" }],
    // The older job is rendered LAST on purpose: without row ownership it simply
    // overwrites the live one, which is the defect.
    jobs: [
      { id: "new", url: FEATURE, state: "downloading", startedAt: 200, completedSegments: 2, totalSegments: 8 },
      { id: "old", url: FEATURE, state: "interrupted", error: "old run", startedAt: 100 }
    ],
    sessionJobIds: ["new"]
  });

  const [row] = popup.rows();
  assert.equal(row.count, "2 / 8 segments", "the live job owns the row");
  assert.equal(row.download.jobId, "new");
  assert.equal(row.pause.hidden, false, "and its controls are live");
});
