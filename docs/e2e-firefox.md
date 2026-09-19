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

One of them, `native-download.test.mjs`, goes further than the browser and needs
more than CI has:

```sh
make extension-ffmpeg    # once: install an FFmpeg 7.1+ beside the browser
make extension-install   # once: register the native messaging host
make extension-e2e-native
```

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

`smoke.test.mjs` stops at the browser: FFmpeg and the native messaging host are
not involved, because `download-media` is the only message that reaches them.

`native-download.test.mjs` is the other half, and covers the rest of the path —
the content script's `document.title`, the popup's `download-media` message, the
background script, `runtime.connectNative`, the Rust host, FFmpeg, and the file
that lands on disk. It exists because KEI-60's acceptance criterion lives at the
end of that path: two pages whose playlists are both `index.m3u8` must produce
two distinct, title-based filenames. Everything below the browser is covered by
unit and integration tests; only a real Gecko can show that the title survives
the trip. It also pins the rename sequence, the Settings collision policy, and
that a download which fails before FFmpeg writes leaves no file behind.

### What that test needs, and why it skips

It needs three things a plain checkout does not have: a registered native host
(`make extension-install`), an FFmpeg 7.1+ (`make extension-ffmpeg` — anything
older cannot run the HLS path at all, see KEI-81), and a release binary. CI has
none of them, and `AGENTS.md` keeps FFmpeg out of CI deliberately, so the test
detects each one and **skips out loud**, printing a `SKIP:` line naming what is
missing and the command that supplies it. That is the same contract
`tests/cookie_scope.rs` follows, and for the same reason: a check that quietly
does nothing is worse than one that does not run.

The playlists come from `tests/fixtures/protected_site.py`, whose
`/media/index.m3u8` route exists for this test. Every other playlist it serves
has a distinctive stem, which would never reach the title-naming code — so
before that route existed, a run could look healthy while exercising none of
what it claimed to.

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

## Why the browser, and the FFmpeg, come from conda-forge

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

`scripts/install_test_ffmpeg.sh` is the companion, for the same reason in a
different shape: Ubuntu 24.04 ships FFmpeg 6.1.1, which cannot run an HLS
download, and conda-forge serves a current one from that same single host. It
shares the micromamba the browser installer bootstraps, installs into its own
environment under the same prefix, and never touches the system FFmpeg.

Nothing outside these tests uses conda. Both installs land in
`/opt/downer-browser` (override with `DOWNER_BROWSER_PREFIX`) and both scripts
are idempotent, so re-running them costs nothing.

An existing Firefox works too. The tests take `FIREFOX_BIN` and
`GECKODRIVER_BIN`, and fall back to whatever is on `PATH`:

```sh
FIREFOX_BIN=/usr/bin/firefox GECKODRIVER_BIN=/usr/local/bin/geckodriver make extension-e2e
```

geckodriver 0.36 or newer is required, for `--allow-system-access`.

## Claude Code cloud sessions

Cloud sessions run Ubuntu 24.04 as root with the **Trusted** network access
level, which allows `conda.anaconda.org` but not Mozilla's hosts, so the
installer above is exactly what such a session needs.

`.claude/settings.json` registers `scripts/session_start.sh` as a
[SessionStart hook](https://code.claude.com/docs/en/hooks#sessionstart), which
runs it. A hook was the right place rather than the environment's setup script
for two reasons: the setup script provisions the VM before Claude Code starts,
and what it writes is captured in a filesystem snapshot reused by later
sessions in that environment — whatever repository they check out — so a setup
script has no business reaching into a clone. The hook runs after the clone
exists, with `$CLAUDE_PROJECT_DIR` pointing at it.

A hook in a repository runs wherever Claude Code runs, including on a
contributor's own machine, so `scripts/session_start.sh` exits 0 in silence
before touching anything unless `CLAUDE_CODE_REMOTE` is exactly `true`. A cloud
session VM sets that variable; it is never `true` locally. Set
`DOWNER_SKIP_BROWSER_INSTALL=1` to turn the install off in a cloud session too.
An install failure is reported and swallowed, because a session must not fail
to start over a missing test browser.

Because the installer returns early when the browser is already there, the hook
costs a fraction of a second on a session whose environment snapshot kept
`/opt/downer-browser`, and a few seconds on one that did not. Nothing it does
reaches outside that prefix: no system package manager, no file in the
repository.

Two limits worth knowing. A session with several repositories does not load
hooks from any repository's `.claude/settings.json`, so the browser will be
missing there; run `make extension-browser`. And a mid-session install is lost
when the session ends. To have every session start from a snapshot that
already contains the browser, put the body of `scripts/install_test_browser.sh`
in the environment's setup script as well — it needs only `curl` and `tar`, and
must not depend on the checkout.
