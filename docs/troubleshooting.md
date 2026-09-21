# Troubleshooting

Organized by what you saw. If you have not yet, run the setup checks first —
they catch most of what follows before you have to read about it:

```sh
downer doctor --dir ~/Downloads
```

and, for the extension, **Settings → Check setup**. Those two do not always
agree, and when they disagree the extension is the one to believe: it runs the
checks *inside the native host*, in the minimal environment Firefox gives it,
which is where downloads actually happen.

## "Native host disconnected", or the popup does nothing

Firefox's wording for "no such native application, or it exited immediately".
Four causes, in the order worth checking.

**The host was never registered.** Run `downer install-host` and reload the
extension. `downer doctor` reports this as a failing check.

**The launcher points at a binary that is gone.** The usual cause is
`downer install-host --link` or `--dev` against a `target/release/downer` that
has since been rebuilt elsewhere, moved, or `cargo clean`ed. Re-run
`downer install-host` — without `--link` it copies the binary somewhere durable,
which is what makes the registration independent of any checkout
([ADR-0008](adr/0008-relocatable-native-host-installation.md)).

**A stale binary after a Rust change.** Firefox runs whatever the launcher
points at. If you changed native-host code and did not rebuild, you are running
the old host. This is this project's most common "it worked before" report:

```sh
cargo build --release      # or: make extension
```

**The add-on ID does not match.** The host allows exactly
`downer@kz-hq.github.io`. If you built an XPI from a modified
`extension/manifest.json`, the host will refuse it. Both sides read that one
field, so an unmodified checkout cannot drift.

## "FFmpeg not found" — but it is on my PATH

Almost always the extension, not the CLI, and the reason is that they do not
share an environment. **Firefox launches the native host with a minimal
environment**, so a `PATH` or a `DOWNER_FFMPEG` exported in your shell profile
never reaches it.

Record the path where the host will find it, either way round:

```sh
downer install-host --ffmpeg /opt/ffmpeg/bin/ffmpeg
```

or put it in **Settings → FFmpeg path**. It is stored in
`~/.config/downer/config.json` (macOS: `~/Library/Application Support/downer/config.json`).

The host searches `DOWNER_FFMPEG`, then `/opt/homebrew/bin/ffmpeg`, then
`/usr/local/bin/ffmpeg`, then `ffmpeg` on whatever `PATH` it was given.

**Check setup** names the FFmpeg the host actually resolved. That is the answer
that counts; `which ffmpeg` in a terminal answers a different question.

## "FFmpeg is older than the minimum supported 7.1"

Exit code `4` from the CLI. The minimum is 7.1 because HLS playlists with
nonstandard segment names need `-extension_picky 0`, which older builds reject.

An old FFmpeg is **warned about, not refused**: those two options are dropped
and the download proceeds, because the strict checking they switch off arrived
in 7.1 as well, and 6.x does not need them. So a warning is not a failure — but
the version remains unsupported, and anything else needing 7.1 will still fail
with this error.

`apt install ffmpeg` on Ubuntu 24.04 LTS gives 6.1.1, which is the usual way to
meet this. Install a newer build, or point Downer at one you already have. The
measurements are in [ADR-0006](adr/0006-ffmpeg-version-detection.md).

## "No media URL found"

The extension lists what the page has actually requested, so:

**The player has not loaded yet.** Press play, let it buffer a moment, and open
the popup again. Media discovered by the player is most of what this extension
finds.

**The page is one Firefox will not let extensions read.** `about:` pages, AMO,
and other privileged pages are blocked by Firefox itself, and the popup says so
("This page cannot be scanned").

**The media is behind a challenge.** See the next section.

**It is a format Downer does not detect.** DASH (`.mpd`) is listed but marked
experimental, and nothing has yet been validated against FFmpeg end to end.

From the CLI, the equivalent error is `no media found on source page`. The CLI
fetches the page once and does not run its JavaScript, so a page that assembles
its media URL in the player will yield nothing there even when the extension
finds it. That asymmetry is the reason the extension exists.

## Cloudflare, HTTP 403, or a challenge page

The scraper reports Cloudflare challenge responses explicitly rather than
pretending the media is missing, and does not attempt to solve JavaScript
challenges.

**From the extension**, this usually resolves itself: the page's own session is
used, so if the video plays in that tab the download generally works. If it does
not, the session may be scoped more narrowly than the media host — cookies are
deliberately scoped to the media URL's host, so a cross-host segment server gets
none ([ADR-0002](adr/0002-cookie-scoping-and-argv-exposure.md)).

**From the CLI**, you must supply the session yourself:

```sh
downer 'https://example.com/watch/video' --cookie-file ~/.config/downer/cookie
```

Copy the `Cookie` header from the browser's network inspector for a request that
worked. `--cookie-file` and `DOWNER_COOKIE` keep it out of your shell history;
`--cookie` does not.

If the challenge is on the media host itself rather than the page, a direct
signed media URL copied from the browser is the remaining option.

## The segment total never appears

The popup shows "Waiting for playlist metadata…", or a percentage that never
arrives while the download itself proceeds normally.

The total comes from reading the playlist, which is tried twice: by the content
script in the page's session, and by the host as a fallback. When a server
blocks both, the download can still run — FFmpeg fetches segments with the
cookies it was given — but nothing knows how many there are, and the popup says
why rather than showing a made-up number.

Live playlists have no total by nature.

The download is not in trouble. It finishes normally; only the progress estimate
is missing. Completed segment counts are in any case **estimates**, derived from
FFmpeg's output timestamps rather than from counting segments
([ADR-0011](adr/0011-one-playlist-parser.md)).

## "Could not resume: the connection was lost while paused"

Pause suspends the FFmpeg process; it does not tell the server anything. The
connections FFmpeg holds go idle, and servers close idle connections and expire
signed segment URLs on their own schedule. A long pause can therefore cost the
download, and this message means it did.

The input is fine. **Retry** restarts the download from the beginning — there is
no byte-range resume, and the part-written file is kept as evidence rather than
as something to continue from.

This is reported as `resume_failed` rather than a generic failure precisely so
the advice can be "retry" instead of "this media cannot be downloaded"
([ADR-0012](adr/0012-control-semantics.md)).

Keep pauses short. There is no way to hold a connection open across an arbitrary
pause without a segment scheduler of our own, which is not built yet.

## Pause and Resume are missing from the popup

They need Unix process signals, so they appear on macOS and Linux and nowhere
else. The host reports this as `capabilities.pause_resume` in its handshake and
the extension hides the buttons when it is false.

Cancel is always available: it works on every platform, and on a paused download
without resuming it first.

## A download failed and left a partial file

That is deliberate. A download that fails on its own **always** keeps what
FFmpeg wrote, whichever way the cancel toggle is set: the fragment is the
evidence for what went wrong, and it may be most of a long download.

A **cancel** deletes it by default, because a cancel means you did not want the
file. Settings has a toggle to keep it instead.

A job that was running when Firefox closed keeps its file too, and shows as
`interrupted` — nobody asked for that file to go, so it does not
([ADR-0012](adr/0012-control-semantics.md)).

## Where the logs are

**The extension: Settings → Live FFmpeg logs.** The most recent 500 lines per
download (bounded at 128 KiB, over at most 20 jobs), filterable by download,
with a button to clear the history. This is the first place to look for server
responses, playlist access failures and FFmpeg conversion errors.

**The CLI** writes FFmpeg's output to its own stderr as it runs, and writes no
file: a terminal already shows you everything.

**The native host keeps its own log file**, which is where to look when there
is nothing to look at anywhere else — a host that will not start produces no
events, so the extension can only say "native host disconnected". Firefox sends
the host's stderr to the Browser Console and keeps none of it after the process
exits, which is why this file exists
([ADR-0022](adr/0022-a-bounded-redacted-host-log-file.md)).

| | |
| --- | --- |
| macOS | `~/Library/Logs/downer/host.log` |
| Linux | `~/.local/state/downer/logs/host.log` |

`downer doctor` prints the exact path, and so does Settings → **Check setup**.
It holds host start and stop, every request, every FFmpeg spawn and every job's
outcome, bounded at two files of 1 MiB so it cannot fill a disk.

**It is safe to attach to a bug report.** No cookie value, URL query string or
page title can appear in it: the logged command line carries a cookie *count*
(`<1 cookie>`), header *names* (`<User-Agent,Referer>`) and `<output>` in place
of the filename, since that filename is the page title when title naming is on.

FFmpeg's own output is not in it by default — one line per HLS segment would
crowd out everything else. To turn it on, put this in
`~/.config/downer/config.json` and start a new download:

```json
{ "log_level": "debug" }
```

`"off"` disables the file entirely, and no file is created.

**URLs in both are redacted**: scheme, host, port and path are kept, and the
query, any fragment and any userinfo are replaced. Signed URLs and session
tokens live in the query, and FFmpeg logs one URL per segment
([ADR-0003](adr/0003-redact-urls-in-logs.md)). If you are comparing a logged URL
against one from the browser, that is why they differ.

## Something else

Exit codes narrow it down:

| Code | Where to look |
| --- | --- |
| `2` | The arguments. Conflicting options, or a URL that is not `http(s)`. |
| `3` | The output path — a collision, a directory, or somewhere unwritable. |
| `4` | The FFmpeg installation. Not the media. |
| `5` | The media or the server. A retry may help. |
| `6` | `downer doctor` printed which check failed, above the error. |

Anything reproducible that is not covered here belongs in the Linear project
[Downer](https://linear.app/kzhq/project/downer-fb4196d41645), which is where
this project's known gaps are tracked.
