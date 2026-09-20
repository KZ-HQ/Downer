# ADR-0015: FFmpeg is the only engine, it only ever stream-copies, and it is never bundled

* Status: Accepted
* Date: 2026-09-20
* Issue: [KEI-63](https://linear.app/kzhq/issue/KEI-63)
* Followed by: [ADR-0006](0006-ffmpeg-version-detection.md), which decides what
  happens when the FFmpeg found is too old for the options this record chooses
  to pass

## Context

This decision predates the ADR directory. It has been in the code since the
first commit and was never written down, so every later record — ADR-0003,
ADR-0006, ADR-0007, ADR-0014 — reasons *from* it without anyone having stated
it. This record is reconstructed from the code and from the constraints the
other records reveal, not from a fresh evaluation; the evidence is named under
each heading.

Three separate choices sit behind the phrase "downloads through FFmpeg", and
they are separable. A tool could use FFmpeg and re-encode. It could stream-copy
and ship its own HTTP downloader for direct files. It could do either and vendor
an FFmpeg binary so the user installs nothing.

## Decision

### FFmpeg is the only thing that fetches media bytes

`src/ffmpeg.rs` is the only path from a media URL to a file. There is no
in-process downloader: `reqwest` appears in `src/scraper.rs` to fetch **pages
and playlists**, which are text we parse, and never to fetch media.

What that buys is one implementation of the hard part. A direct MP4 over HTTPS,
an HLS playlist with hundreds of segments, a redirect chain, a server that wants
a `Referer` — FFmpeg already handles all of it, including the protocol and
container edge cases nobody on this project will ever enumerate. Writing a
second downloader for the direct-file case would mean two code paths to which
every cookie, redaction and progress rule has to be applied twice, for a case
FFmpeg handles at the same speed.

The cost is real and shows up throughout the other records. FFmpeg is a child
process, so everything this project wants to know about a download has to be
recovered from its stdout and stderr — `-progress` parsing in ADR-0007,
log redaction in ADR-0003 — and everything it wants to *do* to a download has
to be done with process signals, which is ADR-0012. A library would give
callbacks instead of scraped output. The trade was accepted before these
records existed, and every one of them is downstream of it.

### Every invocation is `-c copy`

`src/ffmpeg.rs:490` appends `-c copy` unconditionally. No caller can ask for an
encoder, because the structured invocation of ADR-0007 has no field for one.

Remuxing is repackaging: the same encoded frames written into a different
container. It is I/O-bound, produces a byte-identical video stream, and cannot
lose quality. Transcoding is CPU-bound, lossy, and on a long video slower than
the download by an order of magnitude. A downloader that silently re-encoded
would be a worse downloader, so the only honest default is the one that cannot.

The consequence is that Downer will not repair a mismatch. Where a codec cannot
live in the target container — the standing example is WebVTT subtitles in an
MP4, which is why ADR-0010's rendition handling ignores subtitle renditions
entirely — the answer is that the download does not carry that stream, not that
FFmpeg is asked to convert it. `--threads` therefore controls the little
processing a remux does and nothing else, which is why it does not make HLS
segment fetching concurrent and why that keeps being asked.

### FFmpeg is a runtime dependency, never vendored

`src/native.rs:1084` resolves FFmpeg at run time: `DOWNER_FFMPEG`, then the
two Homebrew locations, then `PATH`. Nothing in the build or the release
pipeline produces an FFmpeg, and `scripts/release_artifacts.sh` ships only this
project's own binary.

Bundling was rejected on licensing before size. This project is MIT
(`LICENSE`). FFmpeg is LGPL-2.1+, and the builds people actually want — the
ones with the popular decoders — are commonly configured `--enable-gpl`.
Shipping one inside an MIT-licensed release would attach obligations to that
release that its own licence does not describe, per platform, per build
configuration, forever. Nothing about a media downloader justifies taking that
on.

Size is the ordinary argument and it also holds: a useful static FFmpeg is tens
of megabytes against a Rust binary of a few, on every platform, for something
most users who want this tool already have.

What it costs is the project's single most common failure: the download that
fails before it starts because no FFmpeg was found, or because Firefox launched
the native host with an environment in which `PATH` does not contain the one the
user installed. That cost is paid down elsewhere rather than by bundling — the
recorded FFmpeg path of ADR-0008, the diagnostics of ADR-0009, and the version
detection of ADR-0006 all exist because the binary is someone else's.

### HLS segment extensions are not validated

For an `.m3u8` input, `hls_lenient` adds `-allowed_segment_extensions ALL` and
`-extension_picky 0` (`src/ffmpeg.rs:461`).

FFmpeg 7.1 began refusing HLS segments whose URL extension is not one it
expects. Real playlists violate that constantly and deliberately: segments named
`.jpeg`, `.png` or with no extension are a routine way to get video past a
caching proxy or an ad blocker that filters on the URL. FFmpeg's check reads the
*URL*, which is not evidence about the bytes — the server's content type and the
demuxer's own parsing are, and both still apply. Refusing those playlists would
reject working media on the strength of a filename, which is the one thing about
a segment that carries no information.

So the check is switched off rather than worked around. The alternative on the
table was to let it fail and tell the user their playlist was unsupported, which
would have been untrue.

This is the weakest of the four in terms of what it protects against, and worth
stating plainly: with the check off, FFmpeg will attempt to demux whatever a
segment URL returns. The protection that remains is FFmpeg's own parsing, which
is the protection that was doing the work anyway. The narrower fix — matching
the content type instead of the extension — is not available as an FFmpeg
option.

## Consequences

* Every download is one FFmpeg process, which is what makes ADR-0013's
  one-download-per-host-process model as cheap as it measured: the host process
  is a rounding error beside the FFmpeg it supervises.
* Pause, resume and cancel are process signals, with the limits ADR-0012
  measured, because there is a process to signal and no library call to make.
* An output is only ever a repackaging of what the server sent. A download that
  plays badly is a stream that was already like that.
* "FFmpeg not found" and "FFmpeg too old" are first-class user-facing states
  rather than internal errors, and three records exist to handle them.
* A future native HLS scheduler (KEI-68, KEI-69) would take segment fetching
  away from FFmpeg while leaving the remux with it. That narrows this decision
  rather than reversing it, and would need an ADR saying which half moved.

## Unverified

* **The licensing reasoning is not a legal opinion.** LGPL-2.1+ and the
  `--enable-gpl` configuration of common builds are stated from FFmpeg's own
  licensing documentation. Nobody with standing to give that advice has looked
  at it, and nobody needs to while nothing is bundled.
* **No transcoding path has ever been benchmarked here**, because none exists.
  The claim that it would be slower than the download is general knowledge about
  codecs, not a measurement from this project.
* **The reason real playlists use misleading segment extensions** is stated from
  the shape of the playlists that motivated the options, not from any survey.
  What is established is narrower and enough: ADR-0006 measured a `seg0.jpeg`
  playlist failing on FFmpeg 7.1+ without these options and downloading with
  them.
