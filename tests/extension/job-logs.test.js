"use strict";

/**
 * The caps on persisted logs (`extension/job-logs.js`).
 *
 * AGENTS.md pins the per-download log limit at 500 lines. KEI-55 adds a byte
 * cap alongside it, because 500 lines is not a bound on size: one line can be
 * arbitrarily long, and the whole point of the change is that `storage.local`
 * stops growing without limit during a long HLS download.
 */

const test = require("node:test");
const assert = require("node:assert/strict");

const logs = require("../../extension/job-logs.js");

function entries(count, text = "line") {
  return Array.from({ length: count }, (_, index) => ({ at: index, text: `${text} ${index}` }));
}

test("the 500-line cap AGENTS.md pins still holds, keeping the newest", () => {
  const trimmed = logs.trimLogEntries(entries(2000));
  assert.equal(trimmed.length, logs.MAX_LOG_LINES);
  assert.equal(trimmed[trimmed.length - 1].text, "line 1999");
  assert.equal(trimmed[0].text, "line 1500");
});

test("fewer entries than the cap are returned untouched", () => {
  const input = entries(3);
  assert.deepEqual(logs.trimLogEntries(input), input);
});

test("the byte cap drops oldest entries even when the line count is legal", () => {
  // 100 lines of 4 KiB is 400 KiB, well inside 500 lines and well outside the
  // byte budget.
  const fat = Array.from({ length: 100 }, (_, index) => ({ at: index, text: "x".repeat(4096) }));
  const trimmed = logs.trimLogEntries(fat);
  assert.ok(trimmed.length < fat.length, "something was dropped");
  const bytes = trimmed.reduce((sum, entry) => sum + logs.entryBytes(entry), 0);
  assert.ok(bytes <= logs.MAX_LOG_BYTES, `kept ${bytes} bytes, cap is ${logs.MAX_LOG_BYTES}`);
  assert.equal(trimmed[trimmed.length - 1], fat[fat.length - 1], "the newest line is kept");
});

test("a single line larger than the whole budget is kept rather than vanishing", () => {
  // Dropping the line that just arrived would make a pathological line silently
  // invisible. Keeping it merely means the next append drops it, and
  // `redact.js` has already truncated it to MAX_LINE_CHARS by this point.
  const huge = [{ at: 1, text: "x".repeat(logs.MAX_LOG_BYTES * 2) }];
  assert.deepEqual(logs.trimLogEntries(huge), huge);
});

test("utf8Bytes counts bytes, not UTF-16 code units", () => {
  assert.equal(logs.utf8Bytes("abc"), 3);
  assert.equal(logs.utf8Bytes("é"), 2);
  assert.equal(logs.utf8Bytes("日"), 3);
  assert.equal(logs.utf8Bytes("😀"), 4, "a surrogate pair is one 4-byte character");
  assert.equal(logs.utf8Bytes(undefined), 0);
});

test("log keys are per job, and recognised on the way back", () => {
  assert.equal(logs.logStorageKey("job-1"), "downloadLogs:job-1");
  assert.equal(logs.jobIdFromLogKey("downloadLogs:job-1"), "job-1");
  assert.equal(logs.jobIdFromLogKey("downloadJobs"), null, "the job list is not a log key");
  assert.equal(logs.jobIdFromLogKey(undefined), null);
});

test("holes in a stored array are dropped rather than counted", () => {
  assert.deepEqual(logs.trimLogEntries([null, { at: 1, text: "a" }, undefined]), [{ at: 1, text: "a" }]);
  assert.deepEqual(logs.trimLogEntries(undefined), []);
});
