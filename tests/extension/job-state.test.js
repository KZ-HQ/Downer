"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");

const JobState = require("../../extension/job-state.js");
const {
  TERMINAL_STATES,
  WIRE_TERMINAL_STATE_SET,
  ACTIVE_STATES,
  ALL_STATES,
  TRANSITIONS,
  INTERRUPTED_ERROR,
  isState,
  isTerminal,
  isActive,
  canTransition,
  showsProgress,
  isBusy,
  canPause,
  canResume,
  canCancel,
  reconcileRestoredJobs
} = JobState;

/** The shared wire vocabulary, also read by tests/native_host.rs. */
const protocol = require("../fixtures/protocol.json");

test("the wire states come from the shared protocol fixture", () => {
  assert.deepEqual(
    [...JobState.WIRE_ACTIVE_STATES].sort(),
    [...protocol.job_states.active].sort()
  );
  assert.deepEqual(
    [...JobState.WIRE_TERMINAL_STATES].sort(),
    [...protocol.job_states.terminal].sort()
  );
});

test("the extension-only states are declared as such in the protocol fixture", () => {
  const extensionOnly = [
    ...JobState.EXTENSION_ACTIVE_STATES,
    ...JobState.EXTENSION_TERMINAL_STATES
  ].sort();
  assert.deepEqual(extensionOnly, [...protocol.job_states.extension_only].sort());
  // The host must never see them.
  for (const state of extensionOnly) {
    assert.ok(!protocol.job_states.active.includes(state), state);
    assert.ok(!protocol.job_states.terminal.includes(state), state);
  }
});

test("the extension's terminal set is wider than the wire's, by exactly interrupted", () => {
  assert.deepEqual([...TERMINAL_STATES].sort(), ["cancelled", "completed", "failed", "interrupted"]);
  assert.deepEqual([...WIRE_TERMINAL_STATE_SET].sort(), ["cancelled", "completed", "failed"]);
  assert.ok(!WIRE_TERMINAL_STATE_SET.has("interrupted"));
});

test("task-protocol settles channels on the wire terminal set only", () => {
  // Derived, not duplicated: a channel must never be settled by `interrupted`.
  const { TERMINAL_STATES: channelTerminal } = require("../../extension/task-protocol.js");
  assert.equal(channelTerminal, WIRE_TERMINAL_STATE_SET);
});

test("every state is either active or terminal, never both", () => {
  for (const state of ALL_STATES) {
    assert.notEqual(isActive(state), isTerminal(state), state);
  }
  assert.equal(ACTIVE_STATES.size + TERMINAL_STATES.size, ALL_STATES.size);
});

test("every state has a transition list and every destination is a real state", () => {
  for (const state of ALL_STATES) {
    assert.ok(Array.isArray(TRANSITIONS[state]), `${state} has a transition list`);
    for (const destination of TRANSITIONS[state]) {
      assert.ok(isState(destination), `${state} -> ${destination}`);
    }
  }
});

test("a job may only open in starting", () => {
  assert.equal(canTransition(undefined, "starting"), true);
  assert.equal(canTransition(null, "starting"), true);
  for (const state of ALL_STATES) {
    if (state === "starting") continue;
    assert.equal(canTransition(undefined, state), false, `opened in ${state}`);
  }
});

test("the real download lifecycle is a legal path", () => {
  // Exactly what background.js produces: beginDownload -> runDownload ->
  // the host's first progress event -> progress -> terminal.
  const lifecycle = [undefined, "starting", "preparing", "starting", "downloading", "completed"];
  for (let i = 1; i < lifecycle.length; i += 1) {
    assert.equal(canTransition(lifecycle[i - 1], lifecycle[i]), true, `${lifecycle[i - 1]} -> ${lifecycle[i]}`);
  }
});

test("pause, resume and cancel are legal paths", () => {
  assert.equal(canTransition("downloading", "paused"), true);
  assert.equal(canTransition("paused", "downloading"), true);
  assert.equal(canTransition("downloading", "cancelling"), true);
  assert.equal(canTransition("paused", "cancelling"), true);
  assert.equal(canTransition("cancelling", "cancelled"), true);
});

test("terminal states are final — nothing may leave them", () => {
  for (const terminal of TERMINAL_STATES) {
    for (const state of ALL_STATES) {
      if (state === terminal) continue;
      assert.equal(canTransition(terminal, state), false, `${terminal} -> ${state}`);
    }
  }
});

test("a late or duplicated event cannot revive a finished job", () => {
  // The concrete case: a progress event arriving after the terminal event.
  assert.equal(canTransition("completed", "downloading"), false);
  assert.equal(canTransition("cancelled", "downloading"), false);
  assert.equal(canTransition("failed", "paused"), false);
  assert.equal(canTransition("interrupted", "downloading"), false);
});

test("re-applying the same state is allowed, so repeated events are harmless", () => {
  for (const state of ALL_STATES) {
    assert.equal(canTransition(state, state), true, state);
  }
});

test("every active state may be interrupted, and no terminal one may", () => {
  for (const state of ACTIVE_STATES) {
    assert.equal(canTransition(state, "interrupted"), true, state);
  }
  for (const state of TERMINAL_STATES) {
    if (state === "interrupted") continue;
    assert.equal(canTransition(state, "interrupted"), false, state);
  }
});

test("unknown states are never legal in either direction", () => {
  assert.equal(canTransition("downloading", "sleeping"), false);
  assert.equal(canTransition("sleeping", "downloading"), false);
  assert.equal(canTransition("downloading", undefined), false);
  assert.equal(isState("sleeping"), false);
  assert.equal(isState(undefined), false);
});

test("cancelling may not go back to downloading", () => {
  // Once cancel is acknowledged the job is on its way out; a stray progress
  // event must not make it look live again.
  assert.equal(canTransition("cancelling", "downloading"), false);
  assert.equal(canTransition("cancelling", "paused"), false);
});

// --- Presentation predicates -------------------------------------------------

test("presentation predicates agree with the state sets", () => {
  for (const state of ACTIVE_STATES) {
    assert.equal(isBusy(state), true, state);
    assert.equal(showsProgress(state), true, state);
  }
  assert.equal(showsProgress("completed"), true);
  for (const state of ["cancelled", "failed", "interrupted"]) {
    assert.equal(isBusy(state), false, state);
    assert.equal(showsProgress(state), false, state);
  }
});

test("an interrupted job offers no controls, so nothing is wired to a dead task", () => {
  assert.equal(canPause("interrupted"), false);
  assert.equal(canResume("interrupted"), false);
  assert.equal(canCancel("interrupted"), false);
  assert.equal(isBusy("interrupted"), false);
});

test("controls are offered exactly where they make sense", () => {
  assert.deepEqual([...ALL_STATES].filter(canPause).sort(), ["downloading", "preparing", "starting"]);
  assert.deepEqual([...ALL_STATES].filter(canResume), ["paused"]);
  // `retrying` cancels but does not pause, and the asymmetry is deliberate:
  // between attempts there is no FFmpeg process, so Pause could only pretend,
  // while the host checks for a cancel throughout the backoff (ADR-0023).
  assert.deepEqual(
    [...ALL_STATES].filter(canCancel).sort(),
    ["downloading", "paused", "preparing", "retrying", "starting"]
  );
  assert.ok(!canPause("retrying"), "no process to signal between attempts");
});

// --- Reconciliation ----------------------------------------------------------

test("a job stored in any active state is reconciled to interrupted", () => {
  for (const state of ACTIVE_STATES) {
    const [job] = reconcileRestoredJobs([{ id: "j", url: "u", state }], 1234);
    assert.equal(job.state, "interrupted", state);
    assert.equal(job.error, INTERRUPTED_ERROR, state);
    assert.equal(job.finishedAt, 1234, state);
  }
});

test("a job stored in a terminal state is passed through untouched", () => {
  for (const state of TERMINAL_STATES) {
    const original = { id: "j", url: "u", state, finishedAt: 7 };
    const [job] = reconcileRestoredJobs([original], 1234);
    assert.equal(job, original, `${state} is the same object, not a copy`);
  }
});

test("reconciliation is idempotent", () => {
  const once = reconcileRestoredJobs([{ id: "j", url: "u", state: "downloading" }], 1);
  const twice = reconcileRestoredJobs(once, 2);
  assert.equal(twice[0], once[0], "a second pass changes nothing");
  assert.equal(twice[0].finishedAt, 1);
});

test("a record with no state, or an unknown one, is reconciled rather than trusted", () => {
  // Backward compatibility: records written before `interrupted` existed.
  for (const state of [undefined, null, "sleeping"]) {
    const [job] = reconcileRestoredJobs([{ id: "j", url: "u", state }], 5);
    assert.equal(job.state, "interrupted", String(state));
  }
});

test("an existing error is kept, so the original failure is not overwritten", () => {
  const [job] = reconcileRestoredJobs(
    [{ id: "j", url: "u", state: "downloading", error: "disk full" }],
    5
  );
  assert.equal(job.error, "disk full");
  assert.equal(job.state, "interrupted");
});

test("progress and logs survive reconciliation", () => {
  const [job] = reconcileRestoredJobs([{
    id: "j",
    url: "u",
    state: "downloading",
    completedSegments: 3,
    totalSegments: 10,
    logs: [{ at: 1, text: "line" }]
  }], 5);
  assert.equal(job.completedSegments, 3);
  assert.equal(job.totalSegments, 10);
  assert.deepEqual(job.logs, [{ at: 1, text: "line" }]);
});

test("malformed records are dropped rather than restored", () => {
  assert.deepEqual(reconcileRestoredJobs([null, undefined, {}, { url: "u" }]), []);
  assert.deepEqual(reconcileRestoredJobs(undefined), []);
  assert.deepEqual(reconcileRestoredJobs([]), []);
});

test("a mixed store reconciles only what was running", () => {
  const stored = [
    { id: "a", url: "u1", state: "completed", path: "/tmp/a" },
    { id: "b", url: "u2", state: "downloading" },
    { id: "c", url: "u3", state: "cancelled" },
    { id: "d", url: "u4", state: "paused" }
  ];
  const restored = reconcileRestoredJobs(stored, 9);
  assert.deepEqual(restored.map((job) => job.state), [
    "completed",
    "interrupted",
    "cancelled",
    "interrupted"
  ]);
  for (const job of restored) assert.equal(isTerminal(job.state), true, job.id);
});

test("KEI-66: retrying is an active wire state, never terminal", () => {
  // The distinction the popup rests on: an active job keeps its controls and
  // its progress row, a terminal one is finished. A retry is between attempts,
  // so it is active (ADR-0023).
  assert.ok(isState("retrying"));
  assert.ok(isActive("retrying"));
  assert.ok(!isTerminal("retrying"));
  assert.ok(
    protocol.job_states.active.includes("retrying"),
    "retrying is in tests/fixtures/protocol.json, which the Rust suite reads too"
  );
});

test("KEI-66: a download may retry and come back, or run out of attempts", () => {
  assert.ok(canTransition("downloading", "retrying"));
  // The next attempt starting.
  assert.ok(canTransition("retrying", "downloading"));
  // The attempts running out.
  assert.ok(canTransition("retrying", "failed"));
  // A cancel during the backoff.
  assert.ok(canTransition("retrying", "cancelling"));
  assert.ok(canTransition("retrying", "cancelled"));
  // And a job cannot open in it: a retry presupposes an attempt.
  assert.ok(!canTransition(undefined, "retrying"));
});
