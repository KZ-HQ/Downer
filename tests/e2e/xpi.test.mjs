/**
 * End-to-end: the packaged release artifact, in a real Firefox.
 *
 * `smoke.test.mjs` installs the `extension/` directory, which is what
 * `about:debugging` does during development. A release publishes something
 * else — the `.xpi` that `scripts/package_extension.sh` builds — and the two
 * differ in ways only a browser can rule on: the archive's member names, its
 * member order, and whether anything the directory has was left out of it.
 *
 * So this installs the archive itself and asserts Firefox gives it the add-on
 * ID the native host was compiled to allow. That is the KEI-58 acceptance
 * criterion "the native host manifest written by the installer uses the same
 * ID as the packaged extension", checked from the packaged end.
 *
 * What this does NOT cover, and no automated test in this repository can: a
 * *permanent* install. A temporary install bypasses add-on signing in every
 * Firefox edition, which is exactly why the e2e suite can use one. Installing
 * this unsigned archive permanently needs Developer Edition, Nightly or ESR
 * with `xpinstall.signatures.required` set to false, and a human; the README
 * says so.
 */

import test from "node:test";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

import { EXTENSION_ID, browserAvailable, launchBrowser } from "./browser.mjs";

const REPO_ROOT = path.resolve(import.meta.dirname, "..", "..");

const missingBrowser = !browserAvailable();

test(
  "Firefox installs the packaged .xpi",
  { skip: missingBrowser && "no Firefox or geckodriver; see docs/e2e-firefox.md" },
  async (t) => {
    const staging = fs.mkdtempSync(path.join(os.tmpdir(), "downer-xpi-"));
    t.after(() => fs.rmSync(staging, { recursive: true, force: true }));

    // Packaged here rather than taken from `dist/`, so the test cannot pass on
    // a stale archive someone built before the last change to `extension/`.
    const xpi = path.join(staging, "downer.xpi");
    execFileSync(path.join(REPO_ROOT, "scripts", "package_extension.sh"), [xpi], {
      stdio: "pipe"
    });

    await t.test("the archive is built the same way twice", () => {
      const second = path.join(staging, "again.xpi");
      execFileSync(path.join(REPO_ROOT, "scripts", "package_extension.sh"), [second], {
        stdio: "pipe"
      });
      assert.deepEqual(
        fs.readFileSync(xpi),
        fs.readFileSync(second),
        "packaging is reproducible, so a published SHA256SUMS can be rechecked by rebuilding"
      );
    });

    const browser = await launchBrowser();
    t.after(() => browser.close());

    await t.test("Firefox accepts it and reports the permanent add-on ID", async () => {
      assert.equal(await browser.installAddon(xpi), EXTENSION_ID);
    });
  }
);
