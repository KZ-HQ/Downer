# downer

[![CI](https://github.com/KZ-HQ/Downer/actions/workflows/ci.yml/badge.svg)](https://github.com/KZ-HQ/Downer/actions/workflows/ci.yml)

`downer` downloads one HTTP(S) source or media URL per invocation through
FFmpeg. Source pages are fetched and scanned for direct video files and HLS
playlists (`.m3u8`); the best discovered media URL is then passed through the
same download/remux path. Direct files, segmented streams, and remuxing are
supported when FFmpeg can stream-copy the input.

The repository also includes a Firefox WebExtension. The extension scans the
currently loaded page, including media discovered by its player, and delegates
the actual download to the Rust/FFmpeg native host — so the page's
already-established browser session can be used for protected media.

## Documentation

| | |
| --- | --- |
| [User guide](docs/user-guide.md) | Install, first download, settings, controls, where files go, and what Downer does not do |
| [Troubleshooting](docs/troubleshooting.md) | Organized by symptom, from "native host disconnected" to where the logs are |
| [Architecture](docs/architecture.md) | Components, data flow, and the trust and session boundaries |
| [Native messaging protocol](docs/protocol.md) | The contract between the extension and the host |
| [Decision records](docs/adr/README.md) | Why things are the way they are |
| [End-to-end tests](docs/e2e-firefox.md) | The real-Firefox harness and where its browser comes from |
| [`AGENTS.md`](AGENTS.md) | Contributor workflow, repository layout, and the required checks |

## Prerequisites

- **FFmpeg 7.1 or newer** at runtime, as `ffmpeg` on `PATH` or supplied with
  `--ffmpeg /path/to/ffmpeg`. It is not bundled
  ([ADR-0015](docs/adr/0015-ffmpeg-as-the-only-engine.md)). The minimum is 7.1
  because HLS playlists with nonstandard segment names are downloaded with
  `-extension_picky 0`, which older builds do not accept. Development and
  testing currently use FFmpeg 9.0.1.

  An older FFmpeg is **warned about, not refused** — the two 7.1-only options
  are omitted so the download can proceed. It remains unsupported. See
  [ADR-0006](docs/adr/0006-ffmpeg-version-detection.md).

- **Rust 1.88 or newer** to build the CLI (declared as `rust-version` in
  `Cargo.toml`); it is the floor required by the versions pinned in
  `Cargo.lock`.

- **macOS or Linux.** Windows is not supported: it registers native hosts in the
  registry, and pause/resume use Unix signals.

## Quick start

```sh
cargo install --path .
downer 'https://example.com/video.mp4'
downer 'https://example.com/live/index.m3u8' --dir ./downloads
downer 'https://example.com/watch/video' --dir ./downloads
```

On macOS with Homebrew, `make setup` installs the `rust` and `ffmpeg` formulae
when they are missing and then verifies the toolchain.

Check that everything a download needs is in place:

```sh
downer doctor --dir ~/Downloads
```

Full options, cookies for protected media, quality selection, output naming and
exit codes are in the [user guide](docs/user-guide.md); `downer --help` lists
every flag.

## Firefox extension

```sh
make extension
```

That builds and registers the native host and packages the extension; load
`extension/manifest.json` in Firefox from `about:debugging` → **This Firefox** →
**Load Temporary Add-on**. Open the source page, let its video player load, then
click the Downer toolbar button and choose a discovered media URL.

The extension is unsigned and not on AMO, so a **permanent** install needs
Firefox Developer Edition, Nightly or ESR — **Release and Beta cannot install it
permanently** and must use the temporary path above. Both routes, the `.xpi`
from each release and its checksum verification are in the
[user guide](docs/user-guide.md#3-install-the-extension).

## Handy Make commands

```sh
make check
make build
make run ARGS='https://example.com/video.mp4 --dir ./downloads'
make install
make extension-xpi
make clean
```

`make check` does not need a browser. The end-to-end tests, which install the
extension into a real headless Firefox, are a separate step:

```sh
make extension-browser   # once: installs Firefox and geckodriver
make extension-e2e
```

See [`docs/e2e-firefox.md`](docs/e2e-firefox.md).

## Versioning

The CLI and the Firefox extension share one product version; see "Versioning" in
[`AGENTS.md`](AGENTS.md). User-visible changes are listed in
[`CHANGELOG.md`](CHANGELOG.md).

## Releasing

A release is a `vX.Y.Z` tag. `.github/workflows/release.yml` builds
`downer-<version>-<target>.tar.gz` for macOS arm64 and Linux x86_64, packages
`downer-<version>.xpi`, writes one `SHA256SUMS`, and attaches all of it to a
GitHub Release whose notes are that version's section of
[`CHANGELOG.md`](CHANGELOG.md).

To cut one: bump `version` in both `Cargo.toml` and `extension/manifest.json`,
move the changelog's `Unreleased` entries under the new version, merge that,
then tag the merge commit `vX.Y.Z` and push the tag. The workflow refuses to
build if the tag and the two version fields disagree, so a half-done bump fails
before it publishes anything.

Everything the workflow does apart from talking to GitHub is in
`scripts/release_artifacts.sh` and `scripts/release_collect.py`, which run on a
laptop:

```sh
./scripts/release_artifacts.sh --out staged --tag v0.5.0
```

Running the workflow from the Actions tab (`workflow_dispatch`) builds and
uploads the same artifacts without creating a Release, which is how to check a
change to it before tagging.

## Known limitations

Moved to the docs, where the remedies are: the list of what Downer does not do
is in the [user guide](docs/user-guide.md#what-downer-does-not-do), and
symptom-by-symptom help is in
[troubleshooting](docs/troubleshooting.md).

## Roadmap, status, and handoffs

Planned work, current status, known gaps, and handoff notes live in the Linear
project **Downer** (https://linear.app/kzhq/project/downer-fb4196d41645), not in
this repository. See [`AGENTS.md`](AGENTS.md) for the contributor workflow.
