# downer

[![CI](https://github.com/KZ-HQ/Downer/actions/workflows/ci.yml/badge.svg)](https://github.com/KZ-HQ/Downer/actions/workflows/ci.yml)

`downer` downloads one HTTP(S) source or media URL per invocation through
FFmpeg. Source pages are fetched and scanned for direct video files and HLS
playlists (`.m3u8`); the best discovered media URL is then passed through the
same download/remux path. Direct files, segmented streams, and remuxing are
supported when FFmpeg can stream-copy the input.

HLS playlists with nonstandard segment names, including JPEG-named video
segments, are accepted by disabling FFmpeg's strict HLS segment-extension
matching.

The repository also includes a Firefox WebExtension. The extension scans the
currently loaded page, including media discovered by its player, and delegates
the actual download to the Rust/FFmpeg native host. This lets the page's
already-established browser session be used for protected media.

## Prerequisites

- Rust and Cargo, to build the CLI. The minimum supported Rust version is
  **1.88** (declared as `rust-version` in `Cargo.toml`); it is the floor
  required by the dependency versions pinned in `Cargo.lock`.
- FFmpeg available at runtime as `ffmpeg` on `PATH`, or supplied with
  `--ffmpeg /path/to/ffmpeg`. The minimum supported FFmpeg version is **7.1**,
  because HLS playlists with nonstandard segment names are downloaded with
  `-extension_picky 0`, which older builds do not accept. Development and
  testing currently use FFmpeg 9.0.1.

The CLI and the Firefox extension share one product version; see "Versioning"
in `AGENTS.md`. User-visible changes are listed in `CHANGELOG.md`.

## Handy Make commands

On macOS with Homebrew, install the prerequisites and verify them with:

```sh
make setup
```

Other useful targets:

```sh
make check
make build
make run ARGS='https://example.com/video.mp4 --dir ./downloads'
make install
make clean
```

`make setup` installs the `rust` and `ffmpeg` Homebrew formulae when they are
missing, then runs the toolchain check. On other platforms, install Rust and
FFmpeg using the platform's package manager and use the same build/test
targets.

## Firefox extension

Build and register the native host, then load the `extension/` directory in
Firefox from `about:debugging` → **This Firefox** → **Load Temporary Add-on**
by selecting `extension/manifest.json`:

```sh
make extension
```

Open the source page in Firefox, let its video player load, then click the
Downer toolbar button and choose a discovered media URL. The extension uses
Firefox cookies for the media host and sends them to the native host only for
the selected download. The native host defaults to the operating system's
Downloads directory; configure another path from the extension's Settings
page if needed.

Firefox native messaging requires the extension ID in the native-host manifest
to match the ID in `extension/manifest.json`. `make extension-install` handles
this on macOS and Linux. The native host searches Homebrew's standard FFmpeg
locations and then `PATH`; set `DOWNER_FFMPEG` if FFmpeg is installed
elsewhere.

After choosing a media URL, the popup immediately shows a preparing or
downloading state. For HLS, it reads the selected playlist through the source
page's browser session and displays completed segments, total segments, and an
estimated percentage. The native host also probes the playlist as a fallback.
If the server blocks both session-aware probes, the download can still run but
the segment total is unavailable and the popup explains why. Segment counts
are estimates of completed HLS segments based on FFmpeg's output timestamp;
the playlist supplies the total for VOD streams.

While a download is active, the popup provides Pause, Resume, and Cancel
controls. Pause and Resume use Unix process signals on macOS and Linux;
cancellation is supported on all platforms supported by the native host.

The Settings page includes a live FFmpeg log console. It keeps the most recent
500 lines per download, supports filtering by download, and can clear the
stored log history. Logs are useful for diagnosing server responses, playlist
access, and FFmpeg conversion failures.

The Settings page can also set an optional FFmpeg processing-thread count.
Leave it blank for FFmpeg's automatic choice. This setting does not make HLS
HTTP segment requests concurrent; that requires a separate segmented-download
implementation.

Build and run:

```sh
cargo install --path .
downer 'https://example.com/video.mp4'
downer 'https://example.com/live/index.m3u8' --dir ./downloads
downer 'https://example.com/watch/video' --dir ./downloads
downer 'https://example.com/video.mp4' --output ./downloads/video.mp4
downer 'https://example.com/video.mp4' --output ./downloads/video.mp4 --overwrite
```

By default, an inferred filename is written to the current directory. URL
path names are percent-decoded and sanitized; playlist names become `.mp4`
outputs. Existing files are never replaced unless `--overwrite` is present.
Partial files are retained if FFmpeg fails for diagnostics. Resuming failed
downloads is not promised in v1.

For a source page that requires an existing browser session, supply the copied
cookie header; it is used for both the page scraper and FFmpeg. There are three
sources, in order of precedence:

```sh
downer 'https://example.com/watch/video' --cookie-file ~/.config/downer/cookie
DOWNER_COOKIE='session=...' downer 'https://example.com/watch/video'
downer 'https://example.com/watch/video' --cookie 'session=...'
```

`--cookie-file` reads the header from a file, ignoring surrounding whitespace,
and `DOWNER_COOKIE` is used when neither option is given. Both keep the value
out of shell history, which is why they are preferred over `--cookie`. Passing
`--cookie` and `--cookie-file` together is an error (exit status `2`).

Cookies are scoped to the media URL's host, so a server FFmpeg is redirected to
and, for HLS, a cross-host segment server receive nothing.

The value is still visible in FFmpeg's process arguments while a download runs:
FFmpeg has no file-based input for headers or cookies. That limitation is
recorded in
[`docs/adr/0002-cookie-scoping-and-argv-exposure.md`](docs/adr/0002-cookie-scoping-and-argv-exposure.md).

The scraper sends a browser-like User-Agent and forwards the source page as the
FFmpeg Referer. It reports Cloudflare challenge responses explicitly; it does
not attempt to solve JavaScript challenges automatically. In that case, a
valid browser cookie or a direct signed media URL is required.

Use `downer --help` for all options. Exit status `2` indicates invalid input,
`3` an output-path problem, `4` unavailable FFmpeg, and `5` a media or FFmpeg
failure.

## Roadmap, status, and handoffs

Planned work, current status, known gaps, and handoff notes live in the Linear
project **Downer** (https://linear.app/kzhq/project/downer-fb4196d41645), not
in this repository. See `AGENTS.md` for the contributor workflow.

## Known limitations

- `--threads` controls FFmpeg processing, not concurrent HLS segment HTTP
  requests.
- HLS progress totals depend on successfully reading a VOD playlist. Cloudflare
  or other session protections can prevent metadata access even when a browser
  player can load the media.
- Completed HLS segment counts are estimated from FFmpeg output duration.
- Batch downloads, authentication automation, provider-specific scraping,
  robust resume, and live playlist scheduling are not implemented.
- AES-128/SAMPLE-AES, byte ranges, discontinuities, and alternate HLS tracks
  require validation before a concurrent segment scheduler can handle them.
