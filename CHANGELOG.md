# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

The Rust package and the Firefox extension share one product version; see
"Versioning" in `AGENTS.md`.

## [Unreleased]

### Fixed

- **The popup no longer offers a Quality picker that has nothing to pick
  from.** `popup.js` had always hidden the rendition control for a playlist with
  fewer than two renditions, but `.variants { display: flex }` in `popup.css`
  overrode the `hidden` attribute — which is only a user-agent stylesheet
  `display: none`, at a precedence any author rule beats. The picker rendered
  empty on every single-rendition download. Both stylesheets now carry
  `[hidden] { display: none !important; }`.

## [0.6.0] - 2026-09-22

### Added

- **CLI discovery, selection and JSON output.** `downer URL --list` prints
  every candidate a page offers — playlists first, then files, with a master
  playlist's renditions under it — and exits without downloading. `--select N`
  or `--media URL` downloads one other than the first; naming one the page does
  not offer stops the run rather than quietly falling back. `--json` makes both
  the listing and a download's result machine-readable (`path`, `engine`,
  `bytes`, `elapsed_ms`), and that output is a stable interface carrying a
  `schema_version` — [ADR-0020](docs/adr/0020-json-output-is-a-cli-interface.md)
  records what may change and why a failure is deliberately still an exit code
  and a line on stderr rather than a JSON document. Default behaviour is
  unchanged: without `--select` or `--media`, the first candidate is downloaded
  exactly as before.

- **A documentation set.** [`docs/user-guide.md`](docs/user-guide.md) covers
  install, a first download, settings, controls, where files go and what Downer
  does not do; [`docs/troubleshooting.md`](docs/troubleshooting.md) covers the
  same ground by symptom, from "native host disconnected" through a missing
  segment total to where the logs are; and
  [`docs/architecture.md`](docs/architecture.md) covers the components, the data
  flow from page to FFmpeg, and the trust and session boundaries. `README.md` is
  now an overview, a quick start and links, and the "Known limitations" list it
  used to carry lives in the user guide next to the remedies. Nothing was
  dropped in the move.
- **Five decision records for decisions the code had always made and nobody had
  written down**: FFmpeg as the only engine, always stream-copying and never
  bundled, including why HLS segment extensions are not validated (ADR-0015);
  blocking I/O on OS threads rather than an async runtime (ADR-0016); Manifest
  V2 with a persistent background page (ADR-0017); the CLI exit codes as a
  stable interface (ADR-0018); and the native host being a mode of the CLI
  binary rather than a second program (ADR-0019). `docs/adr/README.md` indexes
  every record with its status, and `docs/adr/template.md` is the starting point
  for the next one.

- **Releases.** Pushing a `vX.Y.Z` tag now publishes a GitHub Release carrying
  the `downer` binary for macOS arm64 and Linux x86_64, the extension as
  `downer-<version>.xpi`, and a `SHA256SUMS` covering both. The workflow
  refuses to build unless `Cargo.toml`, `extension/manifest.json` and the tag
  name the same version, so a half-finished version bump fails before it
  publishes anything. `make extension-xpi` builds the same XPI from a
  checkout, reproducibly: the same commit packages to the same bytes, so a
  published checksum can be rechecked by rebuilding.
- **How to install the extension permanently**, in the README. The add-on is
  unsigned, so Firefox Release and Beta can only load it temporarily; a
  permanent install needs Developer Edition, Nightly or ESR with
  `xpinstall.signatures.required` set to `false`. AMO unlisted signing is
  documented as a future option and deliberately not implemented.
- **A rendition picker.** The popup now enumerates what an HLS master playlist
  offers — resolution and bit rate, ordered by what the playlist declares
  rather than the order it lists them in — and lets you pick one. Not picking
  downloads what it always did, the highest bandwidth, so the one-click path is
  unchanged. A media playlist, or a master with one rendition, shows no picker
  rather than a picker with nothing to decide.
- **A chosen rendition keeps its audio.** When a master carries audio outside
  the video variant (`#EXT-X-MEDIA` with a `URI`), the download now hands
  FFmpeg the video and the audio together. Previously such a master was left
  unresolved and every rendition was downloaded, because taking the video
  variant alone silently lost the sound. Measured both ways in
  `docs/adr/0014-pair-a-rendition-with-its-audio.md`.
- `--rendition <best|worst|720p|1280x720|URL>` on the CLI, doing the same
  thing. A rendition the playlist does not offer stops the download rather
  than quietly becoming another one.
- A `playlist-info` command on the native messaging protocol, and a matching
  `playlist_info` capability, so the popup can ask the host what a playlist
  offers. The extension fetches the playlist in the page's session and the host
  parses it, as `docs/adr/0011-one-playlist-parser.md` requires. An optional
  `variant_url` field on `download` carries the choice. Both are non-breaking
  additions; the protocol version stays at 1.
- Written, tested semantics for **Pause, Resume and Cancel**, in
  `docs/protocol.md` and the README, decided in
  `docs/adr/0012-control-semantics.md`. Pause is process suspension, not
  protocol-level pausing: a stopped FFmpeg holds idle sockets that a server may
  close, so a long pause can cost the download, and the popup now says so while
  the download is paused.
- A new terminal `error_code`, **`resume_failed`**: a job that fails with no
  FFmpeg output since its resume died at the resume, not at the media. The
  popup says the connection was lost while paused and offers Retry, instead of
  reporting the media as undownloadable. The state is still `failed` and there
  is still one terminal event per job.
- A **"keep the part-written file when I cancel"** setting (off by default) and
  the matching optional `keep_partial` field on the `download` request.
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
- **A "Supported platform" setup check**, first in `downer doctor` and in the
  Settings page's "Check setup" panel. It passes on macOS and Linux and fails
  by name anywhere else, because on a platform Downer does not support, being
  told that FFmpeg is fine is true and useless.
- **Downloads survive a transient network failure without you watching.**
  Every HTTP(S) input now carries FFmpeg's reconnect options, and a download
  that fails for a reason classified as *transient* — a connection reset, a
  timeout, a 5xx — restarts itself up to twice with a short backoff, instead of
  stopping and waiting for you to press **Retry**. The popup says
  "Connection lost. Reconnecting… (attempt 2 of 3)" while it happens, and
  **Cancel** works throughout, including during the wait between attempts.
  A failure that will not improve on a second try is never retried: a 403, a
  404, an unwritable path, a full disk. Tune it with `--retries N` (`0` turns
  it off), `--reconnect-delay-max SECONDS`, and `--no-reconnect`.
  [ADR-0023](docs/adr/0023-surviving-a-transient-failure.md) records the design,
  including what was measured and what is retained on judgement rather than
  evidence.
- **`--timeout SECONDS`** bounds fetching a source page or a playlist, with a
  connect timeout derived from it. The source-page client previously set
  neither, so a server that accepted a connection and then said nothing could
  hold a download for thirty seconds. It does not bound the download itself: a
  large file is not a hung one.
- **The native host keeps a log file, and Settings tells you where it is.**
  Firefox sends the host's stderr to the Browser Console and keeps nothing
  after the process exits, so the failures that matter most — a host that will
  not start, a manifest pointing somewhere stale, a missing FFmpeg — used to
  leave no trace at all. The host now appends to
  `~/Library/Logs/downer/host.log` on macOS and
  `~/.local/state/downer/logs/host.log` elsewhere: host start and stop, every
  request, every FFmpeg spawn, and every job's outcome. It is bounded at two
  files of 1 MiB and rotates, so it cannot fill a disk. **No cookie value, URL
  query string or page title can appear in it** — the command line is rendered
  with the cookie replaced by a count (`<1 cookie>`), headers by their names
  (`<User-Agent,Referer>`) and the output filename by `<output>`, because that
  filename is the page title when title naming is on. `downer doctor` and the
  Settings "Check setup" panel both name the file so it can be attached to a
  bug report, and `status` carries it as `log_path`. FFmpeg's own output is
  logged at `debug`: set `"log_level": "debug"` in
  `~/.config/downer/config.json` to turn it on, or `"off"` for no file at all.
  [ADR-0022](docs/adr/0022-a-bounded-redacted-host-log-file.md) records the
  design, including why a secret is never handed to the logger rather than
  filtered out of it.

### Changed

- **Windows is recorded as unsupported, and building for it now stops rather
  than producing a binary.** `cargo build` on any non-Unix target fails with a
  message naming both reasons — pause and resume are Unix signals, and Firefox
  registers native hosts there through the registry rather than a launcher
  script. Previously such a build succeeded and failed later, at run time, the
  first time anyone pressed Pause. A Unix that is neither macOS nor Linux is
  unaffected: it still builds, and refuses only the Firefox registration, by
  name. Nothing changes for macOS or Linux users.
  [ADR-0021](docs/adr/0021-windows-is-unsupported.md) records the decision and
  what would have to be true to revisit it.
- `downer install-host --ffmpeg` merges into an existing
  `~/.config/downer/config.json` instead of replacing it, so recording a new
  FFmpeg no longer discards a `log_level` set there.
- **The extension has a permanent add-on ID**, `downer@kz-hq.github.io`,
  replacing the placeholder `downer@example.com`. It is written only in
  `extension/manifest.json` now: the native messaging host reads it from there
  at build time, so the ID the installer allows and the ID the extension ships
  with cannot drift apart. An add-on already installed under the placeholder ID
  is a different add-on to Firefox and keeps its own settings; remove it and
  install this one.
- **Which URLs are listed as downloadable.** Detection now matches the last
  extension of the URL's *path* instead of looking for one anywhere in the
  whole URL. A page's TypeScript (`main.ts`), an image named `poster.mp4.jpg`
  and a link like `?next=.mp4` are no longer offered as media; a signed URL
  such as `/video.m3u8?token=…` still is. A `.ts` URL counts as media only when
  it looks like a packager's segment, and a segment whose playlist is also on
  the page is folded into it rather than listed beside it.
- The popup separates what the player actually loaded from what was only found
  in the page's text. The latter is collapsed under "other candidates" instead
  of ranked alongside.
- A DASH manifest is labelled `DASH` and marked experimental, rather than shown
  as an ordinary video. Nothing has validated `.mpd` against FFmpeg yet.
- A master whose only separately declared rendition is subtitles is now
  resolved to one variant like any other master. It was previously left alone
  by a guard meant for audio, which cost twice the data for a byte-identical
  file.

### Fixed

- **A download with no configured directory could land somewhere you never
  chose.** The native host fell back to `.` — its own working directory,
  inherited from however Firefox was started, so `/` from a desktop launcher —
  whenever the desktop reported no download directory. On Linux that is any
  machine without XDG user-directory configuration, which minimal installs and
  containers often lack, while the Settings placeholder promised "your
  Downloads folder" regardless. The default is now the desktop's own download
  directory, or `~/Downloads` where there is none, created on first use; a host
  with no home directory at all refuses rather than guessing. `downer doctor`
  on this machine reported `/home/user/downer` as the download directory before
  the change.
- **Check setup no longer reports a failed setup that works.** A download
  creates its output directory, but the directory check called a
  not-yet-created one "not a directory" — which, once the default became
  `~/Downloads`, was the ordinary case on a fresh Linux machine. It now asks
  whether the directory can be *created*, says "(will be created)" when it
  cannot yet be written to because it is not there, and still fails when a file
  is in the way or a parent is unwritable.

### Changed

- **A running download always shows evidence that it is running, and a stopped
  one says why in its first line.** Four defects that were one: in each, the
  host already had the answer and discarded it before the user could see it.
  - A download with no playlist totals showed "Waiting for playlist metadata…"
    for its whole run, though FFmpeg was reporting how far it had got. Progress
    events now carry `elapsed_ms` whether or not a segment total is known, and
    the popup shows elapsed media time advancing. `percent` still needs a
    total — a numerator is informative, an invented fraction is not.
  - A playlist answered with a challenge page was reported as "No HLS segments
    found". The probe now distinguishes a challenge, an unreachable server, a
    body that is not a playlist, and a playlist with genuinely no segments, and
    says which on a `metadata_error` field — using the same words the CLI
    already used for a challenge.
  - A failure was an unbroken wall of text opening with FFmpeg's own
    configuration. It now leads with the cause (`Connection refused`, `Server
    returned 404`), keeps the rest as readable lines, and is bounded — the full
    text is still in the Settings log console. The popup renders the line
    breaks that were always in the message and were collapsed by CSS.
  - A failed or cancelled download did not say where its file was. All three
    terminal states now carry `path`.
- **The native host now serves one download per process, and says so.** That is
  what the extension has always done — a native port per download, disconnected
  on the terminal event — but the host kept a job map, a duplicate-job check and
  an EOF path that cancelled *every* task, describing a multiplexing host that
  neither side implemented. The registry is now a single slot; a `download`
  arriving while one is running is rejected with `host_busy` rather than run
  alongside. `job_id` stays on the wire, so a long-lived host remains possible
  without a protocol break. Measured first: a host process costs 1.3 ms to
  reach its handshake and ~3.5 MB idle, so there was nothing to amortise. See
  `docs/adr/0013-one-download-per-host-process.md`.
- **Cancelling a download now deletes what FFmpeg had written.** It used to be
  kept, and the popup said so — but nothing had decided that; it was what
  happened when nothing deleted the file. A cancel is the user saying they do
  not want the file, so the fragment goes with it unless the new setting is on.
  A download that *fails* still keeps its part-written file whatever the
  setting says: that fragment is the evidence for the failure.
- Pause and Resume are hidden where the host reports
  `capabilities.pause_resume: false`, instead of being offered on every
  platform and failing. Cancel is still offered everywhere, including on a
  paused download.
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

- A native messaging channel that settled by *failing* — a `rejected` answering
  the request that starts a download — left its port open, so the host process
  behind it stayed alive with nothing to do until garbage collection reached
  it. Only the success path disconnected. Both do now.

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
