"use strict";

/**
 * KEI-55, driven through the real `extension/background.js`.
 *
 * The reported defect: `appendJobLog` called `updateJob` for every FFmpeg stderr
 * line, and `updateJob` broadcast a full job record and scheduled a
 * `storage.local.set` of *all* jobs. FFmpeg's HLS demuxer logs one
 * `Opening '<url>' for reading` line per segment at `-loglevel info`, so a
 * 2,000-segment stream meant thousands of full-state writes, thousands of popup
 * re-renders, and thousands of signed segment URLs persisted until the user
 * pressed "Clear logs".
 *
 * Every token here is a sentinel. There is no real secret in this file, and the
 * assertions are greps for those sentinels — the same shape of test as
 * `tests/native_host.rs::no_host_event_carries_the_cookie_value`.
 */

const test = require("node:test");
const assert = require("node:assert/strict");

const { loadBackground } = require("./helpers/extension-dom.js");
const { MAX_LOG_LINES } = require("../../extension/job-logs.js");

const TOKEN = "SENTINEL-SIGNED-TOKEN-MUST-NOT-PERSIST";
const MEDIA_URL = "https://cdn.example.test/hls/index.m3u8";
const STREAM_LINES = 2000;

/** What FFmpeg's HLS demuxer logs for segment `index`, token and all. */
function segmentLine(index) {
  return `[hls @ 0x7f8] Opening 'https://cdn.example.test/hls/seg${index}.ts?token=${TOKEN}&e=1790000000' for reading`;
}

/** Start a real job and hand back the port the background script opened. */
async function startJob(background, url = MEDIA_URL) {
  const reply = await background.send({ type: "download-media", url });
  await background.settle();
  const port = background.nativePort();
  assert.ok(port, "the background script opened a native port");
  return { jobId: reply.jobId, port };
}

function logEvent(jobId, line) {
  return {
    protocol_version: 1,
    type: "log",
    ok: true,
    job_id: jobId,
    state: "downloading",
    log: line
  };
}

test("a 2,000-line stream costs a bounded number of storage writes", async () => {
  const background = await loadBackground({ native: true });
  const before = background.writes();
  const { jobId, port } = await startJob(background);

  for (let index = 0; index < STREAM_LINES; index += 1) {
    port.emit(logEvent(jobId, segmentLine(index)));
  }
  await background.flush();

  const writes = background.writes() - before;
  // The bound, not the exact number: what matters is that it does not scale
  // with the stream. Before this change it was one write per line, plus the
  // job's own; 2,000 lines cost upwards of 2,000 writes.
  assert.ok(
    writes <= 10,
    `${STREAM_LINES} log lines cost ${writes} storage writes, which is not bounded`
  );
});

test("a 2,000-line stream costs a bounded number of popup broadcasts", async () => {
  const background = await loadBackground({ native: true });
  const { jobId, port } = await startJob(background);
  const before = background.broadcasts.length;

  for (let index = 0; index < STREAM_LINES; index += 1) {
    port.emit(logEvent(jobId, segmentLine(index)));
  }
  await background.flush();

  const logBroadcasts = background.broadcasts
    .slice(before)
    .filter((message) => message?.type === "download-log");
  assert.ok(
    logBroadcasts.length > 0 && logBroadcasts.length <= 10,
    `${STREAM_LINES} log lines produced ${logBroadcasts.length} broadcasts`
  );
  // And they are their own message type, so the popup — which listens for
  // `download-status` only — is not re-rendered per line at all.
  const statusBroadcasts = background.broadcasts
    .slice(before)
    .filter((message) => message?.type === "download-status");
  assert.equal(statusBroadcasts.length, 0, "no job-state broadcast was sent for a log line");
});

test("every line is kept, up to the 500-line cap, newest last", async () => {
  const background = await loadBackground({ native: true });
  const { jobId, port } = await startJob(background);

  for (let index = 0; index < STREAM_LINES; index += 1) {
    port.emit(logEvent(jobId, segmentLine(index)));
  }
  await background.flush();

  const { logs } = await background.send({ type: "get-download-logs" });
  const entries = logs[jobId];
  assert.equal(entries.length, MAX_LOG_LINES, "batching did not drop lines below the cap");
  assert.ok(
    entries[entries.length - 1].text.includes(`seg${STREAM_LINES - 1}.ts`),
    "the newest line is the last one"
  );
  assert.ok(entries[0].text.includes(`seg${STREAM_LINES - MAX_LOG_LINES}.ts`));
});

test("no signed token reaches storage, in any key", async () => {
  const background = await loadBackground({ native: true });
  const { jobId, port } = await startJob(background);

  for (let index = 0; index < 50; index += 1) {
    port.emit(logEvent(jobId, segmentLine(index)));
  }
  await background.flush();

  const serialized = JSON.stringify(background.storage());
  assert.equal(serialized.includes(TOKEN), false, "a signed token was persisted");
  // The line is still useful: host and path survive, only the query is gone.
  const { logs } = await background.send({ type: "get-download-logs" });
  assert.equal(
    logs[jobId][0].text,
    "[hls @ 0x7f8] Opening 'https://cdn.example.test/hls/seg0.ts?…' for reading"
  );
});

test("no signed token reaches the popup either", async () => {
  const background = await loadBackground({ native: true });
  const { jobId, port } = await startJob(background);
  port.emit(logEvent(jobId, segmentLine(1)));
  await background.flush();

  assert.equal(
    JSON.stringify(background.broadcasts).includes(TOKEN),
    false,
    "a signed token was broadcast"
  );
});

test("an error message carrying a token is redacted before it is stored", async () => {
  const background = await loadBackground({ native: true });
  const { jobId, port } = await startJob(background);

  port.emit({
    protocol_version: 1,
    type: "terminal",
    ok: false,
    job_id: jobId,
    state: "failed",
    error: `FFmpeg failed with status 1: HTTP error 403 Forbidden for https://cdn.example.test/hls/key.bin?token=${TOKEN}`,
    error_code: "download_failed"
  });
  await background.flush();

  const [job] = background.stored();
  assert.equal(job.state, "failed");
  assert.equal(job.error.includes(TOKEN), false, "a token survived in the persisted error");
  assert.ok(job.error.includes("https://cdn.example.test/hls/key.bin?…"), job.error);
  // The popup renders `job.error` verbatim, so redacting it at the source
  // covers display as well as persistence.
  assert.equal(JSON.stringify(background.broadcasts).includes(TOKEN), false);
});

test("logs are stored under their own key, not inside the job record", async () => {
  const background = await loadBackground({ native: true });
  const { jobId, port } = await startJob(background);
  port.emit(logEvent(jobId, segmentLine(7)));
  await background.flush();

  const storage = background.storage();
  assert.ok(Array.isArray(storage[`downloadLogs:${jobId}`]), "the split key exists");
  const [job] = storage.downloadJobs;
  assert.equal("logs" in job, false, "the job record no longer carries its logs");
});

test("a terminal state is persisted without waiting for the coalescing window", async () => {
  const background = await loadBackground({ native: true });
  const { jobId, port } = await startJob(background);

  port.emit({
    protocol_version: 1,
    type: "terminal",
    ok: true,
    job_id: jobId,
    state: "completed",
    path: "/tmp/out.mp4"
  });
  // Deliberately no `flush()`: settling microtasks is all a terminal state gets.
  await background.settle();

  const [job] = background.stored();
  assert.equal(job.state, "completed", "the finished job was written immediately");
});

test("clearing logs empties the split keys and tells the Settings page", async () => {
  const background = await loadBackground({ native: true });
  const { jobId, port } = await startJob(background);
  port.emit(logEvent(jobId, segmentLine(3)));
  await background.flush();

  const reply = await background.send({ type: "clear-download-logs" });
  assert.deepEqual(reply, { ok: true });
  await background.settle();

  assert.equal(background.storage()[`downloadLogs:${jobId}`], undefined);
  const { logs } = await background.send({ type: "get-download-logs" });
  assert.deepEqual(logs[jobId], []);
  assert.ok(
    background.broadcasts.some((message) => message?.type === "download-logs-cleared"),
    "the Settings page is told, so its view does not keep showing cleared lines"
  );
});

test("job status replies carry no logs, so the popup is not sent them at all", async () => {
  const background = await loadBackground({ native: true });
  const { jobId, port } = await startJob(background);
  port.emit(logEvent(jobId, segmentLine(4)));
  await background.flush();

  const { jobs } = await background.send({ type: "get-download-statuses" });
  assert.equal(jobs.length, 1);
  assert.equal("logs" in jobs[0], false);
  assert.equal(jobs[0].id, jobId);
});

/**
 * Migration, for records written before KEI-55 split logs out of `downloadJobs`.
 *
 * The decision (see the KEI-55 handoff) is to migrate on load and redact on the
 * way, rather than dropping the history or leaving it in place: a line persisted
 * before redaction existed is precisely the leak this issue is about, and
 * leaving it until the user presses "Clear logs" is what the old behaviour did.
 */

const LEGACY_JOB = {
  id: "legacy",
  url: MEDIA_URL,
  state: "completed",
  path: "/tmp/a.mp4",
  finishedAt: 11
};

test("inline logs from an older record are moved to their own key and redacted", async () => {
  const background = await loadBackground({
    downloadJobs: [
      {
        ...LEGACY_JOB,
        logs: [
          { at: 1, text: segmentLine(1) },
          { at: 2, text: segmentLine(2) }
        ]
      }
    ]
  });
  await background.settle();

  const storage = background.storage();
  assert.equal("logs" in storage.downloadJobs[0], false, "the job record was rewritten without them");
  const migrated = storage["downloadLogs:legacy"];
  assert.equal(migrated.length, 2, "the history was kept, not dropped");
  assert.equal(JSON.stringify(storage).includes(TOKEN), false, "migration redacts on the way");
  assert.ok(migrated[0].text.includes("seg1.ts?…"));
  assert.equal(migrated[0].at, 1, "the original timestamps survive");
});

test("the migration runs once: a second start rewrites nothing", async () => {
  const first = await loadBackground({
    downloadJobs: [{ ...LEGACY_JOB, logs: [{ at: 1, text: segmentLine(1) }] }]
  });
  await first.settle();
  const migrated = first.storage();

  const second = await loadBackground({
    downloadJobs: migrated.downloadJobs,
    storage: { "downloadLogs:legacy": migrated["downloadLogs:legacy"] }
  });
  const writesBefore = second.writes();
  await second.settle();
  assert.equal(second.writes(), writesBefore, "an already-migrated store is not rewritten");

  const { logs } = await second.send({ type: "get-download-logs" });
  assert.equal(logs.legacy.length, 1, "the migrated logs are served to the Settings page");
});

test("a token persisted in an older job's error field is scrubbed on load", async () => {
  const background = await loadBackground({
    downloadJobs: [
      {
        id: "legacy",
        url: MEDIA_URL,
        state: "failed",
        finishedAt: 11,
        error: `HTTP error 403 Forbidden for https://cdn.example.test/hls/key.bin?token=${TOKEN}`
      }
    ]
  });
  await background.settle();

  const [job] = background.stored();
  assert.equal(job.error.includes(TOKEN), false);
  assert.ok(job.error.endsWith("https://cdn.example.test/hls/key.bin?…"), job.error);
});

test("a log key left by a job that fell off the history is deleted on load", async () => {
  const background = await loadBackground({
    downloadJobs: [LEGACY_JOB],
    storage: { "downloadLogs:gone": [{ at: 1, text: "orphan" }] }
  });
  await background.settle();
  assert.equal("downloadLogs:gone" in background.storage(), false);
});

test("logs of a job pushed off the end of the 20-job history are dropped with it", async () => {
  // MAX_SAVED_JOBS is 20. Twenty-one jobs means the oldest is not persisted, and
  // its log key must not outlive it or the split keys grow without bound.
  const background = await loadBackground({ native: true });
  const first = await startJob(background, "https://cdn.example.test/hls/0.m3u8");
  first.port.emit(logEvent(first.jobId, segmentLine(0)));
  await background.flush();
  assert.ok(`downloadLogs:${first.jobId}` in background.storage());

  for (let index = 1; index <= 20; index += 1) {
    await startJob(background, `https://cdn.example.test/hls/${index}.m3u8`);
  }
  await background.flush();

  const storage = background.storage();
  assert.equal(storage.downloadJobs.length, 20);
  assert.equal(
    storage.downloadJobs.some((job) => job.id === first.jobId),
    false,
    "the oldest job fell off the history"
  );
  assert.equal(`downloadLogs:${first.jobId}` in storage, false, "its log key went with it");
});
