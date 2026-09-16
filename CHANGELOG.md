# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

The Rust package and the Firefox extension share one product version; see
"Versioning" in `AGENTS.md`.

## [Unreleased]

### Added

- Rust CLI `downer URL [options]` that downloads one HTTP(S) source or media
  URL per invocation through FFmpeg, with stream-copy remuxing for direct
  files and segmented streams.
- Source-page scraping that resolves direct video files and HLS playlists
  (`.m3u8`) from a page, forwarding the source page as the FFmpeg Referer and
  a browser-like User-Agent, and reporting Cloudflare challenge responses
  explicitly.
- Output handling that percent-decodes and sanitizes inferred filenames,
  writes playlist downloads as `.mp4`, refuses collisions unless
  `--overwrite` is supplied, and retains partial files after failures.
- Stable exit codes: `2` invalid input, `3` output-path problem, `4` FFmpeg
  unavailable, `5` media or FFmpeg failure.
- Acceptance of HLS playlists with nonstandard segment names, including
  JPEG-named video segments.
- Firefox WebExtension that scans the active page for media, forwards the
  page's cookies and User-Agent to the native host for the selected download,
  and shows progress, completed/total HLS segments, and an estimated
  percentage.
- Native messaging host (`downer --native-host`) implementing the `download`,
  `pause`, `resume`, `cancel`, and `hls-info` commands with streamed progress
  and FFmpeg log events.
- Pause, resume, and cancel controls in the popup; pause and resume use Unix
  process signals on macOS and Linux, cancel is supported on all platforms.
- Settings page with a configurable output directory, an optional FFmpeg
  processing-thread count, and a live FFmpeg log console keeping the most
  recent 500 lines per download.
- `Makefile` targets for setup, checks, builds, native-host installation, and
  extension packaging, plus native-host installation scripts for macOS and
  Linux.
- MIT `LICENSE`, this changelog, a declared Rust MSRV, and a documented
  minimum FFmpeg version.

### Known limitations

- `--threads` controls FFmpeg processing, not concurrent HLS segment HTTP
  requests.
- HLS progress totals depend on successfully reading a VOD playlist;
  Cloudflare or other session protections can prevent metadata access even
  when a browser player can load the media.
- Completed HLS segment counts are estimated from FFmpeg output duration.
- Batch downloads, authentication automation, provider-specific scraping,
  robust resume, and live playlist scheduling are not implemented.
- AES-128/SAMPLE-AES, byte ranges, discontinuities, and alternate HLS tracks
  are not validated.
