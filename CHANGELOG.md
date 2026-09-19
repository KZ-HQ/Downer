# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

The Rust package and the Firefox extension share one product version; see
"Versioning" in `AGENTS.md`.

## [Unreleased]

### Added

- `downer doctor` and a **Check setup** panel on the extension's Settings page,
  both reporting the same checks: the native host is registered with Firefox
  and its launcher exists, FFmpeg runs and is new enough, and the download
  directory can be written to. Each check says what was found and, when
  something is wrong, what to do about it. `doctor` exits `6` on a failure and
  `0` on a warning, because a warning means downloads still work. The host
  answers a new `status` command; the handshake is unchanged, so no protocol
  version bump. See `docs/adr/0009-setup-diagnostics.md`.
- An **FFmpeg path** setting on the Settings page, sent with each download.
  Firefox starts the native host with a minimal environment, so a `PATH` or
  `DOWNER_FFMPEG` set in a shell cannot reach it.
- A compact warning in the popup when the setup has never been checked or the
  last check failed.
- Rust CLI `downer URL [options]` that downloads one HTTP(S) source or media
  URL per invocation through FFmpeg, with stream-copy remuxing for direct
  files and segmented streams.
- Source-page scraping that resolves direct video files and HLS playlists
  (`.m3u8`) from a page, forwarding the source page as the FFmpeg Referer and
  a browser-like User-Agent, and reporting Cloudflare challenge responses
  explicitly.
- Output handling that percent-decodes and sanitizes inferred filenames,
  writes playlist downloads as `.mp4`, resolves collisions according to
  `--on-conflict`, and retains partial files after failures.
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

- A shared URL redaction rule, implemented once in `src/redact.rs` for the
  native host and once in `extension/redact.js` for the extension, pinned by the
  case table in `tests/fixtures/redaction.json` that both test suites read.
- A signed HLS playlist at `/media/signed.m3u8` in the `make fixture-site`
  origin, whose segment URLs carry a token in their query string, so redaction
  can be checked end to end through a real browser and a real FFmpeg. Its
  request log prints paths with the query replaced, never the token.
- Automated end-to-end tests against a real, headless Firefox in `tests/e2e/`:
  they install `extension/` as a temporary add-on, assert that Firefox loads
  the manifest and the background scripts, exchange messages with the
  background script, and scan the shared `tests/fixtures/pages/` HTML through
  an injected content script, asserting what the jsdom tests assert of the same
  fixtures. The harness speaks WebDriver to geckodriver directly and adds no
  dependency. `make extension-browser` installs Firefox and geckodriver from
  conda-forge — the one source reachable from a network-restricted container,
  as `docs/e2e-firefox.md` explains — `make extension-e2e` runs the tests, and
  a separate CI job does both. They are not part of `make check`, which still
  needs no browser, and they skip rather than fail when none is installed.
- `scripts/session_start.sh`, registered in `.claude/settings.json` as a
  SessionStart hook, so a Claude Code cloud session starts with that browser
  already installed. It exits before touching anything unless
  `CLAUDE_CODE_REMOTE` is exactly `true`, which only a cloud session VM sets,
  so a checkout on a contributor's own machine installs nothing;
  `DOWNER_SKIP_BROWSER_INSTALL=1` turns it off in a cloud session as well.
- `downer install-host` and `downer uninstall-host`, which register and remove
  the Firefox native messaging host from the binary itself. Installation copies
  the binary to a stable per-user location (`--link` registers it where it is)
  and points Firefox at a generated launcher, so the registration keeps working
  when the repository is moved or deleted. Uninstall removes the manifest, the
  launcher and the config, and the copied binary with `--binary`; removing
  something already gone is not an error. See
  [`docs/adr/0008-relocatable-native-host-installation.md`](docs/adr/0008-relocatable-native-host-installation.md).
- `downer install-host --ffmpeg PATH`, recording an FFmpeg path in
  `~/.config/downer/config.json` for the native host to use. Firefox launches
  the host with a minimal environment, so `DOWNER_FFMPEG` is not something a
  user can set for it; the recorded path is used unless `DOWNER_FFMPEG` does
  override it.

### Changed

- HLS playlists are now parsed in one place. The extension fetches them — only
  the page's context carries the session a protected CDN answers — and sends the
  text to the native host, which reads it for the segment totals and the
  rendition to download. `extension/hls.js` is gone. The two parsers had already
  drifted: a playlist with a space after `#EXTINF:` gave the extension a segment
  count and the host none. The host also makes no playlist request of its own
  when the extension supplied one. See `docs/adr/0011-one-playlist-parser.md`.
- Downloading an HLS **master** playlist now fetches one rendition instead of
  all of them. FFmpeg opens every variant a master lists and keeps only the
  best, so the rest was downloaded and discarded — two renditions meant twice
  the data for the same file. The host now resolves the master to the rendition
  it already counts, costing one small request and no media. An unreadable
  playlist, or one whose audio is a separate rendition, falls back to the
  previous behaviour rather than failing or losing a track. The file produced is
  unchanged. See `docs/adr/0010-resolve-hls-master-playlists.md`.
- The FFmpeg command layer now models a download rather than an argument list.
  `FfmpegInvocation` names what a run is — input, headers, cookies, HLS
  leniency, threads, overwrite, output, and a reporting mode — and renders argv
  in one place, replacing three constructors and an index-based `splice` that
  edited the arguments after the fact. The four `download_resolved*` functions
  collapse into one entry point taking `Hooks`. No user-visible behaviour
  changes; the one argv difference is that a controlled download no longer
  passes a redundant `-loglevel error` before `-loglevel info`. See
  `docs/adr/0007-structured-ffmpeg-command-model.md`.
- The native host registration no longer points into the repository checkout.
  `scripts/install_native_host.sh` is now a thin development wrapper around
  `downer install-host --dev`, and `scripts/native-host.sh` is gone: the
  launcher Firefox runs is generated at install time.
- A download whose media URL has no useful filename is now named `video.mp4`
  rather than after the source page's title. A fixed default is predictable,
  where a derived one varies with the site, the login state and the locale.
  Naming after the page title is still available but **off by default**:
  `--name` on the command line, or "Name downloads after the page title" on the
  extension's Settings page. The source-host fallback is gone — it only ever
  applied when no title was supplied, which is now the normal case. See
  `docs/adr/0005-default-output-name-over-derived-one.md`; this supersedes the
  naming half of ADR-0004, whose collision policy is unchanged.
- The collision suffix is now `_2`, `_3`, … instead of ` (2)`, ` (3)`, … — no
  quoting needed in a shell, and no separator surprises in other tools. Because
  the default name now repeats for every generically named download, renaming is
  the common path rather than the exception; nothing is overwritten unless asked.
- The native protocol is unchanged and stays at version 1. `title` remains an
  accepted optional field; the extension simply does not send it unless the user
  opts in, so the opt-in needs no field of its own.
- The end-to-end suite now covers the whole download path in a real Firefox, not
  just the browser: `tests/e2e/native-download.test.mjs` drives the content
  script's `document.title` through the popup message, the background script, a
  real native messaging port, the Rust host and FFmpeg, to the file that lands
  on disk. It pins the collision-free naming rule's acceptance criterion — two
  pages whose playlists are both `index.m3u8` produce two distinct, title-based
  filenames — plus the rename sequence, the Settings collision policy, and that
  a failed download leaves no file behind. It needs a registered native host and
  an FFmpeg 7.1+, so it skips out loud with a `SKIP:` line wherever those are
  missing, including CI. `make extension-ffmpeg` installs a suitable FFmpeg
  beside the test browser.
- The fixture site serves its playlist a second time as `/media/index.m3u8` and
  titles each instance's page after its own host and port, so two origins
  produce two different filenames from an identically named playlist. Every
  playlist it served before had a distinctive stem, which no naming rule would
  ever reach.

- A download whose media URL has a generic filename — `index`, `playlist`,
  `master`, `download`, `video`, `media`, or digits only, which covers most HLS
  playlists — is now named after the source page's title, falling back to the
  source host. The extension sends the page title; `--name` supplies one on the
  command line. A title is sanitized like any inferred name and truncated to 80
  characters, and never appears in a log, an error, or any native event other
  than the finished file's path. Media URLs with a real filename are unaffected.
- An output collision is now resolved by renaming rather than refusing, when
  *Downer* inferred the filename: the download is written as `name (2).mp4`,
  `name (3).mp4`, and so on, and nothing existing is replaced. An exact
  `--output` path still fails on a collision. `--on-conflict
  fail|rename|overwrite` sets the policy explicitly, the Settings page offers
  the same choice for extension downloads, and `--overwrite` is now shorthand
  for `--on-conflict overwrite` (the two cannot be combined). This replaces the
  previous "refuse every collision unless `--overwrite`" rule; see
  `docs/adr/0004-output-naming-and-collision-policy.md`. A download that fails
  before FFmpeg writes anything leaves no file behind, and the next attempt gets
  the same name rather than being pushed onto ` (2)`; anything FFmpeg did write
  is still preserved.
- The native messaging protocol's `download` request gains two optional fields,
  `title` and `on_conflict`, without a `protocol_version` bump. `overwrite` is
  superseded by `on_conflict` but still accepted. An absent `on_conflict` means
  `rename`; a value outside the documented set is refused rather than ignored.
- Per-download FFmpeg logs are stored under their own `downloadLogs:<jobId>`
  key instead of inside each job record, and storage writes are coalesced in a
  300 ms window, immediately on a terminal state. Saving job state no longer
  rewrites every log line of every job. Records written before this change are
  migrated on the next start: their logs are redacted, moved to the new key, and
  removed from the job record.
- FFmpeg log lines reach the Settings page as their own `download-log` message
  carrying a batch, rather than as a full job-state broadcast per line, so the
  popup is no longer re-rendered once per line of FFmpeg output.
- Persisted logs are bounded by total bytes as well as by the 500-line limit,
  so one very long line cannot fill `storage.local`.

### Fixed

- An FFmpeg older than the supported 7.1 no longer fails every HLS download with
  an unreadable error. `-allowed_segment_extensions` and `-extension_picky` exist
  only from 7.1, so on FFmpeg 6.x — what `apt install ffmpeg` gives on Ubuntu
  24.04 — every playlist download died during argument parsing with
  `Unrecognized option 'allowed_segment_extensions'`, while direct-file downloads
  kept working and nothing suggested the FFmpeg was the problem. The version is
  now detected at startup, those two options are omitted below 7.1 so the
  download proceeds, and both the CLI and the extension say *FFmpeg 6.1.1 is
  older than the minimum supported 7.1* — as a warning up front, and in the error
  if the download then fails (exit `4`, unavailable FFmpeg). Omitting the options
  costs nothing on 6.x: the strict segment-extension checking they switch off was
  introduced in 7.1 as well, measured both ways in
  [ADR-0006](docs/adr/0006-ffmpeg-version-detection.md). 7.1 remains the
  supported minimum; an older FFmpeg is warned about, not supported.
- The second HLS download no longer fails with "output already exists". Because
  most playlists are called `index.m3u8`, `playlist.m3u8` or `master.m3u8` and
  the extension always requested `overwrite: false`, any two HLS downloads
  collided on the same inferred filename — from different sites, about different
  videos — with no way to proceed from the popup.
- A signed token in a URL that FFmpeg echoes through its own stderr is no longer
  persisted. FFmpeg's HLS demuxer logs one `Opening '<url>' for reading` line per
  segment, and those URLs routinely carry one; the line was forwarded as a log
  event and kept in `storage.local` until "Clear logs" was pressed. URLs in log
  lines and error messages now keep their scheme, host, port and path, with the
  query replaced by `?…`, a fragment by `#…`, and any `user:password@` by `…@` —
  applied by the host before the event is sent and again by the extension before
  anything is stored or displayed. Tokens persisted by an earlier build are
  scrubbed when the extension next starts. Verified against FFmpeg 6.1.1 driven
  at a loopback server, including end to end through the native host: the
  per-segment `Opening '<url>' for reading` line is emitted at `-loglevel info`,
  which is the level the download path uses, and two further lines carry the same
  URL.
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
- URL redaction covers query strings and fragments. A site that signs URLs
  inside a path segment (`/hls/<token>/seg.ts`) is not covered, because nothing
  distinguishes such a segment from an ordinary one without knowing the site.
- A job's own media URL is stored whole, including its query: the popup matches
  persisted jobs to page media by exact URL and a re-download needs it. The
  Settings page redacts it at display.
- Batch downloads, authentication automation, provider-specific scraping,
  robust resume, and live playlist scheduling are not implemented.
- AES-128/SAMPLE-AES, byte ranges, discontinuities, and alternate HLS tracks
  are not validated.
