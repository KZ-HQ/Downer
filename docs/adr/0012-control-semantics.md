# ADR-0012: What pause, resume and cancel promise

* Status: Accepted
* Date: 2026-09-20
* Issue: [KEI-65](https://linear.app/kzhq/issue/KEI-65)

## Context

`ProcessControl` has implemented pause as `SIGSTOP`, resume as `SIGCONT` and
cancel as `kill()` since the native host existed. Nothing said what any of them
guaranteed. Three gaps followed from that silence:

* A resume that failed was reported as an ordinary download failure, so a user
  whose pause had outlived a CDN connection was told the media could not be
  downloaded — which was untrue, and pointed at the wrong remedy.
* A cancelled download left its part-written file behind, and the popup said so
  ("The partial file was kept for diagnostics"). Nobody chose that; it was what
  happened when nothing deleted the file.
* `capabilities.pause_resume` was reported in `hello` and ignored by the
  extension, which offered Pause on every platform.

## What pausing actually does

Measured, FFmpeg 6.1.1, `-progress` at a 0.5 s stats period:

| | progress blocks written |
| --- | --- |
| 2 s running | 4 |
| + 3 s under `SIGSTOP` | 4 |
| + 3 s after `SIGCONT` | 10 |

A stopped FFmpeg writes nothing at all — not a repeated timestamp, nothing. The
same is true of a running FFmpeg whose HTTP input has gone quiet: pointed at a
server that sent half a file and then stopped without closing, it wrote one
block and then none for the next five seconds.

Pause is therefore **process suspension, not protocol-level pausing**. FFmpeg
does not know it has been paused. Its sockets stay open and idle, and what
happens to them is the server's decision, not ours.

## Decision

### Pause is best-effort, and a long pause can lose the download

Documented rather than defended against. Holding a connection open through an
arbitrary pause would mean a segment scheduler of our own, which is M5's job
(the issue puts byte-range resume explicitly out of scope). Until then the
honest thing is to say so where the user is deciding how long to leave it — the
popup's paused message now does.

### A failure with no output since the resume is `resume_failed`

`ProcessControl` records FFmpeg's latest `out_time_ms` and the value it stood at
when resume was issued. A resume that signals a live process sets a flag; the
first progress report whose timestamp *exceeds* the resume mark clears it. A
download that fails with the flag still set failed at the resume, and gets
`error_code: "resume_failed"` instead of `download_failed`.

The alternative — a time window after `SIGCONT` — was rejected because it
measures the wrong thing. A download can be resumed and run for ten more minutes
before failing for unrelated reasons; elapsed output distinguishes those cases
and a stopwatch does not.

The state is still `failed` and there is still exactly one terminal event per
job. Only the code and the advice change: the popup says the connection was
lost rather than that the media could not be downloaded, and Retry — which the
`failed` state already offers — is the right button rather than a shot in the
dark.

**This detects a resume that *fails*, not one that *hangs*.** A server that
closes the connection makes FFmpeg exit non-zero and lands here. A server that
accepts the socket and never answers leaves FFmpeg blocked with no timeout set,
and the job simply sits there. That is the resilience issue's territory, not
this one's.

### Cancelling deletes the part-written file; failing keeps it

The two cases were treated alike because neither had been decided. They are not
alike:

* A **cancel** is the user saying they do not want this file. Leaving a
  fragment that will not play in their downloads folder makes cleaning up their
  problem, and they have to guess which of the files there is the dead one.
* A **failure** is the download saying something went wrong. The fragment is
  the evidence, it may be most of a large download, and deleting it is
  irreversible at the worst possible moment.

So: cancel deletes by default, failure always keeps. `keep_partial: true` on the
`download` request opts a cancel out of the deletion; absent means delete, which
is what an older extension and every request from a default install send. The
deletion is best-effort — the download has already ended and the user has
already been told, so a file that cannot be removed does not turn a clean cancel
into an error report.

**EOF on the port is not a cancel.** The host stops every running job when the
native port closes — Firefox quitting, the extension reloading, a crash — and
that path keeps the file whatever `keep_partial` says. Nobody asked for it, and
the extension already promises the opposite: it reconciles such a job to
`interrupted` and tells the user "partial file kept". `ProcessControl`
distinguishes the two with `cancel` and `cancel_for_shutdown`, and only the
first makes the file eligible for deletion. Without that split the behaviour
would also have been a race, since the host exits immediately after stopping
its jobs and nothing guarantees a deletion gets to run.

This reverses the default the issue proposed (`--keep-partial`, default on).
The issue's default preserves today's accidental behaviour; nothing argued for
it beyond its being what already happened.

### The CLI gets no flag

The issue asks for `--keep-partial`. There is nothing for it to do: the CLI
installs no signal handler, so `Ctrl-C` terminates the process outright and no
cleanup code runs. There is no cancel to have a policy about. A flag in `--help`
that changes nothing is worse than its absence, so `DownloadOptions::keep_partial`
is set to `true` at the CLI's construction site — inert, and commented as such —
and the flag waits for the CLI to have a cancel worth naming.

### Pause is hidden where it cannot work

The host already reports `capabilities.pause_resume` (false off Unix). The
extension now keeps what the handshake returned on the job and hides Pause and
Resume when it is `false`. Cancel is not hidden: `kill()` works everywhere, and
it works on a *stopped* process too, so a paused download never has to be
resumed before it can be given up on.

Absent capabilities mean a host too old to report them, which is a host that
does support pausing — the field arrived long after the commands did. Absent is
not false.

## Consequences

* One new `error_code`, `resume_failed`, added to `tests/fixtures/protocol.json`
  and to the taxonomy in `docs/protocol.md`. No version bump: it is an added
  optional value, and a client that does not recognise it sees `failed`, which
  is true.
* One new optional request field, `keep_partial`. Same reasoning; ADR-0004 set
  the precedent.
* A cancel that loses the race — FFmpeg exits successfully in the moment
  between the `cancel` and the kill — reports `cancelled` and keeps the file.
  Deliberate: the deletion is on the failure path only, and what is on disk in
  that case is a complete, playable download rather than a fragment. Deleting a
  finished file is not what "no part-written files" asks for.
* A user who relied on the old behaviour — cancel, then inspect the fragment —
  turns the Settings toggle on. The popup no longer claims a file was kept when
  it was not, which is the defect this replaces.
* Retry after a resume failure restarts the download from the beginning under
  the user's own conflict policy. It does not write over the fragment: the
  failed job's actual output path is not reported to the extension, and
  `download` takes an output *directory*, never a path, so "overwrite" would
  name whatever the retry infers rather than the file that was left behind.
  Resuming into a partial file is byte-range resume, which the issue puts out of
  scope.
