# ADR-0023: Survive a transient failure by retrying the whole download, bounded and announced

* Status: Accepted
* Date: 2026-09-22
* Issue: [KEI-66](https://linear.app/kzhq/issue/KEI-66)
* Follows: [ADR-0012](0012-control-semantics.md), which is what a retry must not
  break; [ADR-0004](0004-output-naming-and-collision-policy.md), whose renaming
  rule decides where the retry loop can live;
  [ADR-0007](0007-structured-ffmpeg-command-model.md), which is how the
  reconnect options are expressed

## Context

A download was one attempt. FFmpeg was invoked with `-c copy` and no HTTP
resilience options at all, and anything that came back as a failure became a
failed job with a **Retry** button — a button that starts the download again
from the beginning, which is exactly what an automatic retry would do, except
that it needs the user to be watching.

The page and playlist fetches were no better bounded.
`scraper::resolve_source` built its client with **neither** a total nor a
connect timeout, inheriting reqwest's 30-second default for the whole request
and nothing at all for establishing the connection — so a host that accepted a
socket and then said nothing held a download for the full budget.
`hls_info_with_timeout` had 8s/4s hard-coded, and none of it was configurable.

## What was measured

The issue's Context asserts that "one dropped connection or a single 5xx on an
HLS segment fails the whole job", and proposes the reconnect options as "the
cheapest large win". Both were tested rather than taken on trust, on
2026-09-22, against **FFmpeg 9.0.2** (conda-forge, the version
`make extension-ffmpeg` installs), using a local server that serves a generated
three-segment HLS stream and refuses `stream1.ts` once with a 503:

| Configuration | Exit | Frames (of 15) |
| --- | --- | --- |
| No injection — the baseline | 0 | 15 |
| One 503, reconnect options **on** | 0 | **15** |
| One 503, reconnect options **off** (`--no-reconnect`) | 0 | **15** |
| One 503, `-xerror` | 183 | — |

**The premise does not hold, and neither does the proposed direction.** A single
5xx on a segment does not fail the job: FFmpeg recovers and the file comes out
whole. And it comes out whole *with the reconnect options turned off*, so on
this path they change nothing that can be measured here.

Two earlier readings of this were wrong and are recorded because the mistakes
are instructive. A first version of the test asserted only that the download
succeeded, which it does with or without the options — a vacuous test that
would have "proved" the feature worked. A first version of the harness was a
single-threaded server, and the frames it lost were requests it never answered;
that was briefly reported as FFmpeg silently truncating the output, which it
does not do. The test in `tests/reconnect_503.rs` now asserts the **frame
count**, and carries a control that runs the same injection with
`--no-reconnect` and prints what it finds, so neither mistake can recur quietly.

What could not be measured here is the failure this is actually for: real packet
loss, a TCP reset under load, a DNS blip, a CDN node dropping out mid-transfer.
A loopback server cannot produce those.

## Decision

### FFmpeg reconnects, and the statuses are narrower than the issue proposed

Every HTTP(S) input carries `-reconnect 1 -reconnect_streamed 1
-reconnect_on_network_error 1 -reconnect_on_http_error 5xx,408,429
-reconnect_delay_max 30`, emitted per input so a separately declared audio
playlist (ADR-0014) is as protected as the video one.

The issue suggests `4xx,5xx`. **It is `5xx,408,429`.** A 403 or a 404 will not
become a 200 by being asked again, so reconnecting across the whole 4xx range
turns a dead link into repeated requests against a server that has already given
its final answer — someone else's server, from a user's address. 408 Request
Timeout and 429 Too Many Requests are the two 4xx that mean "later", so they are
in.

These are kept despite measuring no benefit above. They cost nothing when
nothing goes wrong, they are what FFmpeg's HTTP layer is for, and the failures
they address are the ones a loopback test cannot stage. The honest summary is
*unproven here, cheap, and standard* — not *measured to help*. `--no-reconnect`
turns them off, for a server that behaves worse when a request is retried and
for reproducing a failure that reconnection would paper over.

### The whole download retries, twice, only when the failure looks transient

Bounded at two retries after the first attempt, backing off 1s then 4s. Two
because the common transient failure is a node dropping out and a second node
answering; not five, because past that the user is waiting on something that is
not coming back and a manual retry is one click.

Classification is deliberately asymmetric, in `failure::is_transient`:

* A phrase must be **recognised as transient** for a retry to happen, so an
  unrecognised failure is not retried.
* A **permanent** phrase anywhere in the text vetoes it, checked first. FFmpeg's
  stderr is a running transcript, not a verdict: a run that ends on a 403 can
  still mention an earlier retried read, and matching the transient list against
  that would retry a request that will never be allowed.

The asymmetry is because the costs are asymmetric. A missed retry costs one
click. A wrong retry spends the user's time and someone else's bandwidth on a
request that has already been refused three times over.

### The retry loop lives below the output reservation, and that is the design

`download_resolved` resolves the output path and then calls `resolve_conflict`,
which is what turns a taken name into `video_2.mp4` (ADR-0004). The retry loop
sits **after** that, wrapped around the FFmpeg call only.

Around it instead — the obvious place, and the one the issue's "job-level retry
in the host" phrasing suggests — the second attempt would re-run
`resolve_conflict`, find the *first attempt's own part-written file* sitting at
the reserved name, and rename away from it. A retried download would quietly
produce a second file and abandon the first: two half-files where the user asked
for one whole one, and no error to say so.

So: one reservation, one path, however many attempts. Attempts after the first
force `overwrite`, which is never the user's `-n` being overridden — the file
being overwritten is the one this job wrote a moment ago, at a name reserved for
it. `tests/native_host.rs` asserts that a retried download leaves exactly
`video.mp4` behind.

Because the loop is there rather than in the host, the **CLI retries too**. That
is deliberate: a `downer <url>` that dies on one dropped connection has the same
defect as an extension download that does. `--retries 0` turns it off.

### A retry is announced, not absorbed

`retrying` is a new **active** wire state carrying `attempt` and
`max_attempts`. A download that silently restarts is indistinguishable from one
that has hung, and the progress bar going back to zero without explanation is
worse than either.

The popup says *"Connection lost. Reconnecting… (attempt 2 of 3)"* — named as
reconnection rather than failure, because nothing has gone wrong that the user
can act on and the job may well finish. Both fields are optional on the wire, so
a host that does not send them renders the sentence without the count rather
than "attempt undefined of undefined".

**Cancellable but not pausable.** Between attempts there is no FFmpeg process,
so Pause could only pretend — and ADR-0012 makes pause a promise. Cancel is
real: the backoff is slept in 100ms slices, each checking for a cancel, so
stopping during a five-second wait stops then rather than at the end of it.

### Timeouts become explicit and configurable

`--timeout` (default 30s) bounds a page or playlist fetch, and the connect
timeout is derived from it, capped at ten seconds — a connection that has not
been established in ten seconds is not going to be, and the rest of the budget
belongs to the transfer. One rule, in `scraper::connect_timeout`, so the flag
and every client agree.

It deliberately does **not** bound the download itself. A large file is not a
hung one, and a deadline there would fail exactly the slow-but-working transfers
this record is trying to protect.

### What was rejected

**`-xerror`.** It makes FFmpeg abort on any error, which would let the
job-level retry catch cases FFmpeg currently absorbs. Rejected: it also aborts
on errors FFmpeg recovers from today, so it would turn working downloads into
failing ones, and the measurement above shows there is no silent data loss for
it to catch. It reports "Invalid data found", which this record's own
classification reads as permanent, so it would not even retry without further
special-casing.

**Retrying on an unrecognised failure.** The default would then be to retry, and
the failure modes that reach a user are mostly the unrecognised ones.

**Segment-level retry.** Out of scope by the issue, and it belongs to the native
scheduler in M5 (KEI-68). FFmpeg fetches segments; we do not see them.

**Extension Settings for any of this.** The host takes the defaults. A user who
needs to tune reconnection is at a terminal, and a Settings field nobody changes
is a worse trade than a good default.

## Consequences

* A download survives a failure it previously could not, without the user
  watching — and says so while it happens.
* The CLI's behaviour changes: a failed download may now take up to two
  automatic restarts before reporting failure, so a script that timed a failure
  will see it later. `--retries 0` restores the old behaviour exactly.
* Argv grows by ten arguments per HTTP input. The argv snapshot tests in
  `tests/ffmpeg_argv.rs` carry them, which is the acceptance criterion asking
  for exactly that.
* `resolve_source` now has both timeouts where it had neither, so a
  pathological host fails in seconds rather than holding a download.
* `retrying` is a state every protocol client must tolerate. It is additive and
  active, so a client that does not know it sees an unfamiliar active state
  rather than a broken terminal one.
* A retry restarts from the beginning. For a large file on a slow link that is
  expensive, and byte-range resume — which would make it cheap — is explicitly
  out of scope here and belongs to M5.

## Unverified

* **That the reconnect options help anything.** Measured as inert on the one
  failure that can be staged on loopback. They are retained on judgement, not
  evidence: see "What was measured".
* **The classification against real FFmpeg output.** The transient and permanent
  phrase lists were built from FFmpeg's source strings and from the failures
  this project has seen, and are unit-tested against transcripts written by
  hand. No corpus of real-world failures exists to check them against, so the
  first thing to revisit when a retry fires wrongly — or fails to fire — is
  those two lists.
* **The backoff numbers.** 1s and 4s are judgement, not measurement. Nothing
  here establishes that a CDN node that failed is back within five seconds.
* **Behaviour on a genuinely flaky network.** Everything above was measured on
  loopback with an injected fault. Packet loss, TLS renegotiation and a real
  CDN's failure modes are not reproduced.
