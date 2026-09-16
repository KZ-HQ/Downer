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
- Update the project description only when the roadmap or the architecture
  summary changes.

## Architecture decisions

Architectural decisions are recorded as ADRs under `docs/adr/` once that
directory exists (Linear issue KEI-63 creates it and seeds it with the
decisions already embodied in the code). Until then, record decisions in the
relevant Linear issue. Any change to the native messaging protocol, the host
process model, the FFmpeg command layer, discovery ownership, or control
semantics requires an ADR.

## Repository layout

- `src/`: Rust CLI, FFmpeg process layer, scraper, output handling, and native host.
- `extension/`: Firefox WebExtension files, popup, Settings page, background worker,
  content script, and native messaging protocol.
- `scripts/`: native host installation and launcher scripts.
- `tests/`: CLI integration tests.
- `dist/`: generated Firefox extension package.

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
and Firefox JavaScript/manifest validation. Record the outcome in the Linear
handoff comment.

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
- Keep generated `dist/downer-firefox.zip` synchronized when packaging is part
  of the requested change.
