"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");

const {
  NativeTaskChannel,
  TERMINAL_STATES,
  CONNECTION_STATES,
  PROTOCOL_VERSION,
  protocolCompatibility
} = require("../../extension/task-protocol.js");

/**
 * The shared wire vocabulary, read by this suite and by `tests/native_host.rs`,
 * so the Rust host and this client cannot rename a protocol term unilaterally.
 * `docs/protocol.md` is the prose contract.
 */
const protocol = require("../fixtures/protocol.json");

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
  assert.deepEqual([...TERMINAL_STATES].sort(), [...protocol.job_states.terminal].sort());
});

test("CONNECTION_STATES are the shared fixture's connection states and none is terminal", () => {
  assert.deepEqual([...CONNECTION_STATES].sort(), [...protocol.connection_states].sort());
  for (const state of CONNECTION_STATES) {
    assert.equal(TERMINAL_STATES.has(state), false, `${state} must not be terminal`);
  }
});

test("the client speaks the protocol version recorded in the shared fixture", () => {
  assert.equal(PROTOCOL_VERSION, protocol.protocol_version);
});

test("start posts the request with the job id and returns the completion promise", async () => {
  const { port, task } = channel();
  const completion = task.start({ command: "download", url: "https://example.test/v.mp4" });
  assert.deepEqual(port.posted, [
    {
      protocol_version: PROTOCOL_VERSION,
      command: "download",
      url: "https://example.test/v.mp4",
      job_id: "job-1",
      request_id: "job-1-start"
    }
  ]);
  port.emit({ ok: true, job_id: "job-1", state: "completed", path: "/tmp/v.mp4" });
  assert.equal((await completion).path, "/tmp/v.mp4");
});

test("request correlates the acknowledgement by request_id", async () => {
  const { port, task } = channel();
  const pending = task.request("pause");
  assert.deepEqual(port.posted, [
    {
      protocol_version: PROTOCOL_VERSION,
      command: "pause",
      job_id: "job-1",
      request_id: "job-1-1"
    }
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

test("an unattributed terminal state no longer ends the job it does not name", async () => {
  // Before ADR-0001 the host answered an unsupported command with `failed` and no
  // job_id, and this channel treated any `failed` as terminal for its own job — so
  // one bad control message ended a live download while FFmpeg kept running.
  const { port, events, task } = channel();
  const completion = task.start({ command: "download" });
  port.emit({ ok: false, state: "failed", error: "unsupported native command: ping" });

  assert.equal(events.length, 1, "the event is still delivered for diagnostics");
  assert.equal(port.disconnected, false, "but the channel is not settled");

  port.emit({ ok: true, job_id: "job-1", state: "completed", path: "/tmp/v.mp4" });
  assert.equal((await completion).path, "/tmp/v.mp4");
});

test("a rejected event is never terminal and resolves only its own request", async () => {
  const { port, events, task } = channel();
  const completion = task.start({ command: "download" });
  const pending = task.request("ping");
  port.emit({
    protocol_version: PROTOCOL_VERSION,
    type: "rejected",
    ok: false,
    state: "rejected",
    error_code: "unsupported_command",
    error: "unsupported native command: ping",
    request_id: "job-1-1"
  });

  assert.deepEqual(await pending, {
    protocol_version: PROTOCOL_VERSION,
    type: "rejected",
    ok: false,
    state: "rejected",
    error_code: "unsupported_command",
    error: "unsupported native command: ping",
    request_id: "job-1-1"
  });
  assert.equal(port.disconnected, false);
  assert.deepEqual(events.map((event) => event.state), ["rejected"]);

  port.emit({ ok: true, job_id: "job-1", state: "completed", path: "/tmp/v.mp4" });
  assert.equal((await completion).path, "/tmp/v.mp4");
});

test("a duplicate-job rejection naming the running job does not end it", async () => {
  // The host reports duplicate_job with the *running* job's id; only the state
  // vocabulary keeps this from terminating a healthy download.
  const { port, task } = channel();
  const completion = task.start({ command: "download" });
  port.emit({
    type: "rejected",
    ok: false,
    job_id: "job-1",
    state: "rejected",
    error_code: "duplicate_job",
    error: "download task already exists: job-1",
    request_id: "job-1-99"
  });
  assert.equal(port.disconnected, false);

  port.emit({ ok: true, job_id: "job-1", state: "completed", path: "/tmp/v.mp4" });
  assert.equal((await completion).path, "/tmp/v.mp4");
});

test("rejecting the start request itself fails the job", async () => {
  const { port, task } = channel();
  const completion = task.start({ command: "download" });
  port.emit({
    type: "rejected",
    ok: false,
    state: "rejected",
    error_code: "unsupported_protocol_version",
    error: "unsupported protocol version 99; this host speaks version 1",
    request_id: "job-1-start"
  });
  await assert.rejects(completion, {
    message: "unsupported protocol version 99; this host speaks version 1"
  });
});

test("hello posts the handshake without a job id", async () => {
  const { port, task } = channel();
  task.hello(10);
  assert.deepEqual(port.posted, [
    { protocol_version: PROTOCOL_VERSION, command: "hello", request_id: "job-1-1" }
  ]);
  assert.equal("job_id" in port.posted[0], false);
});

test("hello resolves with the host's handshake answer", async () => {
  const { port, task } = channel();
  const pending = task.hello();
  port.emit({
    protocol_version: PROTOCOL_VERSION,
    type: "hello",
    ok: true,
    state: "ready",
    host_version: "0.3.0",
    capabilities: { pause_resume: true, hls_info: true },
    request_id: "job-1-1"
  });
  const compatibility = protocolCompatibility(await pending);
  assert.equal(compatibility.ok, true);
  assert.equal(compatibility.hostVersion, "0.3.0");
  assert.deepEqual(compatibility.capabilities, { pause_resume: true, hls_info: true });
});

test("protocolCompatibility accepts a matching host and reports its capabilities", () => {
  const compatibility = protocolCompatibility({
    ok: true,
    protocol_version: PROTOCOL_VERSION,
    host_version: "0.3.0",
    capabilities: { pause_resume: false, hls_info: true }
  });
  assert.deepEqual(compatibility, {
    ok: true,
    hostVersion: "0.3.0",
    capabilities: { pause_resume: false, hls_info: true }
  });
});

test("protocolCompatibility refuses a mismatched host and names both versions", () => {
  for (const version of [0, 2, 99]) {
    const compatibility = protocolCompatibility({ ok: true, protocol_version: version });
    assert.equal(compatibility.ok, false);
    assert.match(compatibility.error, new RegExp(`version ${version}\\b`));
    assert.match(compatibility.error, new RegExp(`needs version ${PROTOCOL_VERSION}`));
    assert.match(compatibility.error, /make extension/);
  }
});

test("protocolCompatibility refuses a host that reports no protocol version", () => {
  for (const hello of [{ ok: true }, { ok: true, protocol_version: null }, { ok: true, protocol_version: "1" }]) {
    const compatibility = protocolCompatibility(hello);
    assert.equal(compatibility.ok, false);
    assert.match(compatibility.error, /too old/);
  }
});

test("protocolCompatibility surfaces a handshake that never answered", () => {
  assert.equal(
    protocolCompatibility({ ok: false, error: "Native host did not acknowledge hello." }).error,
    "Native host did not acknowledge hello."
  );
  assert.match(protocolCompatibility(undefined).error, /did not answer the protocol handshake/);
});

test("close fails an unstarted channel and disconnects the port", async () => {
  const { port, task } = channel();
  task.close("The Downer native host speaks protocol version 99.");
  assert.equal(port.disconnected, true);
  await assert.rejects(task.completion, {
    message: "The Downer native host speaks protocol version 99."
  });
  assert.deepEqual(await task.request("pause"), {
    ok: false,
    error: "Download task is no longer active."
  });
});
