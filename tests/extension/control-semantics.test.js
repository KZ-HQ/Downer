"use strict";

/**
 * KEI-65: the extension's half of the control semantics.
 *
 * What each control guarantees is decided by the host and pinned over the wire
 * in `tests/native_host.rs`. What the extension owes is the two things the host
 * cannot know: whether the user wants a cancelled download's fragment kept, and
 * what the host said it could do. A field dropped on the way to the port, or a
 * capability thrown away after the handshake, is a failure here.
 *
 * See `docs/adr/0012-control-semantics.md`.
 */

const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

const { loadBackground } = require("./helpers/extension-dom.js");

const PROTOCOL = JSON.parse(
  fs.readFileSync(path.join(__dirname, "..", "fixtures", "protocol.json"), "utf8")
);

const MEDIA_URL = "https://example.test/hls/index.m3u8";

async function startedRequest(background) {
  const reply = await background.send({ type: "download-media", url: MEDIA_URL });
  await background.settle();
  const port = background.nativePort();
  assert.ok(port, "the background script opened a native port");
  return {
    jobId: reply.jobId,
    request: port.posted.find((posted) => posted?.command === "download")
  };
}

test("a cancelled download's fragment is not kept unless the user asked", async () => {
  const background = await loadBackground({ native: true });
  const { request } = await startedRequest(background);
  assert.equal(request.keep_partial, false);
  assert.equal(
    PROTOCOL.default_keep_partial,
    false,
    "and the wire vocabulary agrees on the default"
  );
});

test("turning the setting on sends keep_partial: true", async () => {
  const background = await loadBackground({ native: true, storage: { keepPartial: true } });
  const { request } = await startedRequest(background);
  assert.equal(request.keep_partial, true);
});

test("only an explicit true opts in, so a stray stored value cannot enable it", async () => {
  for (const stored of ["true", 1, {}, null]) {
    const background = await loadBackground({ native: true, storage: { keepPartial: stored } });
    const { request } = await startedRequest(background);
    assert.equal(request.keep_partial, false, `keepPartial ${JSON.stringify(stored)}`);
  }
});

test("the policy the download ran under is pinned onto the job", async () => {
  // The popup's account of what happened to the fragment has to match the
  // policy in force when the download started, not the setting as it stands
  // when the user reads the message.
  const background = await loadBackground({ native: true, storage: { keepPartial: true } });
  const { jobId } = await startedRequest(background);
  const { jobs } = await background.send({ type: "get-download-statuses" });
  assert.equal(jobs.find((job) => job.id === jobId).keepPartial, true);
});

test("what the host said it could do is kept, not discarded after the handshake", async () => {
  const background = await loadBackground({ native: true });
  const { jobId } = await startedRequest(background);
  const { jobs } = await background.send({ type: "get-download-statuses" });
  const job = jobs.find((entry) => entry.id === jobId);
  assert.deepEqual(job.capabilities, { pause_resume: true, hls_info: true });
  for (const capability of Object.keys(job.capabilities)) {
    assert.ok(
      PROTOCOL.capabilities.includes(capability),
      `${capability} is listed in tests/fixtures/protocol.json`
    );
  }
});

test("KEI-86: a metadata problem reaches the job as an explanation, not a failure", async () => {
  // The host says why the segment total is unavailable on a `progress` event:
  // the download is still running, and the probe is a convenience.
  const background = await loadBackground({ native: true });
  const { jobId } = await startedRequest(background);
  const port = background.nativePort();
  port.emit({
    protocol_version: 1,
    type: "progress",
    ok: true,
    job_id: jobId,
    state: "downloading",
    metadata_error:
      "Cloudflare challenge; provide a browser session cookie with --cookie or use a direct media URL"
  });
  await background.settle();

  const { jobs } = await background.send({ type: "get-download-statuses" });
  const job = jobs.find((entry) => entry.id === jobId);
  assert.match(job.metadataError, /Cloudflare challenge/);
  assert.equal(job.state, "downloading", "an unreadable playlist does not end the job");
});

test("KEI-86: elapsed time reaches the job even with no segment total", async () => {
  const background = await loadBackground({ native: true });
  const { jobId } = await startedRequest(background);
  background.nativePort().emit({
    protocol_version: 1,
    type: "progress",
    ok: true,
    job_id: jobId,
    state: "downloading",
    elapsed_ms: 754000
  });
  await background.settle();

  const { jobs } = await background.send({ type: "get-download-statuses" });
  const job = jobs.find((entry) => entry.id === jobId);
  assert.equal(job.elapsedMs, 754000);
  assert.equal(job.totalSegments, undefined, "and no total was invented");
});

test("KEI-86: a segment total arriving clears the explanation for its absence", async () => {
  const background = await loadBackground({ native: true });
  const { jobId } = await startedRequest(background);
  const port = background.nativePort();
  port.emit({
    protocol_version: 1, type: "progress", ok: true, job_id: jobId,
    state: "downloading", metadata_error: "the playlist could not be fetched: the request timed out"
  });
  await background.settle();
  port.emit({
    protocol_version: 1, type: "progress", ok: true, job_id: jobId,
    state: "downloading", total_segments: 42, completed_segments: 1, percent: 2.4
  });
  await background.settle();

  const { jobs } = await background.send({ type: "get-download-statuses" });
  const job = jobs.find((entry) => entry.id === jobId);
  assert.equal(job.totalSegments, 42);
  assert.equal(job.metadataError, null, "the question has been answered");
});
