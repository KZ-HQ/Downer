# ADR-0014: Pair a chosen rendition with its separately declared audio

* Status: Accepted
* Date: 2026-09-20
* Issue: [KEI-61](https://linear.app/kzhq/issue/KEI-61)
* Amends: [ADR-0010](0010-resolve-hls-master-playlists.md)

## Context

[ADR-0010](0010-resolve-hls-master-playlists.md) resolves an HLS master
playlist to one variant before FFmpeg sees it, and **declines to do so** when
the master declares a rendition separately — `#EXT-X-MEDIA` with a `URI`.
Measured then: the master yields video and audio, the video variant alone
yields video only. `select_variant` returned one URL and could not express
"this video plus that audio", so declining was the safe answer.

KEI-61 adds a rendition picker. A picker that declines for those masters would
offer a choice mainly where it is least needed, because separated audio is the
ordinary shape of modern HLS packaging — the illusion of choice the issue
exists to remove, relocated rather than fixed.

ADR-0010 also left two things explicitly unverified: whether the guard could be
narrowed to audio alone, and how the pieces behave beyond the two renditions it
measured. Both are settled below.

## What was measured

FFmpeg 9.0.2 (conda-forge, `scripts/install_test_ffmpeg.sh`) against a
request-logging HTTP server serving a generated HLS tree: two video-only
renditions (320x180 at 200 kbps, 1280x720 at 900 kbps, four segments each) plus
a separate audio rendition. The master lists the **low** variant first,
reproducing what KEI-89 found in the field. Each case runs the argument shape
`src/ffmpeg.rs` emits — `-allowed_segment_extensions ALL -extension_picky 0`,
`-c copy`, one output.

### Pairing works, and costs less than not choosing

| Input | Requests | Segments (low / high / audio) | Streams written |
| --- | --- | --- | --- |
| the master — ADR-0010's behaviour here | 15 | 11 (2 / 4 / 5) | video 1280x720 **+ audio** |
| the high variant alone | 5 | 4 (0 / 4 / 0) | **video only** |
| high variant **+** audio, `-map 0:v:0 -map 1:a:0` | 11 | 9 (0 / 4 / 5) | video 1280x720 **+ audio** |
| **low** variant + audio, same mapping | 11 | 9 (4 / 0 / 5) | video **320x180** + audio |

Row two reproduces ADR-0010: the guard was right about the defect. Rows three
and four are the answer to it. The last row is the one a picker needs, and no
route reached it before: the master gives the highest rendition, the variant
alone gives no sound.

**`-map` is safe here in a way KEI-89 found it was not.** Input 0 is a *media*
playlist with one video stream, so `0:v:0` is unambiguous. The same option
against a *master* selects the first variant in publisher order, which in the
master KEI-89 measured was the lowest.

### The guard was over-broad

A master whose only separately declared rendition is **subtitles**, audio muxed
into the variants:

| Input | Requests | Segments | Streams written |
| --- | --- | --- | --- |
| the master — the guard declines | 12 | 8 (both renditions) | video 1280x720 + audio |
| the high variant alone | 5 | 4 | video 1280x720 + audio — **identical** |

Declining costs twice the segments and buys nothing: FFmpeg writes no subtitle
stream into the MP4 from the master either. Passing the subtitle rendition as a
third input does not work at all:

```text
[mp4] Could not find tag for codec webvtt in stream #2, codec not currently supported in container
[out#0/mp4] Could not write header (incorrect codec parameters ?): Invalid argument
```

exit 234, no output file.

### A master with two audio languages already drops one

| Input | Requests | Segments | Streams written |
| --- | --- | --- | --- |
| the master (English `DEFAULT=YES`, plus French) | 19 | 13 — both languages *and* both renditions probed | video + **one** audio |
| high variant + English | 11 | 9 | video + audio |
| high variant + French | 11 | 9 | video + audio |

Naming the audio rendition is the only way to get the other one, and it costs
fewer requests than not naming it.

## Decision

**A chosen rendition is paired with its audio, and both are given to FFmpeg.**
`scraper::MasterPlaylist::choose` answers a `VariantChoice` — a video URL and,
when the master carries the audio outside the variant, an audio URL.
`FfmpegInvocation` gains `audio_input: Option<String>`; when it is present the
invocation carries two `-i` values and `-map 0:v:0 -map 1:a:0`.

**One optional audio input, not a general input list.** Nothing else in this
project has a second input, and the shape should say what it is for. A general
list would invite mappings nobody has measured.

**Every input option is repeated before every `-i`.** FFmpeg applies
`-headers`, `-cookies` and the segment-extension options to the *next* input
only. Emitting them once would fetch the audio playlist without the session the
video playlist needed, which on a cookie-gated CDN is a download that half
works. Pinned by `hls_variant::each_input_carries_the_session_options`.

**ADR-0010's guard narrows from "any `#EXT-X-MEDIA` with a `URI`" to "an
*audio* `#EXT-X-MEDIA` with a `URI`".** Subtitles are neither usable as an
input nor a reason to decline, as measured above, so a subtitles-only master
now resolves like any other and downloads half the data for the same file.
Subtitles are not modelled at all: `scraper::AudioRendition` exists and no
subtitle type does.

**Within an audio group, `DEFAULT=YES` wins, and failing that the first
declared.** That is what a player does and — measured — what FFmpeg already
picks when handed the master, so pairing changes which requests are made, not
which audio lands in the file. Choosing a *language* is deliberately not
offered in the UI; the model carries what a later issue would need.

**A rendition that was asked for and is not declared stops the download.**
`DownerError::VariantNotOffered`, surfaced over the protocol as an ordinary
`terminal` / `failed` / `download_failed` event. Falling back to the default
would hand back a different quality from the one chosen without saying so,
which is the defect KEI-61 exists to remove. This is the `on_conflict`
precedent from ADR-0004: when a value's whole purpose is to change what
happens, guessing at an unrecognised one is worse than refusing.

**Unchosen is unchanged.** No rendition named means the highest bandwidth, the
rule `select_variant` has always applied, so KEI-89's criterion — "not choosing
downloads what it downloads today" — holds by construction, except for
separate-audio masters, which now download one rendition instead of all of them
and write the same file.

## Consequences

* A separate-audio master downloads one rendition instead of every one. On the
  fixture that is 9 segment requests against 11, and the gap grows with the
  number of renditions; on a real master with three or four it is most of the
  data.
* A subtitles-only master is resolved where it previously was not: 4 segment
  requests against 8, for a byte-identical file.
* **The FFmpeg command layer now has two inputs and explicit stream mapping.**
  That is the clause in `AGENTS.md` that makes this an ADR. KEI-68's native
  concurrent downloader inherits a model that already expresses "this video
  plus that audio", which it would otherwise have had to add.
* Ordering is by declared bandwidth and then frame area, never by position.
  **A bandwidth tie between two different resolutions now prefers the larger
  frame**, where the previous code kept whichever came first. No real master is
  known to do this; it is a deliberate, stated change rather than an accident.
* `tests/separate_audio.rs` re-runs the measurement above against real media
  and a real FFmpeg, and skips loudly below 7.1 — the HLS options this project
  passes do not exist before then (KEI-81), so an older FFmpeg would fail at
  argument parsing and read as a failure of the behaviour under test.
* `tests/support/mod.rs` gained `Reply::bytes`, because a fixture server that
  can only serve text cannot serve a segment FFmpeg will decode.

## Unverified

* **Only one packaging tool was measured.** The fixture is built by FFmpeg's
  own HLS muxer. A master from a commercial packager may declare attributes
  this parser reads differently, though the attribute grammar is the
  specification's.
* **`-map 0:v:0` assumes the chosen variant has exactly one video stream.** A
  media playlist carrying two would take the first. No such playlist was seen,
  and the HLS specification does not describe one.
* **How much this saves in the field is still inferred.** ADR-0010 noted the
  same limit. Two and three renditions were measured; "n renditions cost n×" is
  extrapolation from those points.
* **DASH is untouched.** `.mpd` is now classified and labelled experimental in
  the popup, and nothing here validates it against FFmpeg.
* **An audio group with no `DEFAULT=YES`** falls back to the first declared
  rendition. That matches player behaviour as documented, not as measured.
