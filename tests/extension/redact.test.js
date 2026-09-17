"use strict";

/**
 * `extension/redact.js` against `tests/fixtures/redaction.json`.
 *
 * That fixture is the shared case table: `src/redact.rs`'s unit tests read the
 * same file, so a case that passes here and fails there is exactly the drift
 * between the host-side and extension-side implementations that the two-layer
 * decision in `docs/adr/0003-redact-urls-in-logs.md` depends on not happening.
 *
 * Every token in the fixture is a sentinel. Nothing here is a real secret, and
 * the point of the sentinels is that the tests can grep for them.
 */

const test = require("node:test");
const assert = require("node:assert/strict");

const redact = require("../../extension/redact.js");
const fixture = require("../fixtures/redaction.json");

test("every shared case redacts the way the fixture says", () => {
  assert.ok(fixture.cases.length > 0, "the shared case table is not empty");
  for (const testCase of fixture.cases) {
    assert.equal(redact.redactText(testCase.input), testCase.expected, testCase.name);
  }
});

test("the placeholders match the shared case table", () => {
  assert.equal(redact.QUERY_PLACEHOLDER, fixture.query_placeholder);
  assert.equal(redact.FRAGMENT_PLACEHOLDER, fixture.fragment_placeholder);
  assert.equal(redact.USERINFO_PLACEHOLDER, fixture.userinfo_placeholder);
  assert.equal(redact.MAX_LINE_CHARS, fixture.max_line_chars);
});

test("no sentinel survives redaction", () => {
  // The property, stated without reference to any one expected string: whatever
  // the exact output, none of it is the secret.
  assert.ok(fixture.sentinels.length > 0);
  for (const testCase of fixture.cases) {
    // One case deliberately carries a scheme this product never downloads, to
    // pin that redaction keys off the URL grammar and not off a token-shaped
    // string anywhere in the line.
    if (testCase.input.startsWith("ftp://")) continue;
    const redacted = redact.redactText(testCase.input);
    for (const sentinel of fixture.sentinels) {
      assert.equal(redacted.includes(sentinel), false, `${testCase.name}: sentinel survived`);
    }
  }
});

test("a very long line is truncated, after redaction rather than before", () => {
  const line = `https://cdn.example.test/${"a".repeat(4000)}?token=SENTINEL-SIGNED-TOKEN-MUST-NOT-PERSIST`;
  const redacted = redact.redactText(line);
  assert.equal([...redacted].length, redact.MAX_LINE_CHARS);
  assert.equal(redacted.includes("SENTINEL-SIGNED-TOKEN-MUST-NOT-PERSIST"), false);
});

test("non-strings pass through, so an absent error field needs no guard", () => {
  assert.equal(redact.redactText(undefined), undefined);
  assert.equal(redact.redactText(null), null);
  assert.equal(redact.redactText(""), "");
});

test("redactFields rewrites only the named string fields", () => {
  const job = {
    id: "a",
    url: "https://cdn.example.test/v.m3u8?token=SENTINEL-SIGNED-TOKEN-MUST-NOT-PERSIST",
    error: "could not open https://cdn.example.test/v.m3u8?token=SENTINEL-SIGNED-TOKEN-MUST-NOT-PERSIST",
    percent: 42,
    metadataError: null
  };
  const redacted = redact.redactFields(job, ["error", "metadataError"]);
  assert.equal(redacted.error, "could not open https://cdn.example.test/v.m3u8?…");
  assert.equal(redacted.percent, 42);
  assert.equal(redacted.metadataError, null);
  // `url` is deliberately not in the list: the popup matches jobs to page media
  // by it, and a re-download needs the real thing. It is redacted at display.
  assert.equal(redacted.url, job.url);
});

test("a record with nothing to redact is returned as-is, not copied", () => {
  // `background.js` uses identity to decide whether a restored record changed
  // and therefore needs writing back, so this is load-bearing.
  const job = { id: "a", error: "FFmpeg failed with status 1" };
  assert.equal(redact.redactFields(job, ["error"]), job);
});
