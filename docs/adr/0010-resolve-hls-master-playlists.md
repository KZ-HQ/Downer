# ADR-0010: Resolve an HLS master playlist to one rendition before FFmpeg sees it

* Status: Accepted
* Date: 2026-09-19
* Issue: [KEI-89](https://linear.app/kzhq/issue/KEI-89)

## Context

Found during the first manual verification against a real media site. The live
FFmpeg log showed a single download fetching two renditions at once, in
lockstep:

```text
[https @ …] Opening 'https://<cdn>/<uuid>/360p/video32.jpeg' for reading
[https @ …] Opening 'https://<cdn>/<uuid>/1080p/video32.jpeg' for reading
```

Given a master playlist, FFmpeg's HLS demuxer opens every variant stream it
lists. `src/scraper.rs::select_variant` already chose a rendition — but only to
compute the segment count. The download handed FFmpeg the master URL untouched.

## What was measured

A two-rendition master (320x180 at 100 kbps, 1280x720 at 800 kbps, four segments
each) served over a request-logging HTTP server, against FFmpeg 9.0.1:

| Input | Requests | Renditions fetched | Video stream written |
| --- | --- | --- | --- |
| master playlist (previous behaviour) | 10 | **both** | 1280x720 |
| master playlist + `-map 0:v:0` | 10 | **both** | **320x180** |
| the variant URL directly | **5** | high only | 1280x720 |

Two things follow, and both corrected the issue as filed.

**The output was never wrong.** FFmpeg's default stream selection picks the
highest-quality video stream, which is the rendition `select_variant` counts.
The file on disk was already correct. This is a bandwidth and time defect, not a
correctness one — the low rendition was downloaded and discarded.

**`-map` does not fix it.** Mapping changes what FFmpeg *writes*, not what it
*reads*: all ten requests still happen. Worse, `0:v:0` is the first variant in
the master — the lowest — so it would have silently downgraded every download
while saving nothing.

## Decision

**Resolve the master to a variant URL before building the invocation.**
`scraper::resolve_variant` fetches the playlist with the same User-Agent,
Referer and cookie the download will use, and returns the chosen variant only
when the playlist is a master. `src/lib.rs::download_resolved` uses it for any
HLS input.

The rule is the existing one — highest bandwidth, the same `select_variant` the
segment count already used — so the rendition downloaded is the rendition
counted, and the file is byte-for-byte what it was before.

**Resolution is best-effort.** Every failure returns `None` and the original URL
is used, which is exactly the previous behaviour. A playlist that cannot be
fetched, is answered with a challenge page (ADR-0009's companion case, KEI-87),
or cannot be parsed costs the bandwidth this would have saved and nothing else.
A wasteful download beats a broken one, and this change must not be able to turn
a working download into a failing one.

**One request, not one per rendition.** A master lists every variant in a single
text file, so the cost does not grow with the number of renditions and no media
is fetched to make the choice.

## Consequences

* A master playlist with *n* renditions downloads one instead of *n*. On the
  site this was found against — 1795 segments per rendition — that is half the
  data for an identical file.
* **A media playlist costs one redundant request.** The host fetches it, finds
  no `#EXT-X-STREAM-INF`, discards it, and FFmpeg fetches the same playlist
  again. It is a few hundred bytes and no media, and it is unavoidable without
  knowing the playlist's type before looking. KEI-51 removes it properly: the
  extension already fetches the playlist in-session and could send what it
  found.
* The host now performs playlist discovery it previously left to FFmpeg, which
  is why this is an ADR under `AGENTS.md`'s "discovery ownership" clause as well
  as its FFmpeg command layer one.
* The CLI prints the rendition it selected when it differs from the URL given,
  so a surprising choice is visible rather than silent.
* The fixture server gains a master-playlist route. It had none, which is how a
  bug in what FFmpeg does with a master playlist survived while the parsing that
  feeds it was well tested.

## Unverified

* **The end-to-end half is measured, not asserted.** `tests/hls_variant.rs`
  pins the decisive step — the URL FFmpeg is handed — against a recording
  server. That a variant URL then fetches only that rendition is FFmpeg's
  behaviour, measured above and by hand against the fixture server, but not
  asserted in CI: the recording server serves text, so it cannot serve the real
  segments a genuine download needs.
* Only two renditions were measured. Nothing suggests more behave differently,
  but "n renditions cost n×" is inference from two points.
* Audio-only and subtitle renditions declared with `#EXT-X-MEDIA` rather than
  `#EXT-X-STREAM-INF` were not exercised. `select_variant` reads only
  `#EXT-X-STREAM-INF`, so a master whose audio is a separate `#EXT-X-MEDIA`
  group may resolve to a video-only variant. That is a real gap and deserves its
  own issue rather than a guess here.
