"use strict";

/**
 * KEI-60: the extension's half of the collision-free naming policy.
 *
 * The naming and renaming rules themselves live in the native host and are
 * tested in `src/output.rs` and `tests/native_host.rs`. What the extension owes
 * the host is the material it cannot derive: the page title, and the user's
 * chosen collision policy. These tests drive the real `background.js`,
 * `popup.js` and `options.js`, so a field dropped on the way to the port is a
 * failure here.
 */

const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

const { loadBackground, loadOptions, loadPopup } = require("./helpers/extension-dom.js");

const PROTOCOL = JSON.parse(
  fs.readFileSync(path.join(__dirname, "..", "fixtures", "protocol.json"), "utf8")
);

const MEDIA_URL = "https://example.test/hls/index.m3u8";
const SOURCE_URL = "https://example.test/watch/123";

/** The `download` request the background script posted to the native port. */
async function startedRequest(background, message) {
  await background.send({ type: "download-media", url: MEDIA_URL, sourceUrl: SOURCE_URL, ...message });
  await background.settle();
  const port = background.nativePort();
  assert.ok(port, "the background script opened a native port");
  return port.posted.find((posted) => posted?.command === "download");
}

test("the page title is forwarded to the host, trimmed", async () => {
  const background = await loadBackground({ native: true });
  const request = await startedRequest(background, { title: "  Lecture 3 — Topology  " });
  assert.equal(request.title, "Lecture 3 — Topology");
});

test("a missing or blank title is sent as null rather than an empty name", async () => {
  for (const title of [undefined, "", "   ", 42]) {
    const background = await loadBackground({ native: true });
    const request = await startedRequest(background, { title });
    assert.equal(request.title, null, `title ${JSON.stringify(title)}`);
  }
});

test("on_conflict defaults to the policy the shared protocol fixture pins", async () => {
  const background = await loadBackground({ native: true });
  const request = await startedRequest(background, { title: "Lecture 3" });
  assert.equal(request.on_conflict, PROTOCOL.default_on_conflict);
  assert.equal(request.on_conflict, "rename");
});

test("the Settings choice reaches the host, and only a policy the host knows", async () => {
  for (const policy of PROTOCOL.on_conflict_policies) {
    const background = await loadBackground({
      native: true,
      storage: { onConflict: policy }
    });
    const request = await startedRequest(background, { title: "Lecture 3" });
    assert.equal(request.on_conflict, policy);
  }

  // A stored value the host would refuse falls back instead of being relayed:
  // the host fails the frame on an unknown policy, which would strand the job.
  const background = await loadBackground({
    native: true,
    storage: { onConflict: "clobber" }
  });
  const request = await startedRequest(background, { title: "Lecture 3" });
  assert.equal(request.on_conflict, PROTOCOL.default_on_conflict);
});

test("overwrite stays false on the wire, so an older host keeps refusing", async () => {
  // `overwrite` predates `on_conflict`. A host that does not understand the new
  // field must not start replacing files because of it.
  const background = await loadBackground({
    native: true,
    storage: { onConflict: "overwrite" }
  });
  const request = await startedRequest(background, { title: "Lecture 3" });
  assert.equal(request.overwrite, false);
  assert.equal(request.on_conflict, "overwrite");
});

test("the popup sends the scanned page title along with the media URL", async () => {
  const popup = await loadPopup({
    sourceUrl: SOURCE_URL,
    pageTitle: "Lecture 3 — Topology",
    candidates: [{ url: MEDIA_URL, type: "playlist" }]
  });
  await popup.download(0);
  const started = popup.sent.find((message) => message?.type === "download-media");
  assert.ok(started, "the popup asked the background script to download");
  assert.equal(started.title, "Lecture 3 — Topology");
  assert.equal(started.sourceUrl, SOURCE_URL);
});

test("Settings offers exactly the policies the host accepts, and saves the choice", async () => {
  const options = await loadOptions({ settings: { onConflict: "fail" } });
  const select = options.field("on-conflict");
  assert.deepEqual(
    [...select.options].map((option) => option.value).sort(),
    [...PROTOCOL.on_conflict_policies].sort()
  );
  assert.equal(select.value, "fail", "the stored policy is what the page shows");

  select.value = "overwrite";
  await options.save();
  const written = options.saved().at(-1);
  assert.equal(written.onConflict, "overwrite");
});

test("Settings falls back rather than showing a policy the host would refuse", async () => {
  const options = await loadOptions({ settings: { onConflict: "clobber" } });
  assert.equal(options.field("on-conflict").value, PROTOCOL.default_on_conflict);
});
