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
`1` rather than reading a payload it cannot trust. EOF on stdin cancels every
active download and exits `0`.

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
| `download` | `url` | `job_id`, `request_id`, `source_url`, `output_dir`, `overwrite`, `cookie`, `user_agent`, `threads`, `total_segments`, `total_duration_ms` |
| `pause` | `job_id` | `request_id` |
| `resume` | `job_id` | `request_id` |
| `cancel` | `job_id` | `request_id` |
| `hls-info` | `job_id`, `total_segments`, `total_duration_ms` | `request_id` |

Notes:

* `job_id` is chosen by the extension. If `download` omits it the host
  generates one and reports it on the first event. `job_id` stays on the wire
  even though the current process model runs one job per process.
* `url` and `source_url` must be `http://` or `https://`.
* `cookie` and `user_agent` are forwarded to FFmpeg as request headers. Cookie
  values are never logged, by either side.
* `total_segments` / `total_duration_ms` on `download` seed HLS progress before
  FFmpeg starts; `hls-info` supplies them later for an already-running job.

## Responses (host → extension)

Every response carries `protocol_version`, `type`, and `ok`. `type` is
explicit: an event's kind is never inferred from which optional fields happen
to be present.

| `type` | `state` | Meaning |
| --- | --- | --- |
| `hello` | `ready` | Handshake answer. Adds `host_version` and `capabilities`. |
| `ack` | `paused`, `downloading`, `cancelling` | A control command was applied. Echoes `request_id`. |
| `progress` | `starting`, `downloading`, `paused` | Job progress. May carry `completed_segments`, `total_segments`, `percent`. |
| `log` | `downloading` | One line of FFmpeg stderr, in `log`, redacted (see below). |
| `terminal` | `completed`, `failed`, `cancelled` | The job ended. `completed` carries `path`; the others carry `error` and `error_code`. |
| `rejected` | `rejected` | The host refused the request. Carries `error`, `error_code`, and the `request_id` being refused. **Never terminal.** |
| `control-error` | `control-error` | The host understood a control command but could not apply it. Carries `error`, `error_code`, `job_id`, `request_id`. **Never terminal.** |

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
| `task_not_active` | `control-error` | A control command named a job the host is not running. |
| `invalid_hls_info` | `control-error` | `hls-info` supplied zero or missing segment totals. |
| `control_failed` | `control-error` | The command was understood but could not be applied (for example pause on a non-Unix platform, or a job already cancelled). |
| `download_failed` | `terminal` | FFmpeg failed, or the media could not be resolved. Partial output is kept. |
| `cancelled` | `terminal` | The job was cancelled, by `cancel` or by EOF on stdin. |

## Optional fields

A client must tolerate any documented optional field being absent, and must
ignore fields it does not recognise — that is how this protocol adds
non-breaking fields without a version bump. Absent is not the same as zero:
`total_segments` absent means "unknown", while `0` would mean an empty
playlist.

## Testing

`tests/native_host.rs` starts the real binary and speaks this protocol over
stdio against a generated fake FFmpeg, so every statement above is pinned
without Firefox, the network, or a real FFmpeg. A failing test there is a
protocol decision to make deliberately, not a test to adjust.
