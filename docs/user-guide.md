# User guide

Downer downloads one media URL at a time through FFmpeg. There are two ways to
use it: a command-line tool, and a Firefox extension that hands downloads to the
same tool so the browser's own session can be used for protected media.

If something does not work, [`troubleshooting.md`](troubleshooting.md) is
organized by symptom.

## Before you start

**Platform.** macOS and Linux. Windows is not supported: it registers native
messaging hosts in the registry, and pause/resume use Unix signals.

**FFmpeg 7.1 or newer**, which Downer runs but does not bundle. An older FFmpeg
is warned about rather than refused — two HLS options are dropped so the
download can proceed — but it is unsupported, and anything else needing 7.1 will
still fail. Development and testing use 9.0.1.

**Rust 1.88 or newer**, only if you are building from source rather than using a
released binary.

## Install

### 1. Get the binary

From a release, on the [releases page](https://github.com/KZ-HQ/Downer/releases):
download the `downer-<version>-<target>.tar.gz` for your platform, and check it
against the `SHA256SUMS` published beside it.

```sh
sha256sum -c SHA256SUMS      # shasum -a 256 -c SHA256SUMS on macOS
tar xzf downer-<version>-<target>.tar.gz
```

Or from source:

```sh
cargo install --path .
```

On macOS with Homebrew, `make setup` installs the `rust` and `ffmpeg` formulae
if they are missing and then verifies the toolchain. On Linux, install both with
your package manager.

Check it runs:

```sh
downer --help
```

If you are only going to use the command line, you are done — skip to
[Your first download](#your-first-download).

### 2. Register the native messaging host

Firefox reaches the downloader through a native messaging host: a manifest in a
per-user directory naming a program Firefox may launch, and the extension ID
allowed to talk to it. `downer` registers itself:

```sh
downer install-host
```

That copies the binary to a stable per-user location (`~/.local/share/downer/`
on Linux, `~/Library/Application Support/downer/` on macOS) unless it is already
somewhere durable such as `~/.cargo/bin`, writes a small launcher beside it, and
points the Firefox manifest at the launcher. **The registration does not depend
on any checkout**, so a source directory can be moved or deleted afterwards.

`downer install-host --link` registers the running binary where it is instead of
copying it, which is right for a binary you keep in a fixed place yourself.

Firefox launches the host with a minimal environment, so a `PATH` or
`DOWNER_FFMPEG` set in your shell cannot reach it. If FFmpeg is somewhere the
host would not look — it searches Homebrew's standard locations, then `PATH` —
record the path at install time:

```sh
downer install-host --ffmpeg /opt/ffmpeg/bin/ffmpeg
```

The path is stored in `~/.config/downer/config.json` (macOS:
`~/Library/Application Support/downer/config.json`) and used unless
`DOWNER_FFMPEG` overrides it. The extension's Settings page has the same field.

To reverse all of it:

```sh
downer uninstall-host             # manifest, launcher, and config
downer uninstall-host --binary    # and the copied binary
```

### 3. Install the extension

The extension is not published on [addons.mozilla.org](https://addons.mozilla.org)
(AMO) and is not signed. That decides which of the two install paths is open to
you.

**Temporarily, in any Firefox.** `about:debugging` → **This Firefox** → **Load
Temporary Add-on**, selecting `extension/manifest.json` (or the `.xpi`).
`web-ext run` does the same from a command line. A temporary add-on bypasses
signature enforcement in every edition, and disappears when Firefox closes.
This is the development path and the one `make extension` sets up.

**Permanently, from the `.xpi`.** Every tagged release attaches
`downer-<version>.xpi` to its
[GitHub Release](https://github.com/KZ-HQ/Downer/releases); `make extension-xpi`
builds the same file into `dist/` from a checkout. Installing it so that it
survives a restart needs a Firefox that can be told not to require signatures:

1. Use **Developer Edition**, **Nightly**, or **ESR**. Firefox **Release and
   Beta cannot install this add-on permanently** — they enforce add-on signing
   and offer no override, so for them the temporary path above is the only one.
2. In `about:config`, set `xpinstall.signatures.required` to `false`.
3. Open `about:addons` → the gear icon → **Install Add-on From File…** and
   pick the `.xpi`.

The add-on ID is `downer@kz-hq.github.io`, and the native messaging host allows
exactly that ID. Both are read from `extension/manifest.json`, so an XPI built
from a checkout and a released one register the same way.

The extension package is built reproducibly, so `make extension-xpi` on the
tagged commit produces a file with the same checksum as the released one.

**If signing ever becomes worthwhile**, the route that fits a private tool is
AMO *unlisted* signing: `web-ext sign --channel unlisted` with a free AMO
account and an API key and secret. It returns a signed XPI that installs in
Firefox Release without publishing anything to the AMO catalogue. It is
deliberately not part of the release pipeline today — it would put a credential
in CI for a tool with one user.

### 4. Check the setup

```sh
downer doctor --dir ~/Downloads
```

It reports whether the host is registered with Firefox and its launcher still
exists, whether FFmpeg runs and is new enough, and — with `--dir` — whether
downloads can be written where you want them. Each check prints what was found
and, when something is wrong, what to do about it.

It exits `6` if any check **failed**. A **warning** exits `0`, because a warning
means downloads still work; an FFmpeg older than 7.1 is the usual one.

The same checks are available from the extension: **Settings → Check setup**
runs them in the native host and shows the results. That is the place to look
when a download fails before FFmpeg starts, because it reports what the *host*
found, not what your shell would find.

## Your first download

### From the command line

```sh
downer 'https://example.com/video.mp4'
downer 'https://example.com/live/index.m3u8' --dir ./downloads
downer 'https://example.com/watch/video' --dir ./downloads
```

The third form is a source *page*: Downer fetches it, scans it for direct video
files and HLS playlists, and downloads the best one it finds.

### From the extension

Open the page in Firefox and let its video player load — the extension finds
media the player has requested, so a page that has not started playing may show
nothing yet. Then click the Downer toolbar button and choose a media URL from
the list.

The popup shows a preparing or downloading state immediately. For HLS it reads
the playlist through the page's own session and shows completed segments, total
segments, and an estimated percentage.

Downloads go to the directory set in Settings. Left blank — the default — the
native host uses your desktop's own download directory.

## Where downloads go

The **command line** writes to the current directory unless `--dir` or
`--output` says otherwise.

The **extension** writes to the directory set on the Settings page. Left blank,
the native host uses your desktop's download directory, or `~/Downloads` where
the desktop has none. Linux only reports a download directory when XDG
user-directory configuration is present (`~/.config/user-dirs.dirs`), which
minimal installs and containers often lack; `~/Downloads` is what Firefox itself
falls back to there, so downloads land beside the browser's. The directory is
created on first use.

**Check setup** names the exact directory the host resolved, including when it
does not exist yet. That answer comes from the host rather than from a guess
about your platform.

### Names and collisions

A download is named after the media URL's own filename when it has one. When
that name is generic — `index`, `playlist`, `master`, `download`, `video`,
`media`, or digits only, which covers most HLS playlists — the download is named
**`video.mp4`**. A fixed default is predictable, where a name derived from the
page varies with the site, the locale, and whatever marketing put in the title.

Naming a download after the page title is available but **off by default**: use
`--name` on the command line, or tick "Name downloads after the page title" in
Settings. A title is sanitized the same way a URL-derived name is and truncated
to 80 characters. Bear in mind that page titles can carry account or document
names, which would then appear in your downloads folder.

Because the default name repeats, collisions are the common case rather than the
exception:

| Situation | Default | Effect |
| --- | --- | --- |
| Inferred filename (`--dir`, or neither flag, and every extension download) | `rename` | Writes `video_2.mp4`, `video_3.mp4`, … beside the existing file. Nothing is replaced. |
| Exact path (`--output`) | `fail` | Refuses with exit code 3 and leaves the existing file untouched. |

`--on-conflict fail|rename|overwrite` overrides either default, and
`--overwrite` is shorthand for `overwrite`. The two cannot be combined. Settings
offers the same choice; "Replace the existing file" permanently discards what is
already there.

Nothing is ever overwritten without being asked. The reasoning is in
[ADR-0004](adr/0004-output-naming-and-collision-policy.md) and
[ADR-0005](adr/0005-default-output-name-over-derived-one.md).

## Choosing a quality

A master playlist lists several renditions of the same video. Without
`--rendition`, Downer takes the highest bandwidth it declares, and the extension
does the same when you do not pick one.

```sh
downer 'https://example.com/live/master.m3u8' --rendition 720p
downer 'https://example.com/live/master.m3u8' --rendition worst
```

`--rendition` takes `best`, `worst`, a height such as `720p` (or `1280x720`, of
which only the height is matched), or an exact variant URL. A rendition the
playlist does not offer **stops the download** rather than quietly becoming a
different one — a live playlist repackaged since you last looked is when that
happens, and the remedy is to look again.

In the extension, a master with more than one rendition gets a **Quality** menu
in the popup. A media playlist, or a master with one rendition, gets no menu.

Where a master carries its audio as a separate rendition — common in modern
packaging — the chosen video and that audio are handed to FFmpeg together, so
picking a lower quality does not cost you the sound
([ADR-0014](adr/0014-pair-a-rendition-with-its-audio.md)).

## Controls

While a download is active the popup offers Pause, Resume and Cancel.

**Pause** suspends the FFmpeg process. FFmpeg is not told it has been paused, so
the connections it holds open simply go idle — and servers close idle
connections and expire signed segment URLs on their own schedule. A short pause
is safe; a long one can cost the download. If that happens the popup says the
connection was lost while paused rather than reporting the media as
undownloadable, and Retry starts the download again. Pause and Resume need Unix
process signals, so the buttons appear on macOS and Linux and nowhere else.

**Cancel** stops the download immediately and works on a paused download without
resuming it first. By default it also deletes what FFmpeg had written: a cancel
means you did not want the file, and the fragment would not play. Settings has a
toggle to keep it instead.

A download that **fails** on its own always keeps its part-written file,
whichever way that toggle is set — that fragment is the evidence for what went
wrong, and it may be most of a long download.

The command line has no equivalent. `Ctrl-C` terminates `downer` outright, so
nothing runs to clean up after it and there is no cancel policy to set.

The full guarantees are in [`protocol.md`](protocol.md) and the reasoning in
[ADR-0012](adr/0012-control-semantics.md).

## Settings

The extension's Settings page (`about:addons` → Downer → Preferences, or the
link in the popup) has:

| Setting | Effect |
| --- | --- |
| **Output directory** | Where downloads go. Blank uses your desktop's download directory. |
| **FFmpeg path** | Where the *host* should look for FFmpeg. Needed when it is somewhere non-standard, because Firefox starts the host with a minimal environment. |
| **FFmpeg processing threads** | FFmpeg's `-threads`. Blank leaves the choice to FFmpeg. This does **not** make HLS segment requests concurrent. |
| **When a file of that name already exists** | Keep both, stop, or replace. |
| **Name downloads after the page title** | Off by default. |
| **Keep the part-written file when I cancel** | Off by default; failures always keep theirs. |
| **Check setup** | Runs the diagnostics in the host and shows the results. |
| **Live FFmpeg logs** | The most recent 500 lines per download, filterable by download, with a button to clear the history. |

## Protected media

The extension is the easy path: it uses the Firefox cookies for the media host
and sends them to the native host for that one download. Cookies are scoped to
the media URL's host, so a server FFmpeg is redirected to — and, for HLS, a
cross-host segment server — receive nothing.

From the command line, supply the cookie header yourself. There are three
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

The value is still visible in FFmpeg's process arguments while a download runs:
FFmpeg has no file-based input for headers or cookies. That limitation is
recorded in
[ADR-0002](adr/0002-cookie-scoping-and-argv-exposure.md).

The scraper sends a browser-like User-Agent and forwards the source page as the
FFmpeg Referer. It reports Cloudflare challenge responses explicitly and does
not attempt to solve JavaScript challenges — for those, a valid browser cookie
or a direct signed media URL is required.

## Exit codes

Use `downer --help` for all options.

| Code | Meaning |
| --- | --- |
| `0` | Success |
| `2` | Invalid input — fix the arguments |
| `3` | Output path problem |
| `4` | FFmpeg unavailable or older than the supported minimum |
| `5` | A media or FFmpeg failure |
| `6` | A failing setup check from `downer doctor` |

They are a stable part of the interface
([ADR-0018](adr/0018-stable-cli-exit-codes.md)).

## What Downer does not do

Limits worth knowing before you rely on it. Symptoms and remedies are in
[`troubleshooting.md`](troubleshooting.md).

* **One URL per invocation.** No batch downloads, no queue, no authentication
  workflows.
* **No concurrent HLS segment fetching.** `--threads` controls FFmpeg's
  processing, not the number of segment requests in flight.
* **No resume of a failed download.** Partial files are kept as evidence, not as
  something to continue from.
* **No re-encoding.** Every download is a stream copy, so a stream that will not
  fit the container is not converted to fit it — the clearest case being
  subtitles, which are ignored entirely because FFmpeg cannot mux WebVTT into an
  MP4 ([ADR-0015](adr/0015-ffmpeg-as-the-only-engine.md)).
* **Alternate audio languages are parsed but not offered.** A download takes the
  group's `DEFAULT=YES` rendition.
* **DASH manifests (`.mpd`) are experimental** — detected and listed, but none
  has been validated against FFmpeg end to end.
* **AES-128/SAMPLE-AES, byte ranges, discontinuities and alternate HLS tracks**
  are unvalidated.
* **Segment counts are estimates.** Completed counts are derived from FFmpeg's
  output timestamps; the playlist supplies the total, for VOD streams.
