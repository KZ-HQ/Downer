# ADR-0022: The native host keeps one bounded, redacted log file, and `status` names it

* Status: Accepted
* Date: 2026-09-21
* Issue: [KEI-64](https://linear.app/kzhq/issue/KEI-64)
* Follows: [ADR-0009](0009-setup-diagnostics.md), which made `status` the place a
  user finds out what is wrong; [ADR-0003](0003-redact-urls-in-logs.md), whose
  redaction rule this reuses; [ADR-0007](0007-structured-ffmpeg-command-model.md),
  which is what makes a safe rendering of an FFmpeg command possible at all

## Context

The native host had nowhere to write. `src/ffmpeg.rs` echoed FFmpeg's stderr
with `eprint!`, and the host's own errors went to stderr too — and when Firefox
launches a native messaging host, its stderr goes to the Browser Console, which
almost nobody opens and which keeps nothing after the process exits.

That is worst precisely where it matters most. A host that fails at start-up —
a binary that is not executable, a manifest pointing somewhere stale, an FFmpeg
that is not there — fails before it can send a single protocol event, so the
extension sees a port that closed and the user sees "native host disconnected".
[ADR-0009](0009-setup-diagnostics.md) added `status` to answer *"is this
installation able to download anything?"*, which covers the state of the
installation but not the history of what it did. Nothing survived a run.

The extension does persist FFmpeg log lines per job (KEI-55, 500 lines each),
but only for downloads that got far enough to produce events, and only inside
`storage.local` where a user cannot attach them to a bug report.

Doing nothing was not an option for the first release (KEI-93): the artifacts go
somewhere people can install them, and the first question about any failure is
"what does the log say?".

## What was measured

Nothing was measured; this is a design decision, not a performance one. Two
facts were checked rather than assumed, both on Linux on 2026-09-21:

* A download driven through the real host binary with a page title set writes
  `A Very Private Page Title.mp4` into the downloads directory and puts that
  title nowhere in the log file. The title test would otherwise have been
  vacuous — it would pass if title naming had silently not applied.
* The rendered spawn line for a cookie-bearing HLS download is
  `… -headers <User-Agent> -cookies <1 cookie> -i https://cdn.example.test/master.m3u8?… -c copy -y <output>`,
  with `output_dir` carried as its own field.

## Decision

### The host, and only the host, writes a log file

`downer --native-host` initialises the logger; the CLI never does. A terminal
already shows what Firefox swallows, and a `downer <url>` run that silently
began writing files under the user's home would be a surprise with no upside.
`downer doctor` still *reports* the path, because the path is what a user needs
in order to find the file the host wrote.

The file is `~/Library/Logs/downer/host.log` on macOS, which is where macOS
users already look, and `$XDG_STATE_HOME/downer/logs/host.log` elsewhere. The
issue proposed the data directory for the second; state is the better fit, and
the XDG specification agrees — a log survives restarts, is not precious, and
losing it costs the user nothing, which is the definition of state rather than
data.

### It is bounded by construction, not by convention

Two files of 1 MiB: `host.log` and `host.log.1`, rotated by size, one
generation kept. The total on disk is therefore 2 MiB and cannot grow.

The cap is a field on `HostLog`, not a constant, so the rotation test proves the
behaviour with 200 bytes rather than by writing a megabyte to watch it happen. A
bound nobody tests is a bound nobody has.

Rotation happens *before* a line that would cross the cap, so the cap is never
exceeded rather than merely noticed afterwards. A single line larger than the
whole budget is still written, into an empty file: dropping it would lose the
one event most worth having.

### A secret cannot reach it, because a secret is never passed to it

This is the part that decides the design, and the reasoning is worth stating
because the obvious implementation is wrong.

[ADR-0003](0003-redact-urls-in-logs.md)'s redaction covers **URLs** — query,
fragment, userinfo. Every value written here passes through it, so a signed
segment URL loses its token. But the two other secrets in this system are not
URLs and no amount of filtering would reliably catch them:

* **A cookie** reaches FFmpeg as the value of `-cookies`. It has no URL syntax.
  Pattern-matching for cookie-shaped text would be a second redaction rule, and
  `AGENTS.md` has exactly one, implemented once per language and pinned by
  `tests/fixtures/redaction.json`. A second one would be a second thing to get
  wrong.
* **A page title** is the output *filename* when title naming is on, and
  `AGENTS.md` is explicit: a title may reach the output path "and never a log,
  an error, or any event but the output path".

So neither is filtered on the way in. Neither is ever handed over.
[`FfmpegInvocation::to_log_args`](../../src/ffmpeg.rs) renders the command line
with three substitutions, and the module that writes the file never sees the
originals:

| Argument | Logged as | Why that and not nothing |
| --- | --- | --- |
| `-cookies <value>` | `<1 cookie>` | "Were cookies forwarded?" is the first question in every protected-media report. How many is enough to answer it; which ones is never anyone's business. |
| `-headers <block>` | `<User-Agent,Referer>` | Which headers were sent explains a 403. The values add nothing a log may hold — and naming them survives a future header that *is* secret. |
| the output path | `<output>` | It is the title. The directory is logged as its own field, which answers "where was it writing?" without the name. |

That rendering is **derived from `to_args`**, not rebuilt beside it. ADR-0007
made argument order the property of one function; a second renderer would drift
from the first, and the log would then describe a command that never ran.

### `status` gains `log_path`, so the protocol changes

The path is reported by `status` and by `downer doctor`, and rendered in the
Settings "Check setup" panel. It is reported whether or not the file exists yet:
"nothing has been logged" is itself an answer, and a user needs the path to
check.

This is a protocol change, which is why this record exists. The field is
optional on the wire, so an older host that does not send it renders no row
rather than an empty one.

### The level is configured in the file, not the environment

`log_level` in `~/.config/downer/config.json`, defaulting to `info`, with
`DOWNER_LOG` overriding it. The environment variable alone would not do:
Firefox launches the host with a minimal environment, which is the same reason
`ffmpeg` is recorded in that file (ADR-0008). FFmpeg's own stderr is `debug`,
because a long HLS download writes one line per segment and the default level
must not fill the file with them.

An unrecognised level falls back to the default rather than to `Off`. The
failure mode of a typo must not be silence.

### What was rejected

**`tracing` plus `tracing-appender`**, which the issue allowed. It is four
crates and a subscriber registry to write one file that one process appends to
from three threads. The whole writer is under 200 lines, and the project's rule
is a short dependency list.

**Logging raw argv.** The straightforward reading of the issue's scope line, and
it would have put a page title and a cookie in a file on disk.

**Filtering secrets out of the text on the way in.** Rejected above: it needs a
second redaction rule for a class of data that has no syntax to match, and it
fails open — anything it does not recognise is written.

**A log per job.** The extension already has that (KEI-55). This file's purpose
is the failures that happen when there is no job yet.

**Failing a download when the log cannot be written.** Every error here is
swallowed. A full disk or a read-only home must not turn a working download into
a failed one; the log exists to explain failures, not to cause them.

## Consequences

* A user can be asked for one file, and it will contain the start-up failures
  that previously left no trace at all.
* `status` responses are slightly larger, and `docs/protocol.md` gains a field.
* The host writes to the filesystem during an ordinary run, which it did not
  before. Bounded at 2 MiB, created lazily on the first line, and absent
  entirely at `log_level: "off"`.
* `tests/fixtures/protocol.json` now pins check names on both sides: the Rust
  suite asserts the host emits exactly `check_names` in order, and the
  extension's suite asserts its own synthesized names come from
  `extension_check_names` and never collide. This closed a gap that had already
  gone wrong once — ADR-0021 added a `platform` check without updating the
  fixture, and nothing failed, because nothing read it.
* `downer install-host --ffmpeg` now merges into the existing config file rather
  than overwriting it. Before this record the file held one key, so writing it
  whole was harmless; now it can hold a user's `log_level`, and reinstalling to
  record a new FFmpeg must not discard it.
* Adding a fourth secret-bearing FFmpeg option in future means adding it to
  `to_log_args`. The test that pins the substitution count is what will catch
  the omission.

## Unverified

* **Behaviour when the disk fills mid-write.** Writes are best-effort and errors
  are dropped, which is the intended behaviour, but it has not been exercised
  against a genuinely full filesystem — only against a path that could not be
  created.
* **The macOS location.** `~/Library/Logs/downer/host.log` is asserted by unit
  test against a synthetic home directory, from Linux. No macOS machine ran it;
  CI's `macos-latest` job runs the same assertions but has never been inspected
  for where the file actually landed.
* **That 1 MiB is the right cap.** Chosen because it comfortably holds a long
  HLS download's stderr and still attaches to a bug report. Not measured against
  a corpus of real failures, because there is not one yet.
* **Concurrency.** The writer is behind a `Mutex` and the host runs one download
  per process (ADR-0013), so contention is three threads at most. Not load
  tested; there is no load to test it with.
