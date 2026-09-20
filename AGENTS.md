# Downer contributor guide

## Project overview

Downer is a Rust CLI and Firefox WebExtension for downloading one HTTP(S)
media URL at a time through FFmpeg. The extension scans the active page and
uses a Rust native-messaging host so browser cookies, User-Agent, and Referer
information can be forwarded to protected media servers.

This file is the contributor guide: workflow, layout, rules, and the checks that
must pass. What the product *does* is in
[`docs/user-guide.md`](docs/user-guide.md), how it is put together in
[`docs/architecture.md`](docs/architecture.md), and why in
[`docs/adr/`](docs/adr/README.md). Keep user-facing prose out of this file and
out of `README.md`; both point at the docs instead.

## Planning, status, and handoffs

The Linear project **Downer** is the source of truth for the roadmap, work in
progress, known gaps, and handoffs between people or agents:

https://linear.app/kzhq/project/downer-fb4196d41645 (team "Keith", issue
prefix `KEI`). Milestones M1–M5 encode execution order; blocking relations
encode dependencies.

There is no `HANDOFF.md`, and status must not be recorded in repository
files. Repository files hold only stable facts (how to build, test, and
structure code); Linear holds everything that changes as work progresses.

Before starting work:

- Find the Linear issue for the task, or create one in the project if none
  exists. Read its Context, Scope, Out of scope, Acceptance criteria, and
  blockers. Do not start an issue whose blockers are still open unless the
  issue says the overlap is acceptable.
- Move the issue to **In Progress**.

While working:

- Record decisions that affect other issues as comments on the issue and, if
  the decision is architectural, as an ADR (see "Architecture decisions").
- If scope changes, update the issue description rather than deviating
  silently.

When handing off, whether finished or not:

- Run the required workflow below and note the result.
- Post a handoff comment on the issue containing: what changed (files and
  behaviour), verification performed (commands and outcome), what remains and
  why, manual steps the next person needs (for example rebuild the release
  binary or reload the temporary add-on), and any follow-up issues created.
- Move the issue to **In Review** when complete, or leave it **In Progress**
  with the comment when not, and update blockers on dependent issues.
- **Done means merged.** An issue moves to **Done** only once the pull request
  carrying its work is merged, not when the work is pushed or reviewed.
- Update the project description only when the roadmap or the architecture
  summary changes.

## Versioning

The Rust package and the Firefox extension share **one product version**.
`version` in `Cargo.toml` and `version` in `extension/manifest.json` must
always be identical and are bumped together in the same commit, following
semantic versioning for the product as a whole. There is no separate VERSION
file; those two fields are the source of truth, and CI checks that they agree.

Firefox refuses a temporary or signed add-on whose version goes backwards, so
never lower the shared version. Record user-visible changes in `CHANGELOG.md`
under `Unreleased`, and move that section under the new version number when
the version is bumped.

The minimum supported Rust version is declared as `rust-version` in
`Cargo.toml`; raising it is a deliberate change that belongs in the changelog.
The minimum supported FFmpeg version is documented in `README.md`.

A release is a `vX.Y.Z` tag on a commit whose two version fields already say
`X.Y.Z`; `.github/workflows/release.yml` refuses to build otherwise. The
extension's add-on ID, `downer@kz-hq.github.io`, is written only in
`extension/manifest.json`: `build.rs` reads it from there into
`downer::host::EXTENSION_ID`, and `tests/e2e/browser.mjs` reads the same field,
so the ID cannot drift between the extension and the native host that allows
it. Changing that ID breaks every installed add-on, as
`docs/adr/0008-relocatable-native-host-installation.md` records.

## Architecture decisions

Architectural decisions are recorded as ADRs under `docs/adr/`, numbered
`NNNN-short-title.md`. [`docs/adr/README.md`](docs/adr/README.md) indexes them
with a one-line summary and a status each, and
[`docs/adr/template.md`](docs/adr/template.md) is the starting point for a new
one. Take the next free number; records are never renumbered, and an accepted
record is not edited except to add a status line pointing at whatever changed
it.

Any change to the native messaging protocol, the host process model, the
FFmpeg command layer, discovery ownership, or control semantics requires an
ADR.

## Repository layout

- `src/`: Rust CLI, FFmpeg process layer, scraper, output handling, and native host.
- `extension/`: Firefox WebExtension files, popup, Settings page, background worker,
  content script, and native messaging protocol.
- `scripts/`: `install_native_host.sh`, a thin development wrapper that builds
  the release binary and runs `downer install-host --dev` (the installation
  itself lives in `src/host.rs`, so a user without the repository can run it),
  `install_test_browser.sh`, which installs the Firefox and geckodriver the
  end-to-end tests drive, and `session_start.sh`, the SessionStart hook that
  runs it in a Claude Code cloud session and nowhere else. The release tooling
  also lives here, and is written so it runs on a laptop rather than only
  inside a workflow: `check_versions.py` (the version agreement rule, with
  `--tag` for the extra "and the tag agrees" check a release needs),
  `package_extension.sh` (the one packaging rule behind both
  `make extension-package` and `make extension-xpi`, built reproducibly so a
  published checksum can be rechecked by rebuilding), `release_artifacts.sh`
  (one platform's release build, package and checksums),
  `release_collect.py` (merges the platforms' staged artifacts into the files a
  Release carries), `changelog_section.py` (a version's release notes), and
  `test_release_tooling.py`, which tests all of them and runs as part of
  `make check`.
- `.claude/settings.json`: Claude Code project settings. It registers
  `scripts/session_start.sh` as a SessionStart hook, so a cloud session starts
  with the end-to-end browser installed; that script exits immediately unless
  `CLAUDE_CODE_REMOTE` is `true`, so pulling this repository onto your own
  machine installs nothing (`docs/e2e-firefox.md` has the details, and
  `DOWNER_SKIP_BROWSER_INSTALL=1` turns it off in the cloud too). It also
  pre-approves the Linear MCP tools so agent sessions do not prompt for routine issue and
  comment updates, while still asking before any MCP delete. Permission rules
  match on the MCP **server name as configured in that session**, which varies
  by how Linear was connected, so the allow list carries every spelling seen so
  far. If a session still prompts, read the server name from the tool named in
  the prompt (`mcp__<server>__<tool>`) and add `mcp__<server>` to the list.
  Two things this file cannot fix: settings are read at session start, so a
  change needs a fresh session, and if an organization sets a claude.ai
  connector tool to `ask`, allow rules for it never take effect.
- `docs/`: `user-guide.md` (install, first download, settings, controls, and
  what Downer does not do) and `troubleshooting.md` (the same ground by
  symptom), which are where user-facing prose belongs rather than `README.md`;
  `architecture.md`, the components, data flow and trust boundaries;
  `protocol.md`, the native messaging contract implemented by `src/native.rs`
  and `extension/task-protocol.js`; `e2e-firefox.md`, how the real-Firefox tests
  work and where their browser comes from; and `adr/`, the architecture decision
  records, indexed by `adr/README.md`.
- `tests/`: CLI and native-host integration tests (`tests/cli.rs`,
  `tests/native_host.rs`), the extension's Node tests (`tests/extension/`),
  and fixtures shared by both languages: HLS playlists in
  `tests/fixtures/hls/`, HTML pages in `tests/fixtures/pages/`, and
  `tests/fixtures/protocol.json`, the native messaging protocol's wire
  vocabulary, which both test suites read so the Rust and JavaScript sides
  cannot rename a protocol term unilaterally, and
  `tests/fixtures/redaction.json`, the URL redaction case table, read by both
  suites for the same reason — the rule is implemented once per language.
  `tests/fixtures/protected_site.py` is a standard-library HTTP server that
  gates media on a session cookie, run with `make fixture-site` and
  `make fixture-site-peer`. It exists for the checks no automated test can
  make: whether a cookie survives the whole path from Firefox's cookie jar
  through the native port to FFmpeg, and whether it stops at the media host.
  Two instances on `localhost` and `127.0.0.1` give two host strings on one
  network. It logs whether a `Cookie` header arrived, never its value, and
  prints request paths with the query replaced. It also serves a signed playlist
  whose segment URLs carry a token, which is how log redaction is checked end to
  end.
- `dist/`: build artifact directory for the packaged Firefox extension. It is
  produced by `make extension-package` (and by CI/release tooling) and is not
  tracked in git.

## Architecture map

Where things live. The data flow from page to FFmpeg, the trust and session
boundaries, the job state machine, and the reasoning behind each component are
in [`docs/architecture.md`](docs/architecture.md); this list is here so a
session can find the right file without opening it.

| File | Owns |
| --- | --- |
| `extension/media-scan.js` | Detection rules. Kept identical to `src/scraper.rs::extract_media_urls`, pinned by `tests/fixtures/media-extensions.json`. |
| `extension/content.js` | Page DOM and resource-entry scan; fetches an HLS playlist in the page's own session. |
| `extension/popup.js` | Starts downloads, renders status and controls. Holds no authoritative state. |
| `extension/job-view.js` | Which persisted jobs a popup renders, and which may set the headline status. |
| `extension/background.js` | Jobs, cookies, playlist metadata, native ports, progress, log history, and restart reconciliation. |
| `extension/job-state.js` | The job state machine: states, transitions, terminal set, render predicates. |
| `extension/task-protocol.js` | Correlates native acknowledgements and terminal responses by job/request ID. |
| `src/main.rs` | Entry point; pre-parses `--native-host` before clap (ADR-0019). |
| `src/native.rs` | The native messaging host: framing, handshake, workers, progress, control. |
| `src/ffmpeg.rs` | Invokes FFmpeg without a shell, parses `-progress`, captures stderr, pause/resume signals. |
| `src/scraper.rs` | Resolves source-page media URLs and parses HLS playlists. The only playlist parser. |
| `src/discovery.rs` | The candidate list behind `--list`, `--select`/`--media` and `--json`. Assembles what `scraper` answers; decides nothing about media itself. |
| `src/output.rs` | URL validation, filename inference and sanitizing, the `OnConflict` policy. |
| `src/host.rs` | Native-host registration: manifest, launcher, durable binary location, recorded FFmpeg path. |

Two things in that table are easy to break and worth knowing before you edit
them. The detection rules and the redaction rule are each implemented once per
language and pinned by a shared fixture, so changing one side alone fails the
other side's tests. And `background.js` keeps logs under a
`downloadLogs:<jobId>` key of their own, with storage writes and log broadcasts
coalesced, so a long HLS download does not rewrite all state per line of FFmpeg
output.

## Required workflow

Before handing off code changes, run:

```sh
make check
cargo build --release
make extension-package
```

`make check` runs Rust formatting, Clippy with warnings denied, Rust tests,
`web-ext lint`, the extension's Node tests, and Firefox JavaScript/manifest
validation. Record the outcome in the Linear handoff comment.

## Continuous integration

`.github/workflows/ci.yml` runs on every push and pull request. The `check`
job runs on `ubuntu-latest` and `macos-latest` and executes exactly what you
run locally — `make check`, then `cargo build --release --locked` and
`make extension-package` — and uploads the release binary and
`dist/downer-firefox.zip` as run artifacts. An `e2e` job installs Firefox and geckodriver with
`make extension-browser` and runs `make extension-e2e`. A separate `msrv` job
builds against the `rust-version` declared in `Cargo.toml`, so the declared
minimum stays honest. FFmpeg is deliberately not installed in CI; tests generate fake
FFmpeg executables instead. The tests that need a real one —
`tests/cookie_scope.rs`, `tests/log_redaction.rs`, and the real-FFmpeg tests at
the end of `tests/native_host.rs` — skip loudly when none is present, printing a
line beginning `SKIP:`. A green CI run is therefore not evidence for those; run
them locally with `-- --nocapture` and record the result.

**CI must be green before an issue moves to In Review.** If a change needs a
new check, add it to `make check` rather than to the workflow, so local runs
and CI cannot drift apart. The end-to-end tests are the one deliberate
exception: they need a browser that `make check` cannot assume.

`.github/workflows/release.yml` runs on `v*` tags, and on demand from the
Actions tab for a dry run that builds and uploads the artifacts without
publishing anything. It is deliberately thin: everything it does apart from
talking to GitHub is in the `scripts/` release tooling above, so a failure can
be reproduced locally — which matters because this repository's GitHub job logs
are not always readable after a run. What cannot be checked without pushing a
tag is that GitHub fires the workflow and that `gh release create` attaches the
files; say so plainly rather than implying a workflow works because it parses.

The root `package.json` is development-only tooling (`web-ext`); the extension
itself ships without dependencies, and `node_modules/` is not tracked.
`make extension-lint` installs it on first use, `make extension-test` runs the
`node:test` suites in `tests/extension/`, and both run as part of
`make check`. Pure extension helpers belong in an importable file with the
`module.exports` guard used by `extension/task-protocol.js` and
`extension/hls.js`, so they can be tested from Node.

Files that touch the `browser` global or the DOM at load — `content.js` and
`popup.js` — cannot be `require()`d, but they are still tested: 
`tests/extension/helpers/extension-dom.js` evaluates the real, shipped files in
a `jsdom` window with a stubbed `browser` API, and
`tests/extension/content-dom.test.js` and `tests/extension/popup-dom.test.js`
assert on the resulting DOM. That harness is not Firefox: it does not cover real
WebExtension APIs, content-script injection, or native messaging over a real
port.

`tests/e2e/` covers all three in a real, headless Firefox. `smoke.test.mjs`
installs `extension/` as a temporary add-on, checks that Firefox loads the
manifest and the background scripts, exchanges messages with the background
script, and scans the same `tests/fixtures/pages/` HTML through an injected
content script, asserting what the jsdom tests assert. Where the two disagree,
the browser is right.

`native-download.test.mjs` goes the rest of the way — the content script's
`document.title`, the popup's `download-media` message, the background script,
`runtime.connectNative`, the Rust host, FFmpeg, and the file on disk — so
native messaging over a real port is covered, and so is the output naming rule
above, whose acceptance criterion only exists at the end of that path. Run them
with:

```sh
make extension-browser   # once: installs Firefox and geckodriver
make extension-e2e

make extension-ffmpeg    # once more, for the native download test
make extension-install   # once: registers the native messaging host
make extension-e2e-native
```

They are not part of `make check`, because a plain checkout has no browser; CI
runs them in a separate `e2e` job. The native download test additionally needs a
registered native host and an FFmpeg 7.1+, which CI does not have and is not
meant to, so **it detects each prerequisite and skips out loud with a `SKIP:`
line** rather than failing or passing vacuously — the same contract
`tests/cookie_scope.rs` follows. A green `e2e` job is therefore not evidence for
it; run it locally and record the result, as with the other skipping suites.
`docs/e2e-firefox.md` explains the harness, the environment variables that point
it at an existing Firefox, and why the browser and that FFmpeg are installed
from conda-forge.

`jsdom` is development-only tooling, like `web-ext`; the extension itself still
ships with no dependencies.

For a complete local extension setup on macOS or Linux:

```sh
make setup
make extension
```

After changing extension files, reload the temporary add-on from
`about:debugging` in Firefox. After changing Rust native-host code, rebuild the
release binary; `make extension` also refreshes the native-host registration.
Firefox invokes `target/release/downer` through the native-host wrapper, so a
stale release binary is the most common cause of "it worked before" reports.

## Coding and behavior rules

- Keep the CLI interface as `downer URL [options]`.
- Accept only `http://` and `https://` URLs.
- Pass URLs and paths to child processes as arguments; never use shell
  interpolation.
- Name a download after the media URL's own filename when it has one. When that
  stem is generic (`index`, `playlist`, `master`, `download`, `video`, `media`,
  or digits only) the name is **`video.<ext>`** — a predictable default beats a
  derived one, see ADR-0005. Naming after the page title is **opt-in**: `--name`
  on the CLI, and a Settings checkbox that is off by default in the extension,
  which simply does not send `title` when off. A title is user data reaching the
  disk: sanitize it with the same rules as a URL-derived name, bound its length,
  and never let it into a log, an error, or any event but the output path.
- Resolve an output collision by **renaming** — `_2`, `_3`, … before the
  extension — whenever *we* inferred the filename, which is every extension
  download and every CLI run without `--output`. Refuse the collision only when
  the user named an exact path with `--output`. `--on-conflict
  fail|rename|overwrite` and the matching Settings option make the choice
  explicit, and `--overwrite` remains shorthand for `overwrite`. Renaming takes
  its name by creating the file exclusively, so two hosts racing for one
  directory cannot pick the same name; a download that then fails without
  writing anything deletes that reservation, so a failure leaves no residue and
  a retry gets the same name. This rule replaces the earlier "refuse
  output collisions unless `--overwrite` is explicitly supplied"; the reasoning
  is in ADR-0004.
- Preserve partial output and diagnostic files after download failures. This
  covers what FFmpeg wrote, not a still-empty name the output layer reserved
  before invoking it: that is released on failure (see ADR-0004). The emptiness
  check is the line between the two — never widen a cleanup past it.
- Keep one URL per invocation; batch downloading and authentication workflows
  are out of scope unless explicitly requested.
- Keep browser controls (pause, resume, cancel) functional for native-host
  downloads.
- Preserve cookies and Referer handling for protected media, but do not log
  cookie values or other secrets.
- Bound persistent histories and logs. The current per-download log limit is
  500 lines and 128 KiB, over at most 20 persisted jobs.
- Redact URLs in anything that is logged, persisted, or displayed: keep scheme,
  host, port and path, and replace the query, a fragment, and any userinfo. The
  rule is `src/redact.rs` and `extension/redact.js`, pinned by
  `tests/fixtures/redaction.json`, which both test suites read. FFmpeg's own
  stderr is the reason it exists; see ADR-0003.
- Treat HLS segment counts as playlist-derived totals. Completed segment counts
  are estimates based on FFmpeg output timestamps unless a future scheduler can
  report exact segment completions.

## FFmpeg and HLS notes

FFmpeg is a required runtime dependency and is not bundled. The extension
native host searches Homebrew locations and then `PATH`; `DOWNER_FFMPEG` can
override discovery.

The current `--threads` setting controls FFmpeg processing threads. It does
not make HLS HTTP requests concurrent. The native concurrent HLS downloader is
planned and specified in Linear issue KEI-68 (design) and its follow-up
issues; it must use a bounded worker pool, preserve segment order during
assembly, and keep pause/resume/cancel behavior consistent with the existing
native process layer.

## Editing and testing guidance

- Use `apply_patch` for source edits when running in Codex. This instruction
  is Codex-specific; Claude Code sessions should ignore it and use their own
  file editing tools.
- Add or update tests with behavior changes.
- Prefer local fixtures and fake FFmpeg executables over live protected URLs.
  Do not assume a live CDN is reachable from the development environment.
- Do not commit generated temporary files, downloaded media, cookies, or
  credentials.
- Do not expose browser cookie headers in Settings logs, error messages, or
  Linear comments.
