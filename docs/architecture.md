# Architecture

How Downer is put together, and why the pieces sit where they do. For the wire
format between the extension and the native host, see
[`protocol.md`](protocol.md) — it is the contract, and this document does not
restate it. For the decisions behind any of this, see
[`adr/`](adr/README.md).

## The shape of it

Downer is one Rust binary and one Firefox extension.

The binary is both a command-line downloader and, when run with
`--native-host`, the native messaging host the extension talks to
([ADR-0019](adr/0019-native-host-as-a-mode-of-the-cli.md)). Either way it ends
in the same place: an FFmpeg child process that does all the fetching and
remuxing, always with `-c copy`
([ADR-0015](adr/0015-ffmpeg-as-the-only-engine.md)).

The extension exists for one reason. Protected media needs the session the
browser already has, and a command-line tool does not have it. Everything about
the extension's design follows from moving as little as possible across that
gap.

## Data flow

A download through the extension runs the whole path:

```
  page
    │  DOM + resource entries
    ▼
  content.js ──────────────► media-scan.js      detection rules
    │                                           (mirrors src/scraper.rs)
    │  candidates, and the playlist text
    │  fetched in the page's own session
    ▼
  popup.js ─────────────────► job-view.js       which jobs this popup shows
    │  download-media
    ▼
  background.js                                 owns jobs, cookies, ports, logs
    │  runtime.connectNative  (one port per download)
    ▼
  downer --native-host  (src/native.rs)
    │  spawn, no shell
    ▼
  ffmpeg ──────────────────► the file on disk
```

Each hop drops something. The content script sees the page but never a cookie
value; the popup sees candidates but owns no state; the background script holds
the cookies but never parses a playlist; the host parses and downloads but never
sees the page. That is the design, not an accident of layering — the reasons are
under [Trust and session boundaries](#trust-and-session-boundaries).

The CLI path is the same minus the first four hops: `src/cli.rs` →
`src/scraper.rs` → `src/ffmpeg.rs`.

## Components

### Extension

| File | Owns |
| --- | --- |
| `media-scan.js` | The detection rules — which attributes and extensions count as media. Kept identical to `src/scraper.rs::extract_media_urls`, pinned by `tests/fixtures/media-extensions.json`, which both test suites read ([ADR-0011](adr/0011-one-playlist-parser.md)). |
| `content.js` | Scans the page's DOM and resource entries, and fetches an HLS playlist **in the page's own session**. |
| `popup.js` | Starts downloads and renders status and controls. Holds no authoritative state. |
| `job-view.js` | Decides which persisted jobs a popup renders: only media on the page being viewed, and only jobs begun in this browser session may set the headline status. |
| `background.js` | Owns everything durable — job records, cookies, playlist metadata, native ports, progress, log history — and reconciles jobs that were still active when the browser closed. |
| `job-state.js` | The job state machine: states, legal transitions, the terminal set, and the predicates the popup renders from. |
| `task-protocol.js` | Correlates native acknowledgements and terminal responses by job and request ID. |
| `redact.js` | The URL redaction rule, mirrored from `src/redact.rs` and pinned by `tests/fixtures/redaction.json` ([ADR-0003](adr/0003-redact-urls-in-logs.md)). |
| `job-logs.js` | Bounds log history: 500 lines and 128 KiB per download, over at most 20 jobs. |

The background script is persistent, which is what lets it hold a native port
for the length of a download
([ADR-0017](adr/0017-manifest-v2-persistent-background-page.md)). It stores logs
under a `downloadLogs:<jobId>` key of their own rather than inside the job
record, and coalesces storage writes and log broadcasts, so a long HLS download
does not rewrite all state once per line of FFmpeg output.

### Rust

| File | Owns |
| --- | --- |
| `src/main.rs` | Entry point. Pre-parses `--native-host` before clap, because Firefox appends arguments of its own ([ADR-0019](adr/0019-native-host-as-a-mode-of-the-cli.md)). |
| `src/native.rs` | The native messaging host: framing, the handshake, launching download workers, forwarding progress and logs, and controlling FFmpeg. |
| `src/ffmpeg.rs` | Invokes FFmpeg without a shell, parses `-progress`, captures stderr, and applies pause/resume signals. The invocation is a structured value rendered in one place ([ADR-0007](adr/0007-structured-ffmpeg-command-model.md)). |
| `src/scraper.rs` | Resolves source-page media URLs and parses HLS playlists. The only playlist parser in the project. |
| `src/output.rs` | Validates URLs, infers and sanitizes filenames, and applies the conflict policy ([ADR-0004](adr/0004-output-naming-and-collision-policy.md), [ADR-0005](adr/0005-default-output-name-over-derived-one.md)). |
| `src/host.rs` | Registers and removes the native messaging host: the manifest, the launcher that supplies `--native-host`, the durable binary location, and the recorded FFmpeg path ([ADR-0008](adr/0008-relocatable-native-host-installation.md)). |
| `src/diagnostics.rs` | The setup checks behind `downer doctor` and the protocol's `status` ([ADR-0009](adr/0009-setup-diagnostics.md)). |
| `src/redact.rs`, `src/error.rs`, `src/failure.rs` | Redaction, the error taxonomy, and the mapping to exit codes ([ADR-0018](adr/0018-stable-cli-exit-codes.md)). |

Concurrency is OS threads blocking on child processes and sockets — four spawn
sites in total, no async runtime in this project's own code
([ADR-0016](adr/0016-blocking-io-and-os-threads.md)).

## Trust and session boundaries

Three boundaries matter, and they are not the same line.

### The session boundary: only the page has it

A protected CDN answers the browser and refuses everyone else. That session
lives in the page's context and cannot be moved, so **the fetch does not move**:
`content.js` retrieves the playlist and the text travels to the host, which
parses it. The host keeps its own fetch as a fallback for when the extension
could not get one, and the CLI — which has no session at all — always uses it.
[ADR-0011](adr/0011-one-playlist-parser.md) has the case that forced this: a CDN
answering the host with a challenge page while the in-session fetch succeeded.

### The trust boundary: the native port

Everything above `runtime.connectNative` runs inside Firefox under its
permissions. Everything below it is a process on the user's machine with the
user's full rights. That is the boundary the protocol guards, and the reason it
is versioned, framed with a size limit, and refuses a handshake it does not
recognise ([ADR-0001](adr/0001-native-messaging-protocol.md)).

Two rules hold it:

* **Nothing reaches a shell.** URLs and paths are passed to FFmpeg as process
  arguments, never interpolated. This is a rule in `AGENTS.md` as well as a
  property of `src/ffmpeg.rs`.
* **The host allows exactly one extension ID.** `downer@kz-hq.github.io`, read
  from `extension/manifest.json` at build time into `downer::host::EXTENSION_ID`
  so it cannot drift.

### The secret boundary: cookies go one hop further than they look

Cookies are collected by the background script for the media URL only, sent
across the port for one download, and scoped at the host so a redirect or a
cross-host HLS segment server receives nothing
([ADR-0002](adr/0002-cookie-scoping-and-argv-exposure.md)).

Two leaks are known and handled rather than closed. FFmpeg takes headers on its
command line, so a cookie is visible in its argv while a download runs — FFmpeg
offers no file-based alternative, and ADR-0002 records the acceptance. And
FFmpeg logs one URL per segment to stderr, signed query strings included, which
is why redaction exists on both sides of the port
([ADR-0003](adr/0003-redact-urls-in-logs.md)).

No host event carries a cookie value, and
`tests/native_host.rs::no_host_event_carries_the_cookie_value` greps a sentinel
through every event the host emits to keep it that way.

## The protocol

[`protocol.md`](protocol.md) is the contract: framing, versioning, every request
and response, the error taxonomy, the ordering guarantees, and what the controls
promise. It is implemented by `src/native.rs` and
`extension/task-protocol.js`, and its wire vocabulary is pinned in
`tests/fixtures/protocol.json`, which both test suites read — so neither
language can rename a protocol term on its own.

**One host process runs one download.** The extension opens a port per download
and disconnects on the terminal event, which is EOF for the host, which exits.
`hello` and `status` use their own short-lived connection.
[ADR-0013](adr/0013-one-download-per-host-process.md) has the measurements and
the case against a long-lived host.

## Job state

The wire state machine is in [`protocol.md`](protocol.md#per-job-state-machine)
and is authoritative. In short: `starting` → `downloading` ⇄ `paused`, ending in
exactly one of `completed`, `failed` or `cancelled`, with `cancelling` as the
acknowledgement in between.

**The extension's terminal set is wider than the protocol's.** `job-state.js`
adds two states that never appear on the wire:

* `preparing` — the background script is collecting cookies and playlist
  metadata, before it has connected to anything.
* `interrupted` — a job restored from storage that was still active when the
  browser closed. Native ports do not survive a restart, so no process is behind
  it. Terminal for the extension; meaningless to the host.

Both exist because the extension's job outlives the host's, on both ends: it
begins before the port opens and can be found again after the process is gone.

What the controls guarantee — including that pause is process suspension rather
than protocol-level pausing, and that a long pause can cost the download — is in
[ADR-0012](adr/0012-control-semantics.md) and summarized in
[`protocol.md`](protocol.md#what-the-controls-guarantee).

## What is deliberately not here

* **A queue.** One URL per invocation; concurrency is the client's to arrange by
  opening more connections.
* **Concurrent HLS segment fetching.** FFmpeg fetches segments serially.
  `--threads` controls FFmpeg's processing threads and nothing else. A native
  scheduler is designed in KEI-68 and would take fetching from FFmpeg while
  leaving the remux with it.
* **Resume of a failed download.** Partial files are kept as evidence, not as
  something to continue from. Byte-range resume is M5's.
* **Windows.** Native hosts are registered in the registry there, and
  pause/resume use Unix signals.

## Testing the seams

The boundaries above are where this project's tests are aimed, and the ones that
matter are in [`AGENTS.md`](../AGENTS.md) under "Continuous integration". Two
are worth knowing here:

* `tests/fixtures/protocol.json` and `tests/fixtures/redaction.json` are read by
  both the Rust and the JavaScript suites, so a rule implemented once per
  language cannot drift.
* `tests/e2e/native-download.test.mjs` runs the entire flow above in a real
  headless Firefox, through a real native port, to a file on disk.
  [`e2e-firefox.md`](e2e-firefox.md) explains the harness and why it skips
  loudly rather than passing vacuously when its prerequisites are missing.
