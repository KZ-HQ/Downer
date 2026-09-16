# Downer contributor guide

## Project overview

Downer is a Rust CLI and Firefox WebExtension for downloading one HTTP(S)
media URL at a time through FFmpeg. The extension scans the active page and
uses a Rust native-messaging host so browser cookies, User-Agent, and Referer
information can be forwarded to protected media servers.

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

## Architecture decisions

Architectural decisions are recorded as ADRs under `docs/adr/`, numbered
`NNNN-short-title.md`. ADR-0001 records the native messaging protocol
contract; KEI-63 backfills the decisions already embodied in the code and adds
the rest of the documentation set.

Any change to the native messaging protocol, the host process model, the
FFmpeg command layer, discovery ownership, or control semantics requires an
ADR.

## Repository layout

- `src/`: Rust CLI, FFmpeg process layer, scraper, output handling, and native host.
- `extension/`: Firefox WebExtension files, popup, Settings page, background worker,
  content script, and native messaging protocol.
- `scripts/`: native host installation and launcher scripts.
- `.claude/settings.json`: Claude Code project settings. It pre-approves the
  Linear MCP tools so agent sessions do not prompt for routine issue and
  comment updates, while still asking before any MCP delete. Permission rules
  match on the MCP **server name as configured in that session**, which varies
  by how Linear was connected, so the allow list carries every spelling seen so
  far. If a session still prompts, read the server name from the tool named in
  the prompt (`mcp__<server>__<tool>`) and add `mcp__<server>` to the list.
  Two things this file cannot fix: settings are read at session start, so a
  change needs a fresh session, and if an organization sets a claude.ai
  connector tool to `ask`, allow rules for it never take effect.
- `docs/`: `protocol.md`, the native messaging contract implemented by
  `src/native.rs` and `extension/task-protocol.js`, and `adr/`, the
  architecture decision records.
- `tests/`: CLI and native-host integration tests (`tests/cli.rs`,
  `tests/native_host.rs`), the extension's Node tests (`tests/extension/`),
  playlist fixtures shared by both languages (`tests/fixtures/hls/`), and
  `tests/fixtures/protocol.json`, the native messaging protocol's wire
  vocabulary, which both test suites read so the Rust and JavaScript sides
  cannot rename a protocol term unilaterally.
- `dist/`: build artifact directory for the packaged Firefox extension. It is
  produced by `make extension-package` (and by CI/release tooling) and is not
  tracked in git.

## Architecture map

1. `extension/content.js` scans page DOM and resource entries. It can also
   fetch an HLS playlist using the source page's browser session.
2. `extension/popup.js` starts downloads and renders status/control events.
3. `extension/background.js` owns persistent jobs, cookies, playlist metadata,
   native task channels, progress state, and log history.
4. `extension/task-protocol.js` correlates native control acknowledgements and
   terminal responses by job/request ID.
5. `src/native.rs` implements the Firefox native-messaging protocol, launches
   download workers, forwards progress/log events, and controls FFmpeg.
6. `src/ffmpeg.rs` invokes FFmpeg without a shell, parses `-progress` output,
   captures stderr, and supports Unix pause/resume signals.
7. `src/scraper.rs` resolves source-page media URLs and parses HLS metadata.
8. `src/output.rs` validates URLs, infers and sanitizes filenames, and
   enforces the output collision rule.

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
`dist/downer-firefox.zip` as run artifacts. A separate `msrv` job builds
against the `rust-version` declared in `Cargo.toml`, so the declared minimum
stays honest. FFmpeg is deliberately not installed in CI; tests generate fake
FFmpeg executables instead.

**CI must be green before an issue moves to In Review.** If a change needs a
new check, add it to `make check` rather than to the workflow, so local runs
and CI cannot drift apart.

The root `package.json` is development-only tooling (`web-ext`); the extension
itself ships without dependencies, and `node_modules/` is not tracked.
`make extension-lint` installs it on first use, `make extension-test` runs the
`node:test` suites in `tests/extension/`, and both run as part of
`make check`. Pure extension helpers belong in an importable file with the
`module.exports` guard used by `extension/task-protocol.js` and
`extension/hls.js`, so they can be tested from Node; files that touch the
`browser` global at load time cannot be imported.

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
- Refuse output collisions unless `--overwrite` is explicitly supplied.
- Preserve partial output and diagnostic files after download failures.
- Keep one URL per invocation; batch downloading and authentication workflows
  are out of scope unless explicitly requested.
- Keep browser controls (pause, resume, cancel) functional for native-host
  downloads.
- Preserve cookies and Referer handling for protected media, but do not log
  cookie values or other secrets.
- Bound persistent histories and logs. The current per-download log limit is
  500 lines.
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
