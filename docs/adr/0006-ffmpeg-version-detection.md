# ADR-0006: Detect the FFmpeg version, and degrade rather than fail on an old one

* Status: Accepted
* Date: 2026-09-19
* Issue: [KEI-81](https://linear.app/kzhq/issue/KEI-81)
* Follows: [ADR-0015](0015-ffmpeg-as-the-only-engine.md), which records why
  those two options are passed in the first place. This record decides only what
  happens when the FFmpeg found is too old to accept them.

## Context

`src/ffmpeg.rs` appends `-allowed_segment_extensions ALL` and
`-extension_picky 0` to every `.m3u8` input. Both options exist only from FFmpeg
7.1, which `README.md` documents as the minimum, so passing them is consistent
with the stated contract.

The failure mode was not. On an older FFmpeg the run died during argument
parsing, before a single HTTP request:

```text
Unrecognized option 'allowed_segment_extensions'.
Error splitting the argument list: Option not found
```

The user saw `FFmpeg failed with status 8: Unrecognized option …` for every HLS
download while direct-file downloads kept working, because those carry no
version-specific options. Nothing said "your FFmpeg is too old", and nothing
checked. FFmpeg 6.1.1 is not a hypothetical: it is what `apt install ffmpeg`
gives on Ubuntu 24.04 LTS.

## What was measured

KEI-81 asked whether the two options could simply be omitted on an older FFmpeg
rather than being fatal, and said to measure rather than assume. Measured on
2026-09-19 against a local HLS server, with an `.m3u8` whose segment is named
`seg0.jpeg` — the odd extension these options exist for — and with the repo's
own `tests/fixtures/hls/` shape:

| FFmpeg | with both options | without them |
| --- | --- | --- |
| 6.1.1 (`6.1.1-3ubuntu5`, Ubuntu 24.04) | **fails**: `Unrecognized option` | **downloads** |
| 9.0.1 (conda-forge, the e2e FFmpeg) | downloads | **fails**: `URL … is not in allowed_segment_extensions` |

The result is stronger than "it might work without them". The strict
segment-extension checking those options switch *off* was **introduced in 7.1**
along with the options themselves. FFmpeg 6.x is already permissive, so on 6.x
the options are not merely tolerable to omit — they are a no-op. Omitting them
costs nothing at all.

## Decision

### The version is detected once, where FFmpeg is discovered

`ffmpeg::version(path)` runs `ffmpeg -version`, parses the first line, and
memoizes the answer per executable per process. Detection is therefore "once"
in the sense that matters, while the two discovery paths — the CLI's `--ffmpeg`
and the native host's `ffmpeg_path()` — stay untouched and free to change
independently.

Parsing takes the numeric prefix of the token after `version`, so
`6.1.1-3ubuntu5` reads as `6.1.1` and `n4.4.1` as `4.4.1`. Only major and minor
take part in the comparison; the patch is kept so the diagnostic can name the
build the user actually has.

**An undetectable version is never treated as too old.** A snapshot build
(`N-109755-g1b9f9c1a3d`) names no release, and those builds are newer than 7.1,
not older. A failed probe is no reason to change how a download is built.

### An old FFmpeg omits the options rather than failing

Given the measurement, refusing to run would break downloads that otherwise
succeed — including direct-file downloads, which never had a problem. So below
the minimum the two options are dropped
(`FfmpegCommand::without_segment_extension_options`) and the download proceeds.
On 6.x this is not degradation in any observable sense; it is the same
behaviour with the no-op arguments removed.

This is deliberately **not** a lowering of the supported minimum, which KEI-81
puts out of scope. 7.1 remains what `README.md` documents and what the project
tests against. An old FFmpeg is unsupported, warned about, and allowed to try.

### It says so, on both surfaces, whether or not it works

One wording, `outdated_ffmpeg_warning`, so the three places cannot drift:

* **Always, before the download.** The CLI prints it to stderr; the native host
  emits it as a `log` event, the channel the Settings console already shows and
  persists. A download that succeeds on an old FFmpeg is therefore not silent.
* **On failure, as the error.** A failure under an old FFmpeg becomes
  `DownerError::FfmpegTooOld`, which reads *FFmpeg 6.1.1 is older than the
  minimum supported 7.1* and keeps FFmpeg's own stderr after it, because an old
  FFmpeg can fail for ordinary reasons and the detail is still the useful part.
  That error is the extension's job error and the CLI's `error:` line.

`FfmpegTooOld` exits **4** (unavailable FFmpeg), not 5 (media failure): an
FFmpeg too old to use is a broken installation, not a broken stream.

## Consequences

* HLS downloads now work on FFmpeg 6.x where every one of them used to fail.
  That is a behaviour change on an explicitly unsupported version, and it is the
  point of the issue.
* Every download spawns one extra short-lived `ffmpeg -version`, memoized per
  process. The CLI and the native host are both one process per download, so
  this is one extra spawn per download — negligible beside the download itself,
  and the price of a diagnostic that cannot be derived any other way.
* Fake FFmpegs in the test suite must answer `-version`; they now do, reporting
  a supported version by default. A fake that did not answer would be asked to
  "download" to a file named `-version`.
* `tests/log_redaction.rs` and the real-FFmpeg tests in `tests/native_host.rs`
  worked around the old failure by avoiding the HLS path or invoking FFmpeg
  directly. Their comments are updated; the workarounds still stand on their own
  merits and were not removed.

## Unverified

* **Only 6.1.1 and 9.0.1 were measured.** 7.0 is asserted to behave like 6.x and
  4.x/5.x are assumed likewise, from the FFmpeg 7.1 changelog rather than from a
  run. The behaviour on a 5.x FFmpeg is not known first-hand.
* Whether *other* 7.1-only behaviour this project relies on breaks on 6.x is not
  established. The warning says downloads may fail for exactly this reason: what
  was measured is the segment-extension options, not the whole of FFmpeg.
* The version is not yet surfaced in the Settings page next to the FFmpeg path,
  which KEI-81 raised as a "consider". It would need the version on the wire —
  a `hello` field, and so a protocol change and an ADR of its own. Deferred as a
  follow-up rather than folded in here.
