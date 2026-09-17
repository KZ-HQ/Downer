"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");

const {
  TERMINAL_STATES,
  RENDER_LIVE,
  RENDER_STALE,
  RENDER_HISTORY,
  classifyPersistedJobs,
  renderableJobs
} = require("../../extension/job-view.js");

const PAGE_MEDIA = "https://example.test/media/feature.mp4";
const OTHER_MEDIA = "https://other.test/yesterday.mp4";

function classify(jobs, options = {}) {
  return classifyPersistedJobs({
    jobs,
    candidateUrls: options.candidateUrls || [PAGE_MEDIA],
    sessionJobIds: options.sessionJobIds || []
  });
}

test("TERMINAL_STATES is the extension's terminal set, from the shared state machine", () => {
  // Wider than the protocol's on purpose: `interrupted` ends a job for the
  // extension but never appears on the wire. See extension/job-state.js.
  const JobState = require("../../extension/job-state.js");
  assert.equal(TERMINAL_STATES, JobState.TERMINAL_STATES);
  assert.deepEqual([...TERMINAL_STATES].sort(), ["cancelled", "completed", "failed", "interrupted"]);
  assert.deepEqual([...JobState.WIRE_TERMINAL_STATE_SET].sort(), ["cancelled", "completed", "failed"]);
});

test("a terminal job from an earlier session for another page is history", () => {
  // The reported bug: this job had no row, so the popup's row lookup skipped the
  // per-row branch — and then set the headline to "Download cancelled." anyway,
  // for a page on which nothing had been downloaded.
  const [decision] = classify([
    { id: "old-1", url: OTHER_MEDIA, state: "cancelled", error: "Download cancelled." }
  ]);
  assert.equal(decision.render, RENDER_HISTORY);
  assert.equal(decision.reason, "not-on-this-page");
  assert.deepEqual(renderableJobs({
    jobs: [{ id: "old-1", url: OTHER_MEDIA, state: "cancelled" }],
    candidateUrls: [PAGE_MEDIA]
  }), []);
});

test("with several stored jobs none of them can set the headline", () => {
  // Previously the last job in the list won, whatever page it belonged to.
  const stored = [
    { id: "old-1", url: OTHER_MEDIA, state: "completed" },
    { id: "old-2", url: "https://other.test/a.mp4", state: "failed" },
    { id: "old-3", url: "https://other.test/b.mp4", state: "cancelled" }
  ];
  assert.deepEqual(renderableJobs({ jobs: stored, candidateUrls: [PAGE_MEDIA] }), []);
});

test("a job reconciled to interrupted is stale, not live", () => {
  // The second failure mode from the issue, now carrying a real state: its URL
  // does match a candidate, so the old code wrote "Downloading…" into the live
  // row, disabled Download, and showed Pause/Cancel wired to a native task that
  // no longer exists.
  const [decision] = classify([{ id: "interrupted-1", url: PAGE_MEDIA, state: "interrupted" }]);
  assert.equal(decision.render, RENDER_STALE);
  assert.equal(decision.reason, "interrupted");
});

test("a job left non-terminal by an older version is stale too", () => {
  // Startup reconciliation (KEI-56) turns these into `interrupted` before the
  // popup ever sees them; this is the defensive path for a record written
  // before reconciliation existed, which must still never present as live.
  for (const state of ["starting", "preparing", "downloading", "paused", "cancelling"]) {
    const [decision] = classify([{ id: "stale-1", url: PAGE_MEDIA, state }]);
    assert.equal(decision.render, RENDER_STALE, state);
    assert.equal(decision.reason, "unreconciled", state);
  }
});

test("a job begun in this session is live and renders normally", () => {
  const [decision] = classify(
    [{ id: "live-1", url: PAGE_MEDIA, state: "downloading" }],
    { sessionJobIds: ["live-1"] }
  );
  assert.equal(decision.render, RENDER_LIVE);
  assert.equal(decision.reason, "this-session");
});

test("a job finished in this session still sets the headline", () => {
  // Restoring after a completed download in the same session must keep saying so.
  const [decision] = classify(
    [{ id: "live-1", url: PAGE_MEDIA, state: "completed", path: "/tmp/feature.mp4" }],
    { sessionJobIds: ["live-1"] }
  );
  assert.equal(decision.render, RENDER_LIVE);
});

test("a job finished in an earlier session for this page is history", () => {
  // `interrupted` is excluded deliberately: it is terminal, but it is about this
  // page and carries an explanation the user should see.
  for (const state of ["completed", "failed", "cancelled"]) {
    const [decision] = classify([{ id: "old-1", url: PAGE_MEDIA, state }]);
    assert.equal(decision.render, RENDER_HISTORY, state);
    assert.equal(decision.reason, "earlier-session", state);
  }
});

test("a session job about a page the user is not looking at stays history", () => {
  // A download running in another tab must not narrate this popup.
  const [decision] = classify(
    [{ id: "live-1", url: OTHER_MEDIA, state: "downloading" }],
    { sessionJobIds: ["live-1"] }
  );
  assert.equal(decision.render, RENDER_HISTORY);
  assert.equal(decision.reason, "not-on-this-page");
});

test("renderableJobs drops history and keeps live and stale in order", () => {
  const decisions = renderableJobs({
    jobs: [
      { id: "old-1", url: OTHER_MEDIA, state: "cancelled" },
      { id: "interrupted-1", url: PAGE_MEDIA, state: "downloading" },
      { id: "old-2", url: PAGE_MEDIA, state: "completed" },
      { id: "live-1", url: PAGE_MEDIA, state: "downloading" }
    ],
    candidateUrls: [PAGE_MEDIA],
    sessionJobIds: ["live-1"]
  });
  assert.deepEqual(
    decisions.map((decision) => [decision.job.id, decision.render]),
    [["interrupted-1", RENDER_STALE], ["live-1", RENDER_LIVE]]
  );
});

test("malformed and empty input is ignored rather than thrown on", () => {
  assert.deepEqual(classifyPersistedJobs(), []);
  assert.deepEqual(classifyPersistedJobs({ jobs: [null, undefined, {}, { url: PAGE_MEDIA }] }), []);
  const [decision] = classify([{ id: "no-url" }]);
  assert.equal(decision.render, RENDER_HISTORY);
});

test("classification never invents or renames a job state", () => {
  // The extension job state machine belongs to KEI-56. This module decides how
  // to render a job, and must leave the job itself untouched.
  const job = { id: "interrupted-1", url: PAGE_MEDIA, state: "downloading" };
  const [decision] = classify([job]);
  assert.equal(decision.job.state, "downloading");
  assert.equal(decision.job, job, "the job object is passed through, not rewritten");
  assert.deepEqual(Object.keys(decision).sort(), ["job", "reason", "render"]);
});
