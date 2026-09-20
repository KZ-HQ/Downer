# Downer native messaging protocol

Version **1**.

This is the contract between the Firefox extension (`extension/background.js`
and `extension/task-protocol.js`) and the Rust native messaging host
(`downer --native-host`, implemented in `src/native.rs`). The reasoning behind
it is recorded in [ADR-0001](adr/0001-native-messaging-protocol.md).

The wire vocabulary — protocol version, command names, event types, state
strings, error codes, capabilities — is listed once in
`tests/fixtures/protocol.json`. `tests/native_host.rs` and
`tests/extension/task-protocol.test.js` both read that file, so neither
implementation can add or rename a term without the other test suite noticing.

## Framing

Firefox's native messaging framing, which this protocol does not change: each
message is a 4-byte little-endian unsigned length prefix followed by that many
bytes of UTF-8 JSON.

Both directions are limited to **1 MiB** per message. A length prefix larger
than 1 MiB is unrecoverable — the host writes no response and exits with status
`1` rather than reading a payload it cannot trust. EOF on stdin stops the
running download and exits `0`. That is a shutdown, not a cancel anyone asked
for, so each job keeps its part-written file whatever `keep_partial` said.

All wire field names are `snake_case`. The extension's internal job objects are
`camelCase`; the mapping lives in `handleNativeEvent` in `background.js` and is
not part of this contract.

## Versioning

Every request and every response carries `protocol_version`, an integer. The
integer **is** the major version: there are no minor versions, and any change
that would break either side bumps it.

* The host answers requests whose `protocol_version` equals its own.
* A request that **omits** `protocol_version` is treated as legacy version `0`
  and still served, so an extension built before the handshake keeps working
  for one release cycle. This allowance is removed when the version next bumps.
* Any other value is refused with a `rejected` event carrying
  `error_code: "unsupported_protocol_version"`.

The extension performs the handshake on every connection (see `hello`) and
refuses to start a download against a host whose `protocol_version` differs
from its own, reporting the mismatch as the job's failure message. Because a
host process is started per download, the handshake is one request and one
response and does no I/O.

The protocol version is independent of the product version. `host_version` in
the `hello` response is the product version (`Cargo.toml`), reported for
diagnostics only; nothing keys behaviour off it.

## Requests (extension → host)

Every request has `command` and should have `protocol_version`. `request_id`,
when present, is echoed on the response that answers it, and is how the
extension correlates acknowledgements. The extension generates request IDs as
`<job_id>-<n>`, and `<job_id>-start` for the request that starts a download.

| Command | Required fields | Optional fields |
| --- | --- | --- |
| `hello` | — | `request_id` |
| `status` | — | `request_id`, `output_dir`, `ffmpeg` |
| `download` | `url` | `job_id`, `request_id`, `source_url`, `output_dir`, `title`, `on_conflict`, `overwrite`, `keep_partial`, `cookie`, `user_agent`, `threads`, `total_segments`, `total_duration_ms`, `playlist_text`, `ffmpeg` |
| `pause` | `job_id` | `request_id` |
| `resume` | `job_id` | `request_id` |
| `cancel` | `job_id` | `request_id` |
| `hls-info` | `job_id`, `total_segments`, `total_duration_ms` | `request_id` |

Notes:

* `job_id` is chosen by the extension. If `download` omits it the host
  generates one and reports it on the first event. `job_id` stays on the wire
  even though one host process runs one download (see **Process model**): it is
  what would let a long-lived host be introduced later without a protocol
  break, since every event and control command already names its job.
* `url` and `source_url` must be `http://` or `https://`.
* `output_dir` is where the download is written. **Absent means the desktop's
  own download directory**, or `$HOME/Downloads` where the desktop has none —
  on Linux that is any machine without XDG user-directory configuration, which
  is the common case in containers and minimal installs. The directory is
  created if it does not exist. A host with no home directory either refuses
  the download rather than guessing; it used to fall back to its own working
  directory, inherited from however Firefox was started. `status` reports the
  same resolved directory, so the Settings panel and a download cannot
  disagree. See KEI-90.
* `ffmpeg` on `download` and `status` names the FFmpeg the user chose on the
  Settings page. Absent, the host discovers one as it always has
  (`DOWNER_FFMPEG`, then the install-time configuration, then the usual paths).
  It exists because Firefox starts the host with a minimal environment, so
  neither `PATH` nor `DOWNER_FFMPEG` can carry the answer.
* `cookie` and `user_agent` are forwarded to FFmpeg as request headers. Cookie
  values are never logged, by either side.
* `total_segments` / `total_duration_ms` on `download` seed HLS progress before
  FFmpeg starts; `hls-info` supplies them later for an already-running job.
* `playlist_text` is the playlist the extension fetched **in the page's
  context**, sent instead of the totals it used to compute itself. The division
  is deliberate: the extension fetches, because only there is the page's session
  — cookies, Referer, service-worker tokens — and a challenged CDN answers
  nothing else; the host parses, because one implementation should decide what a
  playlist means. The host reads it for the segment totals and, for a master, for
  the rendition to download, and so makes no request of its own. Absent, the host
  fetches as it always has, including the `../playlist.m3u8` fallback, so a
  playlist the extension could not reach still gets its chance. See
  [ADR-0011](adr/0011-one-playlist-parser.md).
* `title` is the source page's title, used to name the output file when the
  media URL's own filename stem is generic (`index`, `playlist`, `master`,
  `download`, `video`, `media`, or digits only). **It is opt-in and absent by
  default**: the extension omits it unless the user turns on "name downloads
  after the page title", and a `download` without it is named `video.<ext>`.
  Sending or omitting the field *is* how the opt-in is expressed, which is why
  no separate flag joins this contract (ADR-0005). It is naming material, not
  diagnostics: the host sanitises and bounds it, and never echoes it in a `log`,
  a `progress` or an `error` — the `path` of a `terminal` event is the only
  response it can reach. See
  [ADR-0004](adr/0004-output-naming-and-collision-policy.md).
* `on_conflict` is `"fail"`, `"rename"` or `"overwrite"` and decides what
  happens when the inferred output path is already taken. **Absent means
  `"rename"`**: the host only ever infers a filename into `output_dir`, never an
  exact path, so renaming to `name (2).ext` is the right default for every
  request it can receive — including one from an extension built before this
  field existed. A value that is none of the three is refused with
  `invalid_request` rather than ignored, because a misread collision policy is
  the one misunderstanding that can destroy a file.
* `keep_partial` decides what happens to the part-written file when the job is
  **cancelled**. **Absent means delete it**: a cancel is the user saying they do
  not want this file, and a fragment that will not play is litter they did not
  ask for. `true` keeps it. This governs an explicit `cancel` only: a job
  stopped because the port closed keeps its fragment either way, since nobody
  asked for that one. A download that *fails* keeps its fragment either way
  too — this field says nothing about that case, because the fragment is then
  the evidence for the failure. Deletion is best-effort and never turns a clean
  cancel into an error. See
  [ADR-0012](adr/0012-control-semantics.md).
* `overwrite` is **superseded by `on_conflict`** and kept for older clients.
  `on_conflict` wins when both are present; `overwrite: true` on its own still
  means overwrite. The extension sends `on_conflict` and leaves `overwrite`
  `false`, so a host too old to understand `on_conflict` keeps refusing rather
  than replacing a file.

## Responses (host → extension)

Every response carries `protocol_version`, `type`, and `ok`. `type` is
explicit: an event's kind is never inferred from which optional fields happen
to be present.

| `type` | `state` | Meaning |
| --- | --- | --- |
| `hello` | `ready` | Handshake answer. Adds `host_version` and `capabilities`. |
| `status` | `ready` | Setup-check answer. Adds `host_version`, `capabilities` and `status`. |
| `ack` | `paused`, `downloading`, `cancelling` | A control command was applied. Echoes `request_id`. |
| `progress` | `starting`, `downloading`, `paused` | Job progress. May carry `completed_segments`, `total_segments`, `percent`, `elapsed_ms`, `metadata_error`. |
| `log` | `downloading` | One line of FFmpeg stderr, in `log`, redacted (see below). |
| `terminal` | `completed`, `failed`, `cancelled` | The job ended. All three carry `path`; `failed` and `cancelled` also carry `error` and `error_code`. |
| `rejected` | `rejected` | The host refused the request. Carries `error`, `error_code`, and the `request_id` being refused. **Never terminal.** |
| `control-error` | `control-error` | The host understood a control command but could not apply it. Carries `error`, `error_code`, `job_id`, `request_id`. **Never terminal.** |

### Progress fields

* `completed_segments`, `total_segments` and `percent` require playlist totals.
  Absent, they are **absent** rather than zero — the host does not know.
* `elapsed_ms` is how far into the *media* FFmpeg has got, and is reported
  whenever FFmpeg has said, **with or without** playlist totals. For a download
  whose totals could not be read it is the only evidence anything is happening;
  a client should show it advancing rather than a static "waiting". It is a
  numerator, not a fraction: `percent` stays absent without a total, because an
  invented percentage is worse than none.
* `metadata_error` says why the segment total is unavailable, in the host's own
  words — a challenge, an unreachable server, a body that is not a playlist, or
  a playlist with no segments. It rides on a `progress` event because **the
  download is still running**: the probe is a convenience, and FFmpeg fetches
  the playlist for itself in a session the probe does not have. It is never a
  job state and never terminates anything.

### `path` on a terminal event

All three terminal states carry `path`, not only `completed`.

* `completed` — where the media is.
* `failed` — where the part-written file is. A failure keeps its fragment
  (ADR-0012), and naming it is the difference between "something went wrong"
  and something the user can act on.
* `cancelled` — where the file *was*. Under the default policy it has just been
  deleted; saying where it was is still the honest answer, and a client must not
  claim the file is there unless it asked for `keep_partial`.

The path is the one the host inferred from `output_dir`, the URL, any `title`
and the conflict policy, so a client cannot compute it for itself.

### Redaction

The `log` field and a `terminal` event's `error` are **redacted** before they are
sent: in every URL they contain, the scheme, host, port and path are kept and the
query becomes `?…`, a fragment becomes `#…`, and `user:password@` becomes `…@`.
Only `http` and `https` URLs are matched, and a line is truncated to 2,000
characters after redaction.

This is not a versioned part of the vocabulary — no field is added, removed or
renamed by it, and `protocol_version` stays at 1 — but it is a contract: a client
must not assume `log` reproduces FFmpeg's stderr byte for byte. The extension
applies the same rule again on its side before persisting or displaying anything,
so an older host is safe to talk to. The rule is implemented by `src/redact.rs`
and `extension/redact.js`, pinned by `tests/fixtures/redaction.json`, and the
reasoning is in `docs/adr/0003-redact-urls-in-logs.md`.

`capabilities` on the `hello` response:

* `pause_resume` — pause and resume use Unix process signals, so this is
  `true` on macOS and Linux and `false` elsewhere.
* `hls_info` — the host accepts the `hls-info` command.

### Terminal versus connection states

`completed`, `failed`, and `cancelled` are **job** states and end a job. They
always carry the `job_id` they belong to, and a client must settle a job only
on a terminal state that names that job.

`ready`, `rejected`, and `control-error` are **connection** states. They
describe one request, not a job, and must never end a job. This is the rule
that closes the original defect: before version 1, a malformed or unsupported
message produced `state: "failed"` with no `job_id`, and the client treated any
`failed` as terminal for the job owning the port — so a bad control message
ended a running download's channel while FFmpeg kept running.

A `rejected` event ends a job in exactly one case: when its `request_id` is the
one that would have started that job (`<job_id>-start`), because in that case
no job was ever created.

## Setup checks (`status`)

`status` answers "is this installation able to download anything?". It is a
separate command from `hello`, not extra fields on it, because **the handshake
does no I/O and this does**: it runs `ffmpeg -version` and writes a probe file
into `output_dir` to see whether a download could be saved there. The handshake
runs before every download; this runs when a user presses "Check setup". See
[ADR-0009](adr/0009-setup-diagnostics.md).

The response carries a `status` object:

| Field | Meaning |
| --- | --- |
| `host_version` | The product version, as in `hello`. |
| `protocol_version` | The version this host speaks. |
| `platform` | `macos`, `linux`, or `unsupported`. |
| `ffmpeg_path` | The FFmpeg a download started now would use. |
| `ffmpeg_version` | As FFmpeg reports it, when it could be run. Absent when it could not. |
| `ffmpeg_ok` | Whether FFmpeg ran at all. An FFmpeg below the supported minimum is still `true`: it downloads (ADR-0006). |
| `checks` | The individual checks, below. |

Each entry in `checks` has:

| Field | Meaning |
| --- | --- |
| `name` | A stable identifier — `host_registration`, `ffmpeg`, `output_directory`. Safe to key UI and tests off; never translated. |
| `title` | A human label for the check. |
| `outcome` | `pass`, `warn`, or `fail`. |
| `detail` | What was found, present even on a pass — "which FFmpeg?" is the question the panel exists to answer. |
| `remedy` | What to do. Present whenever `outcome` is not `pass`, absent when it is. |

`warn` is a real third outcome rather than a soft failure: an FFmpeg older than
the supported minimum still downloads, so calling it a failure would be wrong,
and saying nothing would be wrong too.

`output_dir` absent does **not** skip the directory check: the host resolves the
same default `download` would use — the platform's Downloads folder — and checks
that. Most users configure no directory, so skipping would leave the commonest
setup the one nothing is checked for. The check is skipped only when no default
can be resolved at all, which on Linux means a machine with no XDG user-dirs
configuration; there the download itself has no default either.

The checks the host cannot perform are the ones about reaching it. When the
registration is missing `connectNative` fails and there is no host to ask; when
the two sides disagree on `protocol_version` the handshake is refused by
whichever side is newer. The extension renders these itself, as
`host_connection` and `protocol_version`, because they need different
instructions — re-registering the host does not fix a version mismatch, which is
ordinary upgrade skew between two halves that ship separately.

`downer doctor` reports the same checks from the command line, minus those two:
a CLI run speaks to no extension.

## Process model

**One host process runs one download.** The extension opens a native port per
download and disconnects it on the terminal event, which is EOF for the host,
which exits. `hello` and `status` use their own short-lived connection, because
the user runs setup checks precisely when no download is going.

What follows from that, and what a client may rely on:

* A `download` arriving while this host already has a running job is
  **rejected**, never queued and never run alongside: `duplicate_job` if it
  names the running job, `host_busy` if it names another. Neither is terminal
  and the running download is untouched.
* A settled channel must disconnect its port. A host process whose port stays
  open stays alive with nothing to do; EOF is the only shutdown path.
* The host does **not** exit when a job ends. It releases its slot and waits for
  the port to close, so a second `download` on a finished connection is served
  rather than refused.
* EOF stops the running job **without** deleting its part-written file,
  whatever `keep_partial` said. That is a shutdown, not a cancel anyone asked
  for (ADR-0012).
* Concurrency is the client's to arrange, by opening more connections. Nothing
  in this protocol multiplexes.

`job_id` is on the wire anyway, so this is reversible. See
[ADR-0013](adr/0013-one-download-per-host-process.md) for the measurements
behind it and why a long-lived host was not chosen.

## Per-job state machine

```
                    ┌──────────────► cancelled
                    │                    ▲
starting ──► downloading ⇄ paused        │
                    │         │          │
                    │         └──────────┤
                    ├──► cancelling ─────┘
                    ├──► completed
                    └──► failed
```

* `starting` is emitted once, when the host has registered the job.
* `downloading` and `paused` alternate as pause/resume are applied.
* `cancelling` acknowledges a `cancel`; the `cancelled` terminal event follows
  once FFmpeg has actually stopped.
* Exactly one terminal event is emitted per job, and nothing follows it.
* `preparing` and `interrupted` are **extension-only** states. The background
  script sets `preparing` while it fetches cookies and playlist metadata before
  connecting, and sets `interrupted` when it restores a job that was still
  active when the browser closed — native ports do not survive a restart, so
  such a job has no process behind it. The host never emits or accepts either.
  `interrupted` is terminal *for the extension*, which makes the extension's
  terminal set wider than this protocol's: a native channel is still only ever
  settled by `completed`, `failed`, or `cancelled`. Both states are defined in
  `extension/job-state.js`.

### What the controls guarantee

* **`pause`** suspends the FFmpeg process (`SIGSTOP`). It is not protocol-level
  pausing: FFmpeg does not know it has been paused, and its open sockets sit
  idle. A stopped FFmpeg produces no output and no progress events at all —
  measured, not assumed; see [ADR-0012](adr/0012-control-semantics.md).
  **A long pause can cost the download**, because servers close idle
  connections and expire signed segment URLs on their own schedule. Pause is
  available only where `capabilities.pause_resume` is true.
* **`resume`** continues the process (`SIGCONT`). It is best-effort for the same
  reason: nothing was held open on the user's behalf while it was stopped. A job
  that fails with no FFmpeg output since the resume reports `resume_failed`
  rather than `download_failed`, so a client can say the connection was lost and
  offer a retry rather than declaring the media undownloadable.
* **`cancel`** is immediate and works everywhere, including on a *stopped*
  process — a paused download does not have to be resumed before it can be
  cancelled. It is acknowledged with `cancelling` and settled by the `cancelled`
  terminal event once FFmpeg has actually stopped.
* `pause` and `cancel` may be sent before FFmpeg has started. The host applies
  them as the process appears rather than losing them to the race.

## Ordering guarantees

* `starting` precedes every other event for a job.
* Exactly one terminal event per job, and it is the last event for that job.
* `progress` and `log` events for a job may interleave in any order; log lines
  for one job preserve FFmpeg's own order. A `log` event's `state` is always
  `downloading` and reports nothing the job did not already know, so a client
  must not treat one as a state change.
* A response echoing `request_id` may arrive after unrelated `progress` or
  `log` events. Correlate by `request_id`, never by arrival order.
* Progress is monotonic in intent but not guaranteed: `completed_segments` is
  estimated from FFmpeg output timestamps against playlist-derived totals, so a
  later total (via `hls-info`) can move it.

## Error taxonomy

`error_code` is a stable machine-readable string; `error` is human-readable and
may change wording.

| `error_code` | `type` | Cause |
| --- | --- | --- |
| `invalid_request` | `rejected` | The frame was not valid JSON for a request. No `request_id` or `job_id` is available. |
| `unsupported_command` | `rejected` | `command` is not one of the commands above. |
| `unsupported_protocol_version` | `rejected` | `protocol_version` is neither the host's version nor absent. |
| `duplicate_job` | `rejected` | A `download` named a `job_id` that is already running. The running job is untouched. |
| `host_busy` | `rejected` | A `download` arrived while this host was already running a *different* job. One host process runs one download; see **Process model**. The running job is untouched. |
| `task_not_active` | `control-error` | A control command named a job the host is not running. |
| `invalid_hls_info` | `control-error` | `hls-info` supplied zero or missing segment totals. |
| `control_failed` | `control-error` | The command was understood but could not be applied (for example pause on a non-Unix platform, or a job already cancelled). |
| `download_failed` | `terminal` | FFmpeg failed, or the media could not be resolved. The part-written file is kept. |
| `resume_failed` | `terminal` | The job failed after a `resume`, with no further FFmpeg output since it. The input is fine; the connections FFmpeg was holding while stopped are not. The `state` is still `failed`. |
| `cancelled` | `terminal` | The job was cancelled, by `cancel` or by EOF on stdin. A `cancel` deletes the part-written file unless `keep_partial` was `true`; EOF always keeps it, because nobody asked for that one. |

## Optional fields

A client must tolerate any documented optional field being absent, and must
ignore fields it does not recognise — that is how this protocol adds
non-breaking fields without a version bump. `title` and `on_conflict` were added
this way; ADR-0004 records why they did not bump the version. `keep_partial` and
the `resume_failed` code were added the same way, for the reason ADR-0012 gives:
a client that does not recognise `resume_failed` still reads `state: "failed"`,
which is true. Absent is not the
same as zero: `total_segments` absent means "unknown", while `0` would mean an
empty playlist.

Ignoring an unrecognised *field* is not the same as tolerating an unrecognised
*value* in a field the host does know. `on_conflict` is the case where that
distinction matters: its value decides whether an existing file survives, so a
value outside the documented set is refused rather than guessed at. The
documented set, like the rest of the vocabulary, is listed in
`tests/fixtures/protocol.json` (`on_conflict_policies`, `default_on_conflict`,
`default_keep_partial`, `max_downloads_per_connection`).

## Testing

`tests/native_host.rs` starts the real binary and speaks this protocol over
stdio against a generated fake FFmpeg, so every statement above is pinned
without Firefox, the network, or a real FFmpeg. A failing test there is a
protocol decision to make deliberately, not a test to adjust.
