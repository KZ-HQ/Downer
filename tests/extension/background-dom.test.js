"use strict";

/**
 * Drives the real `extension/background.js` through a simulated browser start:
 * `storage.local` already holds jobs from a previous session, exactly as it
 * would after a restart or an extension reload.
 *
 * `tests/extension/job-state.test.js` covers the reconciliation rule itself.
 * This covers that the background script actually applies it on load, persists
 * the result, and serves it to the popup.
 */

const test = require("node:test");
const assert = require("node:assert/strict");

const { loadBackground } = require("./helpers/extension-dom.js");
const { INTERRUPTED_ERROR, isTerminal } = require("../../extension/job-state.js");

const URL_A = "https://example.test/media/a.mp4";
const URL_B = "https://example.test/media/b.mp4";

test("KEI-56: a job still active at shutdown is interrupted on the next start", async () => {
  // The reported defect: this job stayed "downloading" forever, with the popup
  // showing a disabled button and controls that could only answer
  // "Download task is no longer active."
  const background = await loadBackground({
    downloadJobs: [{ id: "a", url: URL_A, state: "downloading", completedSegments: 3, totalSegments: 10 }]
  });

  const { jobs } = await background.send({ type: "get-download-statuses" });
  assert.equal(jobs.length, 1);
  assert.equal(jobs[0].state, "interrupted");
  assert.equal(jobs[0].error, INTERRUPTED_ERROR);
  assert.ok(jobs[0].finishedAt > 0, "a reconciled job is finished");
  assert.equal(jobs[0].completedSegments, 3, "progress is preserved");
});

test("KEI-56: every restored job is terminal after a start", async () => {
  const background = await loadBackground({
    downloadJobs: [
      { id: "a", url: URL_A, state: "starting" },
      { id: "b", url: URL_B, state: "preparing" },
      { id: "c", url: "https://example.test/c.mp4", state: "downloading" },
      { id: "d", url: "https://example.test/d.mp4", state: "paused" },
      { id: "e", url: "https://example.test/e.mp4", state: "cancelling" }
    ]
  });

  const { jobs } = await background.send({ type: "get-download-statuses" });
  assert.equal(jobs.length, 5);
  for (const job of jobs) {
    assert.equal(job.state, "interrupted", job.id);
    assert.equal(isTerminal(job.state), true, job.id);
  }
});

test("KEI-56: reconciliation is written back, so it happens once and survives", async () => {
  const background = await loadBackground({
    downloadJobs: [{ id: "a", url: URL_A, state: "downloading" }]
  });
  const stored = background.stored();
  assert.equal(stored[0].state, "interrupted", "persisted, not just held in memory");

  // A second start sees the reconciled record and leaves it alone.
  const restarted = await loadBackground({ downloadJobs: stored });
  const { jobs } = await restarted.send({ type: "get-download-statuses" });
  assert.equal(jobs[0].state, "interrupted");
  assert.equal(jobs[0].finishedAt, stored[0].finishedAt, "the finish time is not rewritten");
});

test("KEI-56: finished jobs are restored untouched and storage is not rewritten", async () => {
  const downloadJobs = [
    { id: "a", url: URL_A, state: "completed", path: "/tmp/a.mp4", finishedAt: 11 },
    { id: "b", url: URL_B, state: "cancelled", finishedAt: 22 }
  ];
  const background = await loadBackground({ downloadJobs });

  const { jobs } = await background.send({ type: "get-download-statuses" });
  assert.deepEqual(jobs.map((job) => job.state), ["completed", "cancelled"]);
  assert.deepEqual(background.stored(), downloadJobs, "nothing was rewritten");
});

test("KEI-56: a record written before `interrupted` existed is reconciled too", async () => {
  // Backward compatibility with the old storage format.
  const background = await loadBackground({
    downloadJobs: [{ id: "a", url: URL_A }, { id: "b", url: URL_B, state: "sleeping" }]
  });
  const { jobs } = await background.send({ type: "get-download-statuses" });
  assert.deepEqual(jobs.map((job) => job.state), ["interrupted", "interrupted"]);
});

test("a control command for a job with no live task is refused, not crashed on", async () => {
  const background = await loadBackground({
    downloadJobs: [{ id: "a", url: URL_A, state: "downloading" }]
  });
  const reply = await background.send({ type: "control-download", jobId: "a", command: "pause" });
  assert.deepEqual(reply, { ok: false, error: "Download task is no longer active." });
});

test("malformed stored records are dropped rather than restored", async () => {
  const background = await loadBackground({
    downloadJobs: [null, { url: URL_A, state: "downloading" }, { id: "a", url: URL_B, state: "downloading" }]
  });
  const { jobs } = await background.send({ type: "get-download-statuses" });
  assert.deepEqual(jobs.map((job) => job.id), ["a"]);
});

test("no job begun in this session is reported after a cold start", async () => {
  // What KEI-75's popup filter depends on: restored jobs are not session jobs.
  const background = await loadBackground({
    downloadJobs: [{ id: "a", url: URL_A, state: "downloading" }]
  });
  const { sessionJobIds } = await background.send({ type: "get-download-statuses" });
  assert.deepEqual(sessionJobIds, []);
});

// ---------------------------------------------------------------------------
// KEI-61: enumerating a playlist, and carrying the choice to the host.
// ---------------------------------------------------------------------------

const MASTER_URL = "https://cdn.test/v/master.m3u8";
const MASTER_TEXT = [
  "#EXTM3U",
  '#EXT-X-STREAM-INF:BANDWIDTH=200000,RESOLUTION=320x180',
  "low/video.m3u8",
  '#EXT-X-STREAM-INF:BANDWIDTH=900000,RESOLUTION=1280x720',
  "high/video.m3u8",
  ""
].join("\n");

test("inspect-playlist fetches in the page's session and lets the host parse", async () => {
  const background = await loadBackground({
    native: true,
    playlistText: MASTER_TEXT,
    nativeOptions: {
      playlistInfo: {
        playlist_kind: "master",
        renditions: [
          { url: "https://cdn.test/v/high/video.m3u8", bandwidth: 900000, default: true }
        ]
      }
    }
  });

  const response = await background.send({
    type: "inspect-playlist",
    url: MASTER_URL,
    sourceUrl: "https://page.test/watch",
    tabId: 1
  });
  await background.settle();

  assert.equal(response.ok, true);
  assert.equal(response.kind, "master");
  assert.equal(response.renditions.length, 1);

  // The division ADR-0011 set: this side sends the text it fetched, and does
  // not parse it. A picker that parsed here would be the second parser KEI-51
  // deleted.
  const sent = background.nativePort().posted.find((m) => m?.command === "playlist-info");
  assert.ok(sent, "the host was asked");
  assert.equal(sent.url, MASTER_URL);
  assert.equal(sent.playlist_text, MASTER_TEXT);
});

test("a playlist the page cannot fetch answers not-ok rather than throwing", async () => {
  // No `playlistText`, so the in-page fetch fails. The popup shows no picker
  // and the plain row still downloads.
  const background = await loadBackground({ native: true });
  const response = await background.send({
    type: "inspect-playlist",
    url: MASTER_URL,
    sourceUrl: "https://page.test/watch",
    tabId: 1
  });
  assert.equal(response.ok, false);
  assert.match(response.error, /Could not fetch the playlist/);
});

test("a host that does not enumerate is not asked to", async () => {
  const background = await loadBackground({
    native: true,
    playlistText: MASTER_TEXT,
    nativeOptions: { capabilities: { pause_resume: true, hls_info: true, playlist_info: false } }
  });
  const response = await background.send({
    type: "inspect-playlist",
    url: MASTER_URL,
    sourceUrl: "https://page.test/watch",
    tabId: 1
  });
  await background.settle();
  assert.equal(response.ok, false);
  assert.equal(
    background.nativePort().posted.some((m) => m?.command === "playlist-info"),
    false,
    "the handshake already said it cannot, so nothing is sent"
  );
});

test("a chosen rendition rides on the download request, and absent means unchanged", async () => {
  const background = await loadBackground({ native: true, playlistText: MASTER_TEXT });
  await background.send({
    type: "download-media",
    url: MASTER_URL,
    sourceUrl: "https://page.test/watch",
    tabId: 1,
    variantUrl: "https://cdn.test/v/low/video.m3u8"
  });
  await background.settle();
  const chosen = background.nativePort().posted.find((m) => m?.command === "download");
  assert.equal(chosen.variant_url, "https://cdn.test/v/low/video.m3u8");
  assert.equal(chosen.url, MASTER_URL, "the job is still about the master");

  const plain = await loadBackground({ native: true, playlistText: MASTER_TEXT });
  await plain.send({ type: "download-media", url: MASTER_URL, sourceUrl: "https://page.test/w", tabId: 1 });
  await plain.settle();
  const unchosen = plain.nativePort().posted.find((m) => m?.command === "download");
  assert.equal(unchosen.variant_url, null, "the host applies its own rule, as before");
});
