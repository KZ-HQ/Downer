"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");

const { NativeTaskChannel, TERMINAL_STATES } = require("../../extension/task-protocol.js");

/** Minimal stand-in for a browser.runtime.Port. */
class FakePort {
  constructor({ postMessageError = null } = {}) {
    this.posted = [];
    this.disconnected = false;
    this.postMessageError = postMessageError;
    this.messageListeners = [];
    this.disconnectListeners = [];
    this.onMessage = { addListener: (listener) => this.messageListeners.push(listener) };
    this.onDisconnect = { addListener: (listener) => this.disconnectListeners.push(listener) };
  }

  postMessage(message) {
    if (this.postMessageError) throw new Error(this.postMessageError);
    this.posted.push(message);
  }

  disconnect() {
    this.disconnected = true;
  }

  /** Deliver a message from the native host. */
  emit(message) {
    for (const listener of this.messageListeners) listener(message);
  }

  emitDisconnect() {
    for (const listener of this.disconnectListeners) listener();
  }
}

function channel(options = {}) {
  const port = new FakePort(options.port);
  const events = [];
  const task = new NativeTaskChannel(
    port,
    options.jobId || "job-1",
    (response) => events.push(response),
    options.disconnectError || (() => "native host disconnected")
  );
  return { port, events, task };
}

test("TERMINAL_STATES matches the native host's terminal state strings", () => {
  assert.deepEqual([...TERMINAL_STATES].sort(), ["cancelled", "completed", "failed"]);
});

test("start posts the request with the job id and returns the completion promise", async () => {
  const { port, task } = channel();
  const completion = task.start({ command: "download", url: "https://example.test/v.mp4" });
  assert.deepEqual(port.posted, [
    { command: "download", url: "https://example.test/v.mp4", job_id: "job-1" }
  ]);
  port.emit({ ok: true, job_id: "job-1", state: "completed", path: "/tmp/v.mp4" });
  assert.equal((await completion).path, "/tmp/v.mp4");
});

test("request correlates the acknowledgement by request_id", async () => {
  const { port, task } = channel();
  const pending = task.request("pause");
  assert.deepEqual(port.posted, [
    { command: "pause", job_id: "job-1", request_id: "job-1-1" }
  ]);

  // An unrelated response must not resolve the pending request.
  port.emit({ ok: true, job_id: "job-1", state: "downloading" });
  port.emit({ ok: true, job_id: "job-1", state: "paused", request_id: "job-1-1" });
  assert.deepEqual(await pending, {
    ok: true,
    job_id: "job-1",
    state: "paused",
    request_id: "job-1-1"
  });
});

test("request merges its payload and numbers request ids per channel", async () => {
  const { port, task } = channel();
  // Short timeouts keep the pending timers from holding the test runner open.
  task.request("hls-info", { total_segments: 4, total_duration_ms: 8000 }, 10);
  task.request("pause", {}, 10);
  assert.deepEqual(port.posted.map((message) => message.request_id), ["job-1-1", "job-1-2"]);
  assert.equal(port.posted[0].total_segments, 4);
  assert.equal(port.posted[0].total_duration_ms, 8000);
});

test("request resolves with a timeout message when no acknowledgement arrives", async () => {
  const { task } = channel();
  assert.deepEqual(await task.request("cancel", {}, 10), {
    ok: false,
    error: "Native host did not acknowledge cancel."
  });
});

test("request resolves with an error when the port cannot be written", async () => {
  const { task } = channel({ port: { postMessageError: "Attempt to postMessage on disconnected port" } });
  assert.deepEqual(await task.request("pause"), {
    ok: false,
    error: "Attempt to postMessage on disconnected port"
  });
});

test("a terminal state resolves completion and disconnects the port", async () => {
  const { port, events, task } = channel();
  const completion = task.start({ command: "download" });
  port.emit({ ok: true, job_id: "job-1", state: "downloading", percent: 25 });
  port.emit({ ok: false, job_id: "job-1", state: "failed", error: "FFmpeg failed" });

  const response = await completion;
  assert.equal(response.state, "failed");
  assert.equal(port.disconnected, true);
  assert.deepEqual(events.map((event) => event.state), ["downloading", "failed"]);
});

test("cancelled is terminal and pending requests resolve with an error", async () => {
  const { port, task } = channel();
  const completion = task.start({ command: "download" });
  const pending = task.request("cancel");
  port.emit({ ok: false, job_id: "job-1", state: "cancelled", error: "download cancelled" });

  assert.deepEqual(await pending, {
    ok: false,
    error: "Download task finished before the command was acknowledged."
  });
  assert.equal((await completion).state, "cancelled");
});

test("requests after the task settled are rejected without touching the port", async () => {
  const { port, task } = channel();
  const completion = task.start({ command: "download" });
  port.emit({ ok: true, job_id: "job-1", state: "completed", path: "/tmp/v.mp4" });
  await completion;

  const posted = port.posted.length;
  assert.deepEqual(await task.request("pause"), {
    ok: false,
    error: "Download task is no longer active."
  });
  assert.equal(port.posted.length, posted);
});

test("disconnecting before a terminal state rejects completion with disconnectError", async () => {
  const { port, task } = channel({ disconnectError: () => "No such native application" });
  const completion = task.start({ command: "download" });
  const pending = task.request("pause");
  port.emitDisconnect();

  await assert.rejects(completion, { message: "No such native application" });
  assert.deepEqual(await pending, { ok: false, error: "No such native application" });
});

test("disconnecting falls back to a default message when none is available", async () => {
  const { port, task } = channel({ disconnectError: () => undefined });
  const completion = task.start({ command: "download" });
  port.emitDisconnect();
  await assert.rejects(completion, { message: "native host disconnected" });
});

test("disconnecting after a terminal state does not reject the settled completion", async () => {
  const { port, task } = channel();
  const completion = task.start({ command: "download" });
  port.emit({ ok: true, job_id: "job-1", state: "completed", path: "/tmp/v.mp4" });
  port.emitDisconnect();
  assert.equal((await completion).state, "completed");
});

test("messages for another job are ignored", async () => {
  const { port, events, task } = channel();
  const completion = task.start({ command: "download" });
  port.emit({ ok: true, job_id: "other-job", state: "completed", path: "/tmp/other.mp4" });
  assert.deepEqual(events, []);
  assert.equal(port.disconnected, false);

  port.emit({ ok: true, job_id: "job-1", state: "completed", path: "/tmp/v.mp4" });
  assert.equal((await completion).path, "/tmp/v.mp4");
});

test("responses without a job id are delivered, matching the native host's failures", async () => {
  const { port, events, task } = channel();
  const completion = task.start({ command: "download" });
  port.emit({ ok: false, state: "failed", error: "unsupported native command: ping" });
  assert.equal(events.length, 1);
  assert.equal((await completion).error, "unsupported native command: ping");
});
