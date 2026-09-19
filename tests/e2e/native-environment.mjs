/**
 * Preconditions and fixtures for the native-download end-to-end test.
 *
 * That test is the only one in this suite that reaches past the browser into
 * the native messaging host and FFmpeg, so it needs things the rest of the
 * suite does not: a registered native host, an FFmpeg new enough to complete an
 * HLS download, and the cookie-gated fixture origin. Any of them can be absent
 * on a perfectly good checkout, so each is detected and reported rather than
 * assumed — a test that quietly does nothing is worse than one that does not
 * run, which is the whole reason `tests/cookie_scope.rs` skips out loud.
 */

import fs from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { execFileSync, spawn } from "node:child_process";

const REPO_ROOT = path.join(import.meta.dirname, "..", "..");

/** FFmpeg below this cannot run the HLS path at all — see KEI-81. */
export const MINIMUM_FFMPEG = [7, 1];

/**
 * Where the native host manifest has to be for Firefox to find it.
 * `make extension-install` writes it; this only ever reads.
 */
function nativeHostManifest() {
  const dir =
    process.platform === "darwin"
      ? path.join(os.homedir(), "Library", "Application Support", "Mozilla", "NativeMessagingHosts")
      : path.join(os.homedir(), ".mozilla", "native-messaging-hosts");
  return path.join(dir, "com.downer.native.json");
}

/**
 * The FFmpeg this test should use, or null.
 *
 * `DOWNER_FFMPEG` wins because it is the same override the native host itself
 * honours. Then the conda prefix `install_test_ffmpeg.sh` writes to, then
 * whatever is on PATH — which on Ubuntu 24.04 is 6.1.1 and will be rejected by
 * the version check rather than silently producing a failing download.
 */
export function resolveFfmpeg() {
  const prefix = process.env.DOWNER_BROWSER_PREFIX || "/opt/downer-browser";
  const candidates = [
    process.env.DOWNER_FFMPEG,
    path.join(prefix, "bin", "ffmpeg"),
    "ffmpeg"
  ].filter(Boolean);

  for (const candidate of candidates) {
    const version = ffmpegVersion(candidate);
    if (!version) continue;
    const [major, minor] = version;
    const [wantMajor, wantMinor] = MINIMUM_FFMPEG;
    if (major > wantMajor || (major === wantMajor && minor >= wantMinor)) {
      return { path: candidate, version: version.join(".") };
    }
  }
  return null;
}

function ffmpegVersion(binary) {
  try {
    const first = execFileSync(binary, ["-version"], {
      encoding: "utf8",
      stdio: ["ignore", "pipe", "ignore"]
    }).split("\n")[0];
    const match = /ffmpeg version n?(\d+)\.(\d+)/.exec(first);
    return match ? [Number(match[1]), Number(match[2])] : null;
  } catch {
    return null;
  }
}

/**
 * Every reason this test cannot run, as sentences a reader can act on.
 * Empty means it can.
 */
export function missingPrerequisites({ browserAvailable, ffmpeg }) {
  const missing = [];
  // CI has no FFmpeg by deliberate choice (AGENTS.md) and no native host, so
  // this test is scoped to a developer machine or an agent session.
  if (process.env.CI) {
    missing.push("this test does not run in CI: it needs a native host and a real FFmpeg");
  }
  if (!browserAvailable) {
    missing.push("no Firefox for the end-to-end tests; run 'make extension-browser'");
  }
  if (!ffmpeg) {
    missing.push(
      `no FFmpeg ${MINIMUM_FFMPEG.join(".")}+ (PATH may have an older one); run 'make extension-ffmpeg'`
    );
  }
  if (!fs.existsSync(nativeHostManifest())) {
    missing.push(`no native host registered at ${nativeHostManifest()}; run 'make extension-install'`);
  }
  if (!fs.existsSync(path.join(REPO_ROOT, "target", "release", "downer"))) {
    missing.push("no release binary; run 'cargo build --release'");
  }
  return missing;
}

/**
 * Print the repo's loud-skip line, so a green run cannot be mistaken for a pass.
 *
 * `node --test` formats both stdout and stderr as TAP, so the line comes out as
 * `# SKIP: …` rather than the bare `SKIP: …` that `tests/cookie_scope.rs`
 * prints. The marker is what matters — `grep SKIP:` finds a skipped check in
 * either language — and fighting the runner for the first two characters would
 * cost more than it is worth.
 */
export function announceSkip(testName, reasons) {
  for (const reason of reasons) console.log(`SKIP: ${testName}: ${reason}`);
}

async function freePort() {
  const server = net.createServer();
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const { port } = server.address();
  await new Promise((resolve) => server.close(resolve));
  return port;
}

/**
 * One `tests/fixtures/protected_site.py` instance.
 *
 * The two instances use different host *strings* on one machine, which is what
 * gives them different page titles — and therefore different output filenames
 * from an identically named playlist. That is the whole point of the test.
 */
async function startOrigin(host, ffmpeg) {
  const port = await freePort();
  const child = spawn(
    "python3",
    [
      path.join("tests", "fixtures", "protected_site.py"),
      "--host", host,
      "--port", String(port),
      "--ffmpeg", ffmpeg
    ],
    { cwd: REPO_ROOT, stdio: ["ignore", "ignore", "pipe"] }
  );
  const origin = `http://${host}:${port}`;

  const deadline = Date.now() + 20_000;
  while (Date.now() < deadline) {
    if (child.exitCode !== null) throw new Error(`${origin} exited early`);
    try {
      const response = await fetch(`${origin}/`);
      if (response.ok) {
        await response.text();
        return { child, origin, host, port, title: `Downer fixture — ${host}:${port}` };
      }
    } catch {
      /* not listening yet */
    }
    await new Promise((resolve) => setTimeout(resolve, 200));
  }
  child.kill("SIGKILL");
  throw new Error(`${origin} never became ready`);
}

/** Both fixture origins, and a stop() that leaves no processes behind. */
export async function startFixtureOrigins(ffmpeg) {
  const first = await startOrigin("localhost", ffmpeg);
  let second;
  try {
    second = await startOrigin("127.0.0.1", ffmpeg);
  } catch (error) {
    first.child.kill("SIGKILL");
    throw error;
  }
  return {
    origins: [first, second],
    stop() {
      for (const origin of [first, second]) origin.child.kill("SIGTERM");
    }
  };
}

export function outputDirectory() {
  return fs.mkdtempSync(path.join(os.tmpdir(), "downer-e2e-out-"));
}
