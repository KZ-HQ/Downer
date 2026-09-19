# ADR-0011: The extension fetches playlists; Rust parses them

* Status: Accepted
* Date: 2026-09-19
* Issue: [KEI-51](https://linear.app/kzhq/issue/KEI-51)

## Context

Playlist logic existed twice — `src/scraper.rs` and `extension/hls.js` — plus a
third copy of the media-extension list in `extension/media-scan.js`. Part of
that was justified: the extension must **fetch** playlists from the page's
context, because only there is the session a protected or challenged CDN
answers. Parsing what came back needed no such justification.

## The drift was not hypothetical

Both parsers, same inputs, before any change:

| Playlist body | `extension/hls.js` | `src/scraper.rs` |
| --- | --- | --- |
| `#EXTINF:,` | 1 segment, 0 ms | **no info at all** |
| `#EXTINF: 2.5,` | 1 segment, 2500 ms | **no info at all** |
| `#EXTINF:2.5 ,` | 1 segment, 2500 ms | **no info at all** |
| `#EXTINF:+2.5,` | 1 segment, 2500 ms | 1 segment, 2500 ms |
| `#EXTINF:2.5,` | 1 segment, 2500 ms | 1 segment, 2500 ms |

The cause is number parsing, not line scanning: JavaScript's `Number()` trims
whitespace and turns `""` into `0`; Rust's `f64::from_str` does neither. A
playlist with a space after the colon gave the extension a segment count and the
host none, so which total a user saw depended on which path had run.

The two media-extension lists, by contrast, were byte-identical — in agreement
by discipline rather than by construction.

## Decision

**The extension fetches; Rust parses.** `scraper::parse_playlist(text, base_url)`
is the only implementation of what a playlist means. It answers `Master`
(with the rendition to download), `Media` (segment count and duration), or
`Unusable`.

`extension/hls.js` is deleted. The extension sends `playlist_text` on the
`download` request instead of the totals it used to compute, and the host reads
it for both the totals and, for a master, the rendition.

**Why Rust rather than JavaScript**, given the issue offered the reverse:
KEI-69 commits to an HLS playlist model and parser in Rust for the native
scheduler. Deleting the Rust parser would have removed something that has to
come back.

**The fetch cannot move, and does not.** Only the page's context carries the
session; KEI-87 records a CDN answering the host with a challenge page while the
in-session fetch succeeds. Sending the text is what lets one parser read a
playlist the host could never have fetched.

**Absent text is not a failure.** The host fetches for itself exactly as before,
including the `../playlist.m3u8` fallback that the extension used to duplicate.
So a playlist the extension cannot reach still gets its chance, and the CLI —
which has no browser session at all — is unaffected.

**The media-extension list follows the `protocol.json` precedent** rather than a
new mechanism: `tests/fixtures/media-extensions.json` is the definition, and
both sides assert against it — `scraper::tests::media_extensions_match_the_shared_vocabulary`
and `tests/extension/media-scan.test.js`. Generating the JavaScript constant
would be truer single-sourcing, but a content script cannot read a repository
file at runtime and generation would add build machinery for a fifteen-element
list. Adding an extension on one side now fails a test on the other.

## Consequences

* One parser. The edge cases above now have one answer, whichever path runs.
* **The host makes no playlist request when the extension supplied one.** That
  removes the redundant fetch ADR-0010 accepted as a cost: a media playlist
  needed a host fetch purely to be told it had no variants.
* Totals reach the popup as a `progress` event rather than being computed
  locally before the download starts, so they arrive slightly later. KEI-88
  covers showing progress while they are unknown.
* `playlist_text` is bounded by the protocol's existing 1 MiB inbound limit. A
  playlist longer than that is refused as any oversized frame is, and the host
  falls back to fetching.
* The extension keeps the fetching strategy — page context first, then a direct
  fetch — because that is a session concern, not a parsing one.

## Unverified

* **How large real playlists get.** A 1795-segment playlist was observed in the
  field; at roughly 80 bytes per segment that is ~140 KB, comfortably inside the
  1 MiB frame limit. Where the limit actually bites is unmeasured, and the
  fallback for an oversized playlist — the host fetching it itself — has not
  been exercised against one.
* **The parent-playlist fallback is now host-only.** It was previously attempted
  in-session first. A playlist reachable only in-session, whose *parent* is the
  one that resolves, would now fail where it did not before. No such site is
  known; the shape is possible.
* The drift table was measured on the number parsing. Whether the two line
  scanners disagreed anywhere else was not established before the JavaScript one
  was deleted.
