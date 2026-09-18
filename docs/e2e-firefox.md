# End-to-end tests in a real Firefox

`tests/extension/` runs the shipped extension files in jsdom. That is fast and
covers the logic, but jsdom is not a browser: it has no add-on installation, no
content-script injection, and no `browser.*` APIs. `tests/e2e/` fills that gap
by installing `extension/` into a real, headless Firefox and talking to it the
way the popup does.

```sh
make extension-browser   # once: install Firefox and geckodriver
make extension-e2e       # run the end-to-end tests
```

`make check` deliberately does not run them, because a plain checkout has no
browser. CI runs them in a separate job.

## What the tests cover

- Firefox accepts `extension/manifest.json` and loads the background scripts.
  `web-ext lint` checks the manifest against a schema; this checks it against
  the browser that has to load it.
- The background script answers `get-download-statuses` and
  `get-download-logs`, the messages the popup and the options page send.
- The content script is injected into pages served over HTTP and its scan of
  the live DOM returns what `tests/extension/media-scan.test.js` expects from
  the same fixtures. Where the two disagree, the browser is right and the jsdom
  expectation is the bug.

FFmpeg and the native messaging host are not involved: `download-media` is the
only message that reaches them, and it is not exercised.

## How the harness works

`tests/e2e/browser.mjs` speaks the W3C WebDriver protocol to geckodriver over
plain HTTP. Selenium is not a dependency; the handful of commands the tests
need are shorter than the wiring another devDependency would cost.

Two details are worth knowing before adding a test:

- **The extension's UUID is pinned.** Firefox gives each extension a random
  internal UUID per profile, which would make `moz-extension://` URLs
  unguessable. `launchBrowser` pins it through the
  `extensions.webextensions.uuids` pref so `extensionUrl("options.html")` is a
  knowable address.
- **Extension pages are opened from the chrome context.** Firefox rejects a
  content-initiated navigation to `moz-extension://`, so `openExtensionPage`
  switches to the chrome context and opens the tab with the system principal,
  which is why geckodriver is started with `--allow-system-access`. Everything
  else runs in the extension page, which is the only context with `browser.*`
  APIs. From there, tests drive pages with `browser.tabs`, the same way the
  popup does.

## Why the browser comes from conda-forge

`scripts/install_test_browser.sh` installs Firefox and geckodriver from
conda-forge, which repackages Mozilla's official build. That is an odd choice
for a project with no other Python or conda dependency, and it is deliberate:
the alternatives do not work everywhere the tests need to run.

- Ubuntu's `firefox` package is a transitional stub for the snap, so
  `apt install firefox` yields nothing runnable in a container without snapd.
- Mozilla's own downloads (`download.mozilla.org`, `packages.mozilla.org`) and
  the geckodriver releases on GitHub are unreachable from a network-restricted
  CI or agent sandbox, where `conda.anaconda.org` usually is reachable.
- Playwright's Firefox is a patched build served from `cdn.playwright.dev`, and
  Playwright cannot install Firefox extensions at all.

Nothing outside these tests uses conda. The install lands in
`/opt/downer-browser` (override with `DOWNER_BROWSER_PREFIX`) and the script is
idempotent, so re-running it costs nothing.

An existing Firefox works too. The tests take `FIREFOX_BIN` and
`GECKODRIVER_BIN`, and fall back to whatever is on `PATH`:

```sh
FIREFOX_BIN=/usr/bin/firefox GECKODRIVER_BIN=/usr/local/bin/geckodriver make extension-e2e
```

geckodriver 0.36 or newer is required, for `--allow-system-access`.

## Claude Code cloud sessions

Cloud sessions run Ubuntu 24.04 as root with the **Trusted** network access
level, which allows `conda.anaconda.org` but not Mozilla's hosts, so the script
above is exactly what such a session needs. Put it in the environment's setup
script, where the filesystem snapshot keeps the result for later sessions:

```bash
cd /home/user/Downer && ./scripts/install_test_browser.sh
```

It finishes in seconds, well inside the five-minute setup budget. If the
repository is checked out somewhere else, or you want the browser regardless of
the checkout, inline the script's body instead; it needs only `curl` and `tar`.

A session that did not get the browser at startup can install it mid-session
with `make extension-browser`, but that install is lost when the session ends.
