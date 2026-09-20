# ADR-0017: The extension is Manifest V2 with a persistent background page

* Status: Accepted
* Date: 2026-09-20
* Issue: [KEI-63](https://linear.app/kzhq/issue/KEI-63)
* Open question, deliberately not answered here:
  [KEI-73](https://linear.app/kzhq/issue/KEI-73), which assesses migration to
  Manifest V3 and cross-browser support and carries its own ADR

## Context

`extension/manifest.json` declares `manifest_version: 2`, a `background.scripts`
list with no `persistent: false`, and `browser_action`. The background page is
therefore persistent: it loads when Firefox starts and stays loaded.

This predates the ADR directory and was never chosen on the record — the
extension has been MV2 since it existed. ADR-0013 then leaned on it in passing,
observing that a per-download native port "makes no bet" on background-page
lifetime the way a long-lived one would, and that under MV2 the bet would have
been safe anyway. That is the closest thing to a rationale anywhere, and it is a
footnote in a record about something else.

This record states the decision as it stands today. **It does not decide whether
to migrate**, which is KEI-73's, and it is written so that KEI-73 can supersede
it without having to first work out what it was superseding.

## What the background page holds

`extension/background.js` keeps four things in memory beyond what it persists
(`background.js:47–56`):

* `jobs` — the job records, also written to `storage.local`, debounced.
* `logsByJob` — FFmpeg log lines, also persisted, under a key per job.
* `nativeTasks` — **the live native ports.** Not persistable by nature: a port
  is a connection to a running process.
* `sessionJobs` — which jobs began since this script loaded, which is what lets
  `job-view.js` tell a live download from history an earlier session left
  behind.

Only the last two are the question. `jobs` and `logsByJob` would survive a
suspension because they are already written down; the design that persists them
was driven by log volume rather than by lifetime, but it has that effect.

## Decision

**Manifest V2, background page persistent, for as long as Firefox supports it.**

The load-bearing reason is the one ADR-0013 names in passing. A native
messaging port is a live connection to an OS process, and it belongs to the
background context that opened it. Under MV3 the background is an event page
that the browser may suspend when it judges the extension idle — and an
extension waiting on a native port while FFmpeg downloads a two-hour video is
*exactly* the shape that looks idle. What Firefox actually does in that case,
whether an open port counts as activity, and what happens to a download when it
does not, are unestablished. Under MV2 the question does not arise.

Three lesser reasons point the same way:

* **`sessionJobs` has no persistent equivalent.** Its whole meaning is "this
  script has been running since then", which a suspended script cannot express.
  Reconstructing it would need a new stored notion of a browser session.
* **There is no deadline.** Mozilla supports MV2 in Firefox alongside MV3; the
  Chrome timetable that forces the question elsewhere does not apply here, and
  this extension targets Firefox only. `strict_min_version` is `109.0`.
* **It is not published.** The extension is unsigned and self-distributed
  (`README.md`, "Installing the extension"), so no store review requires MV3.

### Why not migrate now anyway

Because the migration is not a manifest edit. `browser_action` becomes `action`,
the background becomes an event page, and then every piece of in-memory state
above needs a suspension story — which is a redesign of job ownership, tested
against real suspension behaviour, in a browser whose MV3 background model is an
event page rather than Chrome's service worker and so needs its own measurement
rather than a Chrome guide. That is a piece of work with an unknown answer at
the end, which is why KEI-73 exists and why it sits in M5.

Doing it speculatively now would also be done blind: there is no evidence yet
about how Firefox's event page treats a long-held native port, and this decision
would be made by the migration rather than by the measurement.

## Consequences

* The background page is resident whenever Firefox is running. For an extension
  whose background script is 722 lines and idles between downloads, that is
  cheap, and it is what makes a native port safe to hold.
* Job state has two owners in effect: `storage.local` for what must outlive a
  restart, memory for what cannot be written down. `background.js` reconciles
  the difference on load, marking jobs that were active when the browser closed
  as `interrupted` — an extension-only terminal state (`docs/protocol.md`,
  "Per-job state machine"), and the clearest evidence that native ports do not
  survive a restart even with a persistent page.
* The extension cannot run in Chrome today, and this record is not the reason —
  native messaging host registration, `browser.*` promises and MV2 each
  independently prevent it. Cross-browser support is KEI-73's question too.
* Anything added to background memory between now and KEI-73 makes that
  migration larger. New state that *can* be persisted should be, so the eventual
  answer is about ports rather than about bookkeeping.

## Unverified

* **Firefox's MV3 event-page suspension behaviour with an open native port has
  not been tested here.** That is the central factual claim behind preferring
  MV2, and it is stated as an unknown rather than as a finding — the risk is
  that the behaviour is unestablished, not that it is known to be bad.
* **Mozilla's MV2 support is stated from its published position**, not from any
  commitment this project holds. If that changes, this record's "no deadline"
  reason goes with it and KEI-73 becomes urgent rather than scheduled.
* **The cost of a persistent background page was not measured.** It is asserted
  to be negligible from the size and idleness of the script, not from a memory
  profile of a running Firefox.
