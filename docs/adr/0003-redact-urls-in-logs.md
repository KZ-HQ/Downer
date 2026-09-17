# ADR-0003: Redact URL queries in both the native host and the extension

* Status: Accepted
* Date: 2026-09-17
* Issue: [KEI-55](https://linear.app/kzhq/issue/KEI-55)
* Follows: [ADR-0002](0002-cookie-scoping-and-argv-exposure.md), whose
  Consequences section names this as the remaining gap

## Context

ADR-0002 closed the paths by which *Downer* could disclose a session cookie: the
CLI no longer needs shell history, headers cannot be injected, and no host event
carries the value — `tests/native_host.rs::no_host_event_carries_the_cookie_value`
greps a sentinel through every event the host emits.

One path was left open, and it does not run through anything we control. FFmpeg
writes its own diagnostics to stderr, and at `-loglevel info` its HLS demuxer
logs one line per segment:

```text
[hls @ 0x7f8] Opening 'https://cdn.example.test/hls/seg42.ts?token=…&e=…' for reading
```

Signed segment URLs are how most commercial media is protected — the very case
this product exists for. `src/native.rs` forwarded each such line to the
extension as a `log` event, and `extension/background.js` appended it to the
job's `logs` array in `storage.local`, where it stayed until the user pressed
"Clear logs". A token that FFmpeg merely *mentioned* therefore outlived the
download that used it, in a place nothing was watching.

The same lines were also what made persistence expensive. `appendJobLog` went
through `updateJob`, which broadcast the whole job record and scheduled a write
of *all* jobs — up to 20 jobs × 500 lines under one storage key. A
2,000-segment stream cost thousands of full-state writes and as many popup
re-renders. Secrecy and cost had the same cause, so KEI-55 fixes them together;
this ADR records the architectural half.

## Decision

### Redact in both places, with the extension as the gate

The choice was where redaction belongs: in `src/native.rs`, before a log event is
emitted, or in `extension/background.js`, before anything is persisted or
displayed. It is **both**, because neither covers the other's ground.

Host-side alone is insufficient. The extension produces URL-bearing error text
the host never sees: `background.js::hlsSegmentInfo` fetches playlists itself and
stores the failure as `metadataError`, and that URL is the token-bearing
`.m3u8`. It also says nothing about lines already persisted by an older build.
The acceptance criterion "popup error text contains no query strings" cannot be
met from the host.

Extension-side alone is insufficient in the other direction. The token would
still cross the native messaging port, sit in every `runtime.sendMessage`
broadcast, and be present in whatever the host itself writes — KEI-64 adds a
host log file in M4, and would have to solve the same problem again from nothing.
Redacting at the source is what makes that future file safe by default.

So: `src/redact.rs` runs first, on every log line and on the terminal error
(`DownerError::FfmpegFailed` embeds FFmpeg's stderr tail, which is the same leak
by another route). `extension/redact.js` runs again on every line, and on the
`error`, `controlError` and `metadataError` fields of every job record, before
any of it is written or broadcast.

The cost of two layers is that the rule is written twice and could drift.
`tests/fixtures/redaction.json` is the answer, and it is the same answer
ADR-0001 gave for the wire vocabulary: one case table, read by
`src/redact.rs`'s unit tests and by `tests/extension/redact.test.js`, so a case
that passes on one side and fails on the other is a test failure rather than a
silent divergence.

### What redaction keeps

Scheme, host, port and path survive; the query becomes `?…` and a fragment
becomes `#…`. The marker matters: a reader can tell "this URL had parameters"
from "this URL had none", which is often the difference between a signed and an
unsigned CDN path. `user:password@` in an authority becomes `…@`.

A log that named no URL at all would be useless for the thing logs are for —
telling a 404 from a 403 from a DNS failure — so removing the URL entirely was
not considered.

**Only `http` and `https` are matched**, because they are the only schemes
`output.rs` accepts and so the only ones this product can be downloading. A
`key=value` pair elsewhere in a line is not treated as a URL; FFmpeg's own
progress vocabulary (`q=-1.0`, `size=…`) is full of them.

**A token in a path segment is not redacted.** Some CDNs sign that way
(`/hls/<token>/seg.ts`), and nothing distinguishes such a segment from an
ordinary one without knowing the site. Redacting paths heuristically would
destroy the logs' usefulness for a case we cannot detect reliably. This is a
known residual, not an oversight.

### The job's own `url` is stored whole

`job.url` is deliberately *not* redacted in storage. `extension/job-view.js`
matches persisted jobs to the media found on the page being viewed by exact URL,
and re-downloading needs the real thing; a redacted `url` would break both.
Instead `extension/options.js` redacts it at the point of display, where it
heads each log line and fills the per-download filter.

This is the one place where a query string still reaches `storage.local`. It is
the URL the user chose to download, already visible in the page and in the popup,
rather than something FFmpeg happened to mention — a different thing from a log
line, and not what the issue is about.

### The wire protocol does not change

No request or response field is added, removed or renamed, so `protocol_version`
stays at 1 and `tests/fixtures/protocol.json` is untouched. What changes is the
*content* of the existing `log` field and of a terminal `error`, which
`docs/protocol.md` now documents as redacted. An extension talking to an older
host still works; it simply performs the only redaction pass itself, which is why
the extension is the gate and not merely a second opinion.

### Batching, and the storage split

Logs move out of `downloadJobs` into one `downloadLogs:<jobId>` key per job, so
writing job state no longer rewrites every log line of every job. Storage writes
and log broadcasts are coalesced in a 300 ms window, with terminal states
bypassing it so a finished job is durable at once.

A `log` event carries `state: "downloading"` and nothing else, so
`handleNativeEvent` now returns after appending the line instead of falling
through to `updateJob`. Without that, every line still broadcast a job-state
change and re-rendered the popup — the batching was bounded in storage writes and
unbounded in renders, which the test
`a 2,000-line stream costs a bounded number of popup broadcasts` caught.

Records written before this change carry their logs inline. They are migrated on
load: the lines are redacted, moved to the split key, and the job record written
back without them, in the same one-shot pass `reconcileRestoredJobs` already
uses. Migrating rather than dropping is deliberate — a line persisted before
redaction existed is exactly the leak this ADR is about, and leaving it until
"Clear logs" is what the old behaviour did.

## Consequences

* A signed token that FFmpeg logs no longer leaves the host process, and no
  longer reaches `storage.local`, the popup, or the Settings page. Tokens
  persisted by an older build are scrubbed the next time the extension starts.
* Persistence during a long HLS download is bounded: a 2,000-line stream costs a
  single-digit number of `storage.local.set` calls and of popup messages, asserted
  in `tests/extension/background-logs.test.js`.
* Two implementations of one rule now exist. They are pinned to one case table,
  and adding a case to `tests/fixtures/redaction.json` fails both suites until
  both sides implement it. That is the intended maintenance cost.
* `extension/options.js` no longer reads `job.logs`; it assembles the view from a
  `get-download-logs` reply plus `download-log` batches. Those are internal
  runtime messages between extension pages, not the native messaging protocol.
* A token in a *path* segment still persists. If a real site is found that signs
  that way, it needs its own issue and probably its own rule.
* **Nothing here has been run against a real FFmpeg.** FFmpeg is deliberately not
  installed in CI and is unavailable in the environment this was implemented in
  (AGENTS.md); both suites use fake FFmpeg scripts, and
  `tests/native_host.rs::no_host_event_carries_a_url_query` feeds the host a
  stderr line that *resembles* FFmpeg's, rather than one FFmpeg produced. The
  end-to-end check is manual: `tests/fixtures/protected_site.py` now serves
  `/media/signed.m3u8`, whose segment URLs carry a token, so a download through a
  real browser and a real FFmpeg can be inspected in Settings. Until someone runs
  that, the exact shape of FFmpeg's real log lines is an assumption.
