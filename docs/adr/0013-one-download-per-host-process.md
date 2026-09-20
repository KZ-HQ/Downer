# ADR-0013: One download per host process

* Status: Accepted
* Date: 2026-09-20
* Issue: [KEI-53](https://linear.app/kzhq/issue/KEI-53)

## Context

Two process models were in the codebase. Only one was ever used.

The extension has always opened a native port **per download**:
`background.js::nativeDownload` calls `browser.runtime.connectNative` for each
job, and `checkSetup` opens its own short-lived one for `hello` + `status`. A
terminal event disconnects the port, which is EOF for the host, which exits.

The host was written as though it might serve several. `src/native.rs` kept an
`ActiveTasks` map keyed by `job_id`, rejected a duplicate `job_id`, and
cancelled *every* task on EOF. A map that can only ever hold one entry is not a
neutral choice: it says a multiplexing host exists, so every reader has to work
out for themselves that it does not.

Nothing documented which model was intended. ADR-0001 hedged — "the current
process model starts a host process per download… if the process model later
becomes a long-lived host (KEI-53)" — and `docs/protocol.md` carried the same
uncertainty.

## What a process actually costs

The case for a long-lived host is amortisation. Measured on this machine,
release build, 20 runs each:

| | median | min | max |
| --- | --- | --- | --- |
| spawn → `hello` reply | **1.3 ms** | 1.2 | 2.0 |
| spawn → process exited | 2.4 ms | 2.3 | 3.1 |
| `ffmpeg -version` | **30.7 ms** | 28.3 | 321.7 |

Idle resident memory is ~3.5 MB per host process; eight concurrent came to
27 MiB, and each would sit beside an FFmpeg that dwarfs it.

So the host process is not the cost. The only per-process work worth naming is
`ffmpeg -version`, at ~31 ms — memoized per executable *within* a process
(`ffmpeg::version`), so paid once per download. Against a download measured in
seconds to minutes, amortising it saves nothing anyone can perceive.

## Decision

**One download per host process.** `hello` and `status` get their own
short-lived connection, as they already do.

The host now says so in its own structure: the registry is
`Option<(String, ActiveTask)>` rather than a map. A `download` arriving while a
job is running is rejected — `duplicate_job` when it names the running job,
`host_busy` when it names a different one. Neither is terminal, and the running
download is untouched.

`job_id` stays on the wire. It is what makes this decision reversible: a
long-lived host could be introduced later without a protocol break, because
every event and every control command already names its job.

### Why not the long-lived host

It buys four things, and none of them is worth what it costs here.

* **Amortised FFmpeg discovery** — 31 ms, measured above.
* **A job registry that outlives popup reloads.** The popup is not the owner of
  job state; the background script is, and it already persists jobs to
  `storage.local` (KEI-55, KEI-56). A host-side registry would be a second
  source of truth to reconcile against, not a replacement for the first.
* **A status connection without a download.** Already solved, and more simply:
  `checkSetup` opens its own port. That is the right shape regardless — the
  user presses "Check setup" precisely when no download is running, often
  because none can be started.
* **A queue.** Explicitly out of scope per `AGENTS.md`.

Against that it costs fault isolation: one FFmpeg that wedges its host takes
every other download with it. Today a wedged host loses exactly one download,
and the OS reclaims everything when it exits. It would also need reconnection
and state reconciliation on both sides — a protocol the extension does not have
and would have to be tested into existence.

There is a further reason to prefer per-download ports that is not about cost.
A long-lived port is a bet that the background page stays alive to hold it.
Under Manifest V2 that bet is safe, because the background page is persistent.
Under an MV3 event page it is a bet on suspension behaviour we have not
established. A per-download port makes no such bet: it exists only while a
download does. The MV3 assessment itself is KEI-73, and this decision
deliberately leaves it less to undo.

## Consequences

* `ActiveTasks: HashMap` → `ActiveJob: Option<(String, ActiveTask)>`;
  `cancel_all` → `cancel_for_shutdown`; lookups go through `running_task`, and
  every terminal path releases the slot through `clear_job`.
* One new error code, `host_busy`, in the taxonomy and in
  `tests/fixtures/protocol.json`, alongside a new
  `max_downloads_per_connection: 1` that states the model where both test
  suites can read it. Non-breaking: a client that does not recognise the code
  still sees a `rejected` that is not terminal.
* **A latent process leak is closed.** Under this model a settled channel must
  end its process, and that could not depend on *how* it settled:
  `NativeTaskChannel.finish` disconnected the port, `fail` did not. A `rejected`
  answering the start request therefore settled the channel and left the host
  process alive with nothing to do, until garbage collection happened to reach
  it. Both paths now disconnect. No client could reach that case today — which
  is why it survived, and why it is pinned by a test now.
* The host does not exit when a job ends; it releases the slot and waits for
  the port to close. That keeps EOF as the single shutdown path, and means a
  second download on a finished connection would work rather than wedge. The
  extension never sends one, but the behaviour is defined instead of accidental.
* Concurrency is bounded by whatever Firefox allows in native ports rather than
  by anything here. Eight concurrent hosts cost 27 MiB; the FFmpeg processes
  are the real limit, and that was already true.
