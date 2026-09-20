/**
 * Minimal WebDriver client for the real-Firefox end-to-end tests.
 *
 * The extension tests under `tests/extension/` run the shipped files in jsdom,
 * which is fast but is not Firefox: it has no WebExtension APIs, no content
 * script injection, and no real page loads. These helpers drive a real Firefox
 * through geckodriver instead, so a test can install `extension/` as a
 * temporary add-on and talk to the background and content scripts the way the
 * popup does.
 *
 * Selenium is deliberately not a dependency. The W3C WebDriver protocol is
 * plain JSON over HTTP, and the handful of commands these tests need are
 * shorter to write than the wiring needed to justify another devDependency in
 * a project whose extension ships with none.
 */

import { spawn } from "node:child_process";
import { once } from "node:events";
import fs from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";

const REPO_ROOT = path.resolve(import.meta.dirname, "..", "..");

/**
 * The add-on ID, read from the manifest Firefox itself reads rather than
 * copied here. `build.rs` reads the same field for the native host's
 * `allowed_extensions`, so the ID is written in exactly one place (KEI-58).
 */
export const EXTENSION_ID = JSON.parse(
  fs.readFileSync(path.join(REPO_ROOT, "extension", "manifest.json"), "utf8"),
).browser_specific_settings.gecko.id;

/**
 * Firefox gives each installed extension a random internal UUID per profile,
 * which would make `moz-extension://` URLs unguessable. Pinning the UUID with
 * the `extensions.webextensions.uuids` pref lets a test open the extension's
 * own pages directly, which is how it reaches a page with WebExtension APIs.
 */
export const EXTENSION_UUID = "3e6ec1a4-4e6d-4a23-9d1c-1f0d6c3b9f10";

/** Binaries the setup script installs, overridable for local runs. */
function resolveBinary(envVar, name) {
  const fromEnv = process.env[envVar];
  if (fromEnv) {
    if (!fs.existsSync(fromEnv)) {
      throw new Error(`${envVar} points at ${fromEnv}, which does not exist`);
    }
    return fromEnv;
  }

  const candidates = [];
  const prefix = process.env.DOWNER_BROWSER_PREFIX || "/opt/downer-browser";
  candidates.push(path.join(prefix, "bin", name));
  for (const dir of (process.env.PATH || "").split(path.delimiter)) {
    if (dir) candidates.push(path.join(dir, name));
  }
  const found = candidates.find((candidate) => fs.existsSync(candidate));
  if (found) return found;

  throw new Error(
    `${name} was not found. Install the end-to-end browser with ` +
      "`scripts/install_test_browser.sh`, or set " +
      `${envVar} to an existing binary. See docs/e2e-firefox.md.`
  );
}

export function browserBinaries() {
  return {
    firefox: resolveBinary("FIREFOX_BIN", "firefox"),
    geckodriver: resolveBinary("GECKODRIVER_BIN", "geckodriver")
  };
}

/** True when both binaries are present, so a runner can skip instead of fail. */
export function browserAvailable() {
  try {
    browserBinaries();
    return true;
  } catch {
    return false;
  }
}

async function freePort() {
  const server = net.createServer();
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  const { port } = server.address();
  await new Promise((resolve) => server.close(resolve));
  return port;
}

async function waitForStatus(port, child, timeoutMs = 30_000) {
  const deadline = Date.now() + timeoutMs;
  let lastError;
  while (Date.now() < deadline) {
    if (child.exitCode !== null) {
      throw new Error(`geckodriver exited early with code ${child.exitCode}`);
    }
    try {
      const response = await fetch(`http://127.0.0.1:${port}/status`);
      if (response.ok) return;
    } catch (error) {
      lastError = error;
    }
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  throw new Error(`geckodriver did not become ready: ${lastError}`);
}

class WebDriverError extends Error {}

/**
 * A WebDriver session against a headless Firefox, with the extension installed.
 *
 * Every method maps to one W3C endpoint; errors carry the driver's own message
 * because a failure inside the browser is far easier to read than "500".
 */
export class Browser {
  constructor(driver, port, sessionId, logDir) {
    this.driver = driver;
    this.port = port;
    this.sessionId = sessionId;
    this.logDir = logDir;
  }

  async #send(method, endpoint, body) {
    const response = await fetch(
      `http://127.0.0.1:${this.port}/session/${this.sessionId}${endpoint}`,
      {
        method,
        headers: body === undefined ? {} : { "content-type": "application/json" },
        body: body === undefined ? undefined : JSON.stringify(body)
      }
    );
    const payload = await response.json().catch(() => ({}));
    if (!response.ok) {
      const error = payload?.value?.error || `HTTP ${response.status}`;
      const message = payload?.value?.message || "";
      throw new WebDriverError(`${method} ${endpoint} failed: ${error} ${message}`.trim());
    }
    return payload.value;
  }

  /**
   * Installs an unpacked extension directory and returns its add-on id.
   *
   * Temporary, the same way `about:debugging` loads it, which is what lets
   * Firefox accept an unsigned directory at all.
   */
  installAddon(sourceDir) {
    return this.#send("POST", "/moz/addon/install", {
      path: path.resolve(sourceDir),
      temporary: true
    });
  }

  currentUrl() {
    return this.#send("GET", "/url");
  }

  windowHandles() {
    return this.#send("GET", "/window/handles");
  }

  switchToWindow(handle) {
    return this.#send("POST", "/window", { handle });
  }

  /**
   * Runs an async function body in the current context.
   *
   * The script is wrapped so it can `await`: WebDriver's async form hands the
   * page a callback as the last argument, and anything thrown inside would
   * otherwise hang until the script timeout with no message. Rejections are
   * returned as `{ error }` and rethrown here instead.
   */
  async evaluate(body, args = []) {
    const script = `
      const done = arguments[arguments.length - 1];
      (async () => { ${body} })().then(
        (value) => done({ value }),
        (error) => done({ error: String(error && error.stack || error) })
      );
    `;
    const result = await this.#send("POST", "/execute/async", { script, args });
    if (result && result.error) throw new Error(result.error);
    return result ? result.value : undefined;
  }

  /**
   * Opens one of the extension's own pages and switches to it.
   *
   * Firefox rejects a content-initiated navigation to `moz-extension://`, so
   * the tab is opened from the chrome context with the system principal, the
   * same way the browser's own UI opens it. The extension's internal UUID is
   * pinned by `launchBrowser`, which is what makes the URL knowable at all.
   */
  async openExtensionPage(url) {
    const before = new Set(await this.windowHandles());

    await this.#send("POST", "/moz/context", { context: "chrome" });
    try {
      await this.#send("POST", "/execute/sync", {
        script: `
          const win = Services.wm.getMostRecentWindow("navigator:browser");
          win.gBrowser.selectedTab = win.gBrowser.addTab(arguments[0], {
            triggeringPrincipal: Services.scriptSecurityManager.getSystemPrincipal()
          });
        `,
        args: [url]
      });
    } finally {
      await this.#send("POST", "/moz/context", { context: "content" });
    }

    const deadline = Date.now() + 30_000;
    while (Date.now() < deadline) {
      const handle = (await this.windowHandles()).find((candidate) => !before.has(candidate));
      if (handle) {
        await this.switchToWindow(handle);
        if ((await this.currentUrl()) === url) return handle;
      }
      await new Promise((resolve) => setTimeout(resolve, 100));
    }
    throw new WebDriverError(`the extension page ${url} never opened`);
  }

  async close() {
    try {
      await fetch(`http://127.0.0.1:${this.port}/session/${this.sessionId}`, {
        method: "DELETE"
      });
    } catch {
      // The browser may already be gone; killing the driver below is enough.
    }
    this.driver.kill("SIGTERM");
    const exited = once(this.driver, "exit");
    const timer = setTimeout(() => this.driver.kill("SIGKILL"), 5_000);
    try {
      await exited;
    } finally {
      clearTimeout(timer);
    }
    // The driver log only matters when the browser would not start, which
    // fails before any session exists to close.
    fs.rmSync(this.logDir, { recursive: true, force: true });
  }
}

/**
 * Starts geckodriver and a headless Firefox with a fresh profile.
 *
 * The extension's internal UUID is pinned in that profile, because the tests
 * reach the extension's own pages by URL and Firefox would otherwise pick a
 * random one. `headless: false` is for watching a failure happen.
 */
export async function launchBrowser({ headless = true } = {}) {
  const { firefox, geckodriver } = browserBinaries();
  const port = await freePort();
  const logDir = fs.mkdtempSync(path.join(os.tmpdir(), "downer-e2e-"));
  const driverLog = fs.openSync(path.join(logDir, "geckodriver.log"), "a");

  // `--allow-system-access` is what lets a test switch to the chrome context,
  // the only way to open one of the extension's own pages: Firefox refuses a
  // content-initiated navigation to `moz-extension://`.
  const driver = spawn(
    geckodriver,
    ["--port", String(port), "--host", "127.0.0.1", "--allow-system-access"],
    {
      stdio: ["ignore", driverLog, driverLog]
    }
  );

  try {
    await waitForStatus(port, driver);

    const args = headless ? ["-headless"] : [];
    const response = await fetch(`http://127.0.0.1:${port}/session`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        capabilities: {
          alwaysMatch: {
            "moz:firefoxOptions": {
              binary: firefox,
              args,
              prefs: {
                "extensions.webextensions.uuids": JSON.stringify({
                  [EXTENSION_ID]: EXTENSION_UUID
                }),
                // Keep the profile offline apart from the fixture server.
                "browser.startup.homepage": "about:blank",
                "datareporting.policy.dataSubmissionEnabled": false,
                "extensions.update.enabled": false,
                "app.update.auto": false
              }
            }
          }
        }
      })
    });
    const payload = await response.json().catch(() => ({}));
    if (!response.ok) {
      const message = payload?.value?.message || `HTTP ${response.status}`;
      throw new WebDriverError(
        `could not start Firefox: ${message} (driver log in ${logDir})`
      );
    }
    return new Browser(driver, port, payload.value.sessionId, logDir);
  } catch (error) {
    driver.kill("SIGKILL");
    throw error;
  }
}

export function extensionDir() {
  return path.join(REPO_ROOT, "extension");
}

export function extensionUrl(page) {
  return `moz-extension://${EXTENSION_UUID}/${page}`;
}
