# ADR-0007: Express an FFmpeg run as a structured invocation, rendered in one place

* Status: Accepted
* Date: 2026-09-19
* Issue: [KEI-52](https://linear.app/kzhq/issue/KEI-52)

## Context

`FfmpegCommand` held a `Vec<OsString>` that callers built through three
constructors and that one executor then edited in place. To add progress
reporting, `execute_controlled_with_progress_and_logs` removed `-stats` and
spliced seven arguments at index `3`:

```rust
let mut args = command.args.clone();
args.retain(|argument| argument != "-stats");
args.splice(3..3, [ /* -loglevel info -nostats -stats_period 0.5 -progress pipe:1 */ ]);
```

Index `3` was correct only because the builder happened to emit
`-hide_banner -loglevel error -stats` first. Nothing said so, and nothing would
have failed loudly if the builder had changed: the splice would have landed
mid-option and FFmpeg would have been handed a subtly different command.

Three further facts were recovered from argv rather than known:

* the output path, read back as the last element (`command_output_path`), with
  `PathBuf::from("output")` as the fallback for an empty argv that cannot occur;
* whether the input was HLS, decided by a `.m3u8` substring check *inside* the
  builder, so the caller could not say otherwise;
* whether this FFmpeg could accept the leniency options at all, which ADR-0006
  had to express by building the command and then filtering options back out
  (`without_segment_extension_options`).

The public API had accreted to match: `download_resolved`,
`download_resolved_controlled`, `download_resolved_controlled_with_progress`,
and `download_resolved_controlled_with_progress_and_logs`, each a wrapper
choosing a different executor closure.

## Decision

**An invocation is data; argv is a rendering of it.**

`FfmpegInvocation` names what a download *is* — program, input, headers,
cookies, `hls_lenient`, threads, overwrite, output, and a `Reporting` mode —
and `to_args()` is the only code that decides argument order. The executors
render and spawn; they no longer edit.

`Reporting` is an enum rather than a set of flags, because the difference
between a terminal run and a host-driven one is a mode, not an argument list:

* `Reporting::Cli` → `-loglevel error -stats`
* `Reporting::Controlled { stats_period }` → `-loglevel info -nostats
  -stats_period <p> -progress pipe:1`

**The leniency decision moves to the caller.** `hls_lenient` is a field, so
`src/lib.rs` decides it from the input *and* from the FFmpeg version
(`is_hls(url) && outdated.is_none()`). ADR-0006's behaviour is unchanged, but it
is now expressed by not rendering the options rather than by rendering and then
removing them; `without_segment_extension_options` is gone.

**One download entry point.** `download_resolved(media, options, hooks)` is what
the CLI, the native host and the tests all call. `Hooks` carries an optional
`ProcessControl` and optional progress and log callbacks; `Hooks::default()` is
the CLI's case and `Hooks::controlled(...)` the host's. The presence of a
control selects both the executor and the `Reporting` mode in one place, so the
two cannot disagree — previously a caller could pick a controlled executor and
still have argv rendered for a terminal.

## What the rendered argv changed

Five of the six cases in `tests/ffmpeg_argv.rs` render byte-for-byte what the
previous code produced. Controlled mode differs, by a removal:

| | argv |
| --- | --- |
| before | `-hide_banner -loglevel error -loglevel info -nostats …` |
| after | `-hide_banner -loglevel info -nostats …` |

The old splice left the builder's `-loglevel error` in place and inserted
`-loglevel info` after it, relying on FFmpeg's last-one-wins to resolve the
pair. `info` applied then and applies now, so behaviour is unchanged; what is
gone is a dead argument that only existed because argv was edited rather than
rendered. Carrying it forward would have meant teaching the model to emit a
log level it does not mean, which is the defect this ADR removes.

This is the one deviation from KEI-52's acceptance criterion that the snapshots
"match the pre-refactor argv". It is recorded here, and in the test, rather than
resolved silently.

## Consequences

* Argument order is stated once. A change to it shows up as a failing
  comparison in `tests/ffmpeg_argv.rs`, which drives the real download path
  against a recording fake, so what is pinned is what a process received.
* `ProcessControl` stays independent of the invocation, so a second engine can
  use it unchanged.
* `FfmpegInvocation` is the seam KEI-68 needs: an engine field can join the
  struct without a caller learning where an option lands. The engine choice is
  already explicit rather than inferred from a URL substring — the substring
  check survives as `is_hls`, but it informs a field instead of being
  rediscovered inside the builder.
* `Hooks` boxes its callbacks, so a download with hooks allocates twice more
  than before. This is per download, not per progress event.

## Unverified

* Only the six cases KEI-52 names are pinned. Combinations beyond them —
  controlled mode *with* HLS leniency, or cookies together with threads — are
  covered by the rendering unit tests in `src/ffmpeg.rs`, not by a recorded
  process.
* `stats_period` is now a field, but nothing yet sets it to anything other than
  `DEFAULT_STATS_PERIOD` (0.5). Whether another period is useful is untested.
