# ADR-0001: Version the native messaging protocol and separate request rejection from job failure

* Status: Accepted
* Date: 2026-09-16
* Issue: [KEI-50](https://linear.app/kzhq/issue/KEI-50)

## Context

The protocol between the Firefox extension and `downer --native-host` existed
only as Rust structs and JavaScript that pattern-matched on `state` strings. It
had no version field, no handshake, and no written schema. Four consequences
mattered:

1. **No compatibility check.** Firefox loads the extension from its own store
   or a temporary add-on, while the native host is a binary the user built and
   registered separately. The two drift routinely — a stale
   `target/release/downer` is the project's most common "it worked before"
   report — and nothing detected it. A host missing a command answered with a
   generic failure indistinguishable from a download failure.

2. **A rejected request could kill a running job.** An unsupported or malformed
   request produced `state: "failed"` with no `job_id`. The client's
   `NativeTaskChannel.handleMessage` treated any `failed` as terminal for the
   job owning the port, so one bad control message ended a live download's
   channel while FFmpeg kept running — the job showed as failed, and its
   pause/cancel controls stopped working, with no way to stop the process short
   of closing Firefox. `tests/native_host.rs` pinned this behaviour and
   deferred the decision to this ADR.

3. **Event kinds were inferred, not stated.** Progress and log events shared
   one stream and were told apart by whether a `log` field happened to be
   present. Any future event kind would have needed another such sniff.

4. **No written contract.** Every question about optionality, ordering, or
   error semantics was answered by reading both implementations.

## Decision

### Keep JSON, `snake_case`, and Firefox's framing

The 4-byte little-endian length prefix is mandated by Firefox. JSON and
`snake_case` on the wire are kept; the extension's `camelCase` job objects stay
an internal representation, mapped in one place. Changing any of this would
have been churn with no benefit.

### `protocol_version` as a single integer on every message

Every request and response carries `protocol_version`. The integer is the major
version; there are no minor versions. Adding an optional field is
non-breaking and does not bump it — clients are required to ignore unknown
fields. Anything else does bump it.

A request that omits `protocol_version` is treated as legacy version `0` and
still served, for one release cycle, so an extension built before this change
keeps working. That allowance is removed at the next bump.

Rejected alternative: semantic `major.minor` version strings. With one producer
and one consumer shipped from the same repository, minor versions would only
add parsing and comparison code for a distinction nothing would use.

### A `hello` handshake on every connection

The extension sends `hello` on connect and waits for the answer before sending
`download`. The host replies with `protocol_version`, `host_version`, and
`capabilities` (`pause_resume`, which is false off Unix because pause uses
process signals; `hls_info`). On a version mismatch the extension refuses to
start the download and fails the job with a message naming both versions and
telling the user to rebuild with `make extension`.

The handshake costs one round trip over an already-open pipe and does no I/O,
which matters because a host process is started per download — settled since as
the process model, in
[ADR-0013](0013-one-download-per-host-process.md), which measured that round
trip at 1.3 ms from spawn to reply.

Rejected alternative: a `status` command used for diagnostics only, with no
enforcement. That leaves the mismatch to be discovered as a confusing download
failure, which is the problem.

### `rejected` and `control-error` are connection states, not job states

This is the substantive behaviour change. The protocol now distinguishes:

* **Job states** — `starting`, `downloading`, `paused`, `cancelling`,
  `completed`, `failed`, `cancelled`. The last three are terminal and always
  carry the `job_id` they belong to.
* **Connection states** — `ready`, `rejected`, `control-error`. They describe
  one request. They are never terminal.

A client settles a job only on a terminal state that names that job, so an
unattributed failure can no longer end a download. A `rejected` event ends a
job in exactly one case: when the request it refuses is the one that would have
started that job.

Two behaviours changed with it, and their tests changed in the same commit:

* An unsupported or malformed request now reports `state: "rejected"` with
  `error_code`, instead of `state: "failed"`. It still carries no `job_id` when
  none is known, but that no longer matters, because `rejected` is not terminal
  in the first place.
* A `download` naming a `job_id` that is already running is now `rejected` with
  `duplicate_job` rather than `failed`. Previously it reported `failed` with
  the *running* job's `job_id` — the worst case of this defect, since a
  duplicate start actively terminated the client's view of a healthy download.

Rejected alternative: keep `failed` and require clients to check for a matching
`job_id`. The `job_id` check is necessary and has been added, but it is not
sufficient on its own: `duplicate_job` legitimately carries the running job's
ID, so state alone would still be ambiguous. Two vocabularies make the rule
statable in one sentence and testable directly.

### An explicit `type` field on every response

Every response carries `type`: `hello`, `ack`, `progress`, `log`, `terminal`,
`rejected`, or `control-error`. Event kind is no longer inferred from the
presence of `log`.

`type` and `state` are deliberately both present and not redundant: `type` says
what kind of event this is, `state` says what the job or request is now. The
client routes on `type` where a kind matters and on `state` where the job's
condition matters; `state` remains the field the popup renders.

Rejected alternative: derive the kind from `state` alone. `progress` and `log`
share `state: "downloading"`, so that does not work without reintroducing a
field-presence sniff.

### A stable `error_code` beside the human-readable `error`

`error` stays human-readable and its wording is free to change. `error_code`
is stable and machine-readable: `invalid_request`, `unsupported_command`,
`unsupported_protocol_version`, `duplicate_job`, `task_not_active`,
`invalid_hls_info`, `control_failed`, `download_failed`, `cancelled`.

### One shared vocabulary file, read by both test suites

`tests/fixtures/protocol.json` lists the protocol version, commands, event
types, states, error codes, and capabilities. `tests/native_host.rs` and
`tests/extension/task-protocol.test.js` both read it and assert their own
implementation's constants against it.

It is a *checked* shared file, not a *generated* one: neither language's
constants are produced from it. Code generation would need a build step in two
toolchains and a generated-file check in CI to keep the outputs honest, for a
vocabulary of roughly twenty short strings. Reading the same file from both
test suites catches the same drift at the point where it would be introduced.

## Consequences

* An extension and a host that disagree on the protocol now say so in the
  popup, naming both versions, instead of failing a download opaquely.
* A malformed or unsupported message can no longer end a running download.
  Rejections are correlated to their request and surfaced as the job's
  `controlError`, leaving the job's own state alone.
* `docs/protocol.md` is the written contract, and `tests/native_host.rs` is its
  executable form. A change to either without the other is a bug.
* Every response is larger by `protocol_version` and `type` — roughly 40 bytes
  against a 1 MiB limit, on a channel that carries one message per progress
  tick. Not material.
* Per AGENTS.md, any future change to the protocol, the host process model, the
  FFmpeg command layer, discovery ownership, or control semantics needs its own
  ADR. The process model (ADR-0013), pause/resume/cancel semantics (ADR-0012), and
  the job state machine (KEI-56) are owned elsewhere and deliberately not
  settled here; this ADR fixes only the wire contract.
