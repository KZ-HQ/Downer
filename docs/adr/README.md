# Architecture decision records

One record per decision, numbered in the order they were made and never
renumbered. A record is not edited once accepted, except to add a status line
pointing at the record that changed it — so the reasoning that was current at
the time stays readable, including where it was later found wrong.

Start from [`template.md`](template.md). Take the next free number, and say in
the title what was decided rather than what the record is about.

`AGENTS.md` names the changes that require one: the native messaging protocol,
the host process model, the FFmpeg command layer, discovery ownership, and
control semantics.

| # | Decision | Status |
| --- | --- | --- |
| [0001](0001-native-messaging-protocol.md) | Version the native messaging protocol; separate request rejection from job failure | Accepted, amended by 0004 |
| [0002](0002-cookie-scoping-and-argv-exposure.md) | Accept cookie exposure in FFmpeg's argv; scope cookies at the host | Accepted |
| [0003](0003-redact-urls-in-logs.md) | Redact URL queries in both the host and the extension | Accepted |
| [0004](0004-output-naming-and-collision-policy.md) | Rename rather than refuse on an output collision | Accepted; naming half superseded by 0005 |
| [0005](0005-default-output-name-over-derived-one.md) | Default to `video.<ext>`; title naming opt-in | Accepted |
| [0006](0006-ffmpeg-version-detection.md) | Detect the FFmpeg version; degrade rather than fail on an old one | Accepted |
| [0007](0007-structured-ffmpeg-command-model.md) | Express an FFmpeg run as a structured invocation, rendered in one place | Accepted |
| [0008](0008-relocatable-native-host-installation.md) | Install the native host from the binary, not from the checkout | Accepted |
| [0009](0009-setup-diagnostics.md) | Setup checks as a `status` command, shared with `downer doctor` | Accepted |
| [0010](0010-resolve-hls-master-playlists.md) | Resolve a master playlist to one rendition before FFmpeg sees it | Accepted, partly superseded by 0014 |
| [0011](0011-one-playlist-parser.md) | The extension fetches playlists; Rust parses them | Accepted |
| [0012](0012-control-semantics.md) | What pause, resume and cancel promise | Accepted |
| [0013](0013-one-download-per-host-process.md) | One download per host process | Accepted |
| [0014](0014-pair-a-rendition-with-its-audio.md) | Pair a chosen rendition with its separately declared audio | Accepted; amends 0010 |
| [0015](0015-ffmpeg-as-the-only-engine.md) | FFmpeg is the only engine, always `-c copy`, never bundled | Accepted |
| [0016](0016-blocking-io-and-os-threads.md) | Blocking I/O on OS threads; no async runtime in our own code | Accepted |
| [0017](0017-manifest-v2-persistent-background-page.md) | Manifest V2 with a persistent background page | Accepted |
| [0018](0018-stable-cli-exit-codes.md) | Exit codes classify the failure and are part of the CLI contract | Accepted |
| [0019](0019-native-host-as-a-mode-of-the-cli.md) | The native host is a mode of the CLI binary | Accepted |
| [0020](0020-json-output-is-a-cli-interface.md) | `--json` output is a public interface, versioned in the document | Accepted |
| [0021](0021-windows-is-unsupported.md) | Windows is unsupported, and the crate refuses to build there | Accepted |
| [0022](0022-a-bounded-redacted-host-log-file.md) | The native host keeps one bounded, redacted log file, and `status` names it | Accepted |
| [0023](0023-surviving-a-transient-failure.md) | Survive a transient failure by retrying the whole download, bounded and announced | Accepted |

## Reading them together

Five threads run through the set, and a record is usually easier to understand
beside the others on its thread.

**The browser boundary.** 0001 (the wire contract) → 0019 (which program speaks
it) → 0008 (how that program gets registered) → 0009 (how a user finds out it
did not) → 0022 (where it writes down what happened, since Firefox keeps none of
it).

**Cookies and secrets.** 0002 (where cookies are scoped, and what FFmpeg's argv
exposes) → 0003 (the leak that was left, in FFmpeg's own stderr).

**HLS.** 0015 (FFmpeg does the fetching, leniently) → 0010 (resolve a master
before FFmpeg sees it) → 0011 (who fetches a playlist, who parses it) → 0014
(pair the chosen video with its audio).

**Downloads as processes.** 0015 (there is a process at all) → 0016 (we block
on it rather than await it) → 0013 (one download per host process) → 0012 (what
we can promise by signalling it) → 0021 (which platforms can keep that promise,
and why the ones that cannot are refused at compile time) → 0023 (what happens
when the process dies for a reason nobody chose).

**What a script may rely on.** 0018 (the exit codes classify the failure) →
0020 (`--json` describes the success), which makes the same promise about the
other half of what a caller reads.

Records 0015–0019 were written after the fact, under KEI-63, for decisions that
had been embodied in the code since before this directory existed. They
reconstruct the reasoning from the code and say so; where a claim is an
inference rather than a measurement, their "Unverified" sections name it.
