# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

The Rust package and the Firefox extension share one product version; see
"Versioning" in `AGENTS.md`.

## [Unreleased]

### Added

- Rust CLI `downer URL [options]` that downloads one HTTP(S) source or media
  URL per invocation through FFmpeg, with stream-copy remuxing for direct
  files and segmented streams.
- Source-page scraping that resolves direct video files and HLS playlists
  (`.m3u8`) from a page, forwarding the source page as the FFmpeg Referer and
  a browser-like User-Agent, and reporting Cloudflare challenge responses
  explicitly.
- Output handling that percent-decodes and sanitizes inferred filenames,
  writes playlist downloads as `.mp4`, refuses collisions unless
  `--overwrite` is supplied, and retains partial files after failures.
- Stable exit codes: `2` invalid input, `3` output-path problem, `4` FFmpeg
  unavailable, `5` media or FFmpeg failure.
- Acceptance of HLS playlists with nonstandard segment names, including
  JPEG-named video segments.
- Firefox WebExtension that scans the active page for media, forwards the
  page's cookies and User-Agent to the native host for the selected download,
  and shows progress, completed/total HLS segments, and an estimated
  percentage.
- Native messaging host (`downer --native-host`) implementing the `hello`,
  `download`, `pause`, `resume`, `cancel`, and `hls-info` commands with
  streamed progress and FFmpeg log events.
- A single definition of the extension's download job state machine in
  `extension/job-state.js` — states, legal transitions, the terminal set, and
  the presentation predicates the popup uses — replacing the state-name lists
  that were repeated across `background.js` and `popup.js`.
- Versioned native messaging protocol (version 1), specified in
  `docs/protocol.md` and decided in
  `docs/adr/0001-native-messaging-protocol.md`. Every request and response
  carries `protocol_version` and every response carries an explicit event
  `type` and, on failures, a stable `error_code`. The extension performs a
  `hello` handshake on connect and refuses to start a download against a host
  that speaks a different protocol version, explaining the mismatch and naming
  both versions instead of failing the download opaquely. Requests that omit
  `protocol_version` are still served for one release cycle.
- Pause, resume, and cancel controls in the popup; pause and resume use Unix
  process signals on macOS and Linux, cancel is supported on all platforms.
- Settings page with a configurable output directory, an optional FFmpeg
  processing-thread count, and a live FFmpeg log console keeping the most
  recent 500 lines per download.
- `Makefile` targets for setup, checks, builds, native-host installation, and
  extension packaging, plus native-host installation scripts for macOS and
  Linux.
- MIT `LICENSE`, this changelog, a declared Rust MSRV, and a documented
  minimum FFmpeg version.
- Integration tests for the native messaging host, Node unit tests for the
  extension's task protocol and HLS helpers, HLS fixtures shared by the Rust
  and JavaScript tests, and `web-ext lint` in `make check`.
- `data_collection_permissions` declared as `none` in the extension manifest.
- GitHub Actions CI running `make check`, a release build and extension
  packaging on macOS and Linux, a job that builds against the declared MSRV,
  and a `make version-check` target enforcing that the Cargo package and the
  extension manifest share one version.
- Two further ways to supply a session cookie to the CLI, both of which keep
  the value out of shell history: `--cookie-file PATH` reads the header from a
  file, and the `DOWNER_COOKIE` environment variable is used when neither
  cookie option is given. Precedence is `--cookie`, then `--cookie-file`, then
  `DOWNER_COOKIE`. Supplying `--cookie` and `--cookie-file` together is an
  error with exit status `2`.
- `docs/adr/0002-cookie-scoping-and-argv-exposure.md`, recording that cookie
  values remain visible in FFmpeg's process arguments — FFmpeg has no
  file-based header or cookie input — and that the eventual fix is the native
  HLS scheduler, where the host fetches segments itself and FFmpeg is never
  given a cookie.
- A dependency-free loopback HTTP server in `tests/support/` that records the
  headers it receives, and `tests/cookie_scope.rs`, which drives a real FFmpeg
  against two of them through a redirect and a cross-host HLS segment to
  establish which requests a forwarded cookie actually reaches, plus an opt-in
  probe reporting which spellings of `-cookies` FFmpeg honours. The tests that
  need a real FFmpeg skip when none is present, as in CI.

### Fixed

- A forwarded cookie is no longer sent to every host FFmpeg contacts for an
  input. It was rendered as a `Cookie:` line in the `-headers` block, which
  FFmpeg applies to every request, so a redirect target and — for HLS — a
  cross-host segment or key server received the media host's session. Cookies
  now reach FFmpeg as `-cookies` entries scoped to the media URL's host, which
  FFmpeg matches per request. Verified against FFmpeg 9.0.1: a scoped cookie
  stays off both a redirect target and a cross-host HLS segment server.
- A cookie or User-Agent containing CRLF can no longer append headers of its
  own choosing to the block passed to FFmpeg as `-headers`. The block is
  assembled by concatenation, so any ASCII control character is now removed
  from a value before it is placed in a header line.
- A download that was still running when Firefox closed is no longer stuck
  forever. Native ports do not survive a restart, so such a job had no process
  behind it, yet it was restored from storage as `downloading`: the popup showed
  "Downloading…" with a disabled button, and Pause and Cancel could only answer
  "Download task is no longer active." The background script now reconciles any
  restored job that was still active into a new terminal `interrupted` state,
  and the popup explains it and offers the download again. Records written
  before this state existed are reconciled on load.
- Two downloads of the same media URL no longer fight over one popup row. The
  newest job owns the row, and an older one cannot take it back, so a finished
  or interrupted job can no longer overwrite a live download's progress and
  controls.
- A late or duplicated native event can no longer revive a job that has already
  finished: job state changes are checked against the state machine, and
  terminal states are final.
- A malformed, unsupported, or duplicate native messaging request can no longer
  terminate a running download. Such requests are answered with the
  non-terminal `rejected` state instead of `failed`, and the extension now
  settles a job only on a terminal state that names that job. Previously one
  bad control message ended a live download's channel while FFmpeg kept
  running, leaving the job shown as failed and its controls inoperable.
- The popup's headline status is no longer set by a persisted download job from
  an earlier session. `get-download-statuses` returns every stored job, and
  `renderDownloadStatus` updated the headline outside its row-lookup guard, so a
  job cancelled days earlier for an unrelated page announced itself as this
  page's status while every candidate row read "Not started". The popup now
  renders a job only when it belongs to media listed on the page being viewed,
  and a job is allowed to set the headline only when it also began in the
  current browser session.
- A download that was still running when the browser closed no longer presents
  as live. Such a job is persisted as `downloading` and never resumes, but its
  URL could match a candidate on the page, giving a row with the Download button
  disabled and Pause/Cancel wired to a native task that no longer exists — the
  row stayed unusable until storage was cleared. It is now shown as
  "Interrupted — not running", with Download enabled and no controls.
- The extension now detects media linked with an anchor. `<a href="movie.mp4">`
  was never scanned: `href` was missing from the content script's attribute
  list, an anchor is not a fetched resource until it is clicked, and the
  page-markup fallback matched only absolute URLs, so a relative `href` was
  missed as well. The content script's attribute list is now identical to
  `src/scraper.rs::extract_media_urls` (`href`, `data-file`, `file`, and
  `video_url` were missing on the JavaScript side; `data-hls` was missing on the
  Rust side), and page markup is scanned for those attributes with relative
  values resolved against the page. The `blob:`/`data:` exclusion and the
  http/https-only rule are unchanged.
- Extension HLS variant selection now reads `BANDWIDTH` when it is the first
  attribute of `#EXT-X-STREAM-INF` (previously the first variant was chosen
  instead of the highest-bandwidth one), and no longer mistakes the first
  segment of a media playlist for a variant.

### Known limitations

- `--threads` controls FFmpeg processing, not concurrent HLS segment HTTP
  requests.
- A cookie supplied to the CLI or forwarded by the extension is visible in
  FFmpeg's process arguments to other processes on the same machine for the
  duration of a download.
- HLS progress totals depend on successfully reading a VOD playlist;
  Cloudflare or other session protections can prevent metadata access even
  when a browser player can load the media.
- Completed HLS segment counts are estimated from FFmpeg output duration.
- Batch downloads, authentication automation, provider-specific scraping,
  robust resume, and live playlist scheduling are not implemented.
- AES-128/SAMPLE-AES, byte ranges, discontinuities, and alternate HLS tracks
  are not validated.
