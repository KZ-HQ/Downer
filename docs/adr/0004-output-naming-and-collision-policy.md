# ADR-0004: Name downloads from the page title, and rename rather than refuse

* Status: Accepted
* Date: 2026-09-17
* Issue: [KEI-60](https://linear.app/kzhq/issue/KEI-60)
* Amends: [ADR-0001](0001-native-messaging-protocol.md), by adding two optional
  `download` request fields under its own additive-field rule

## Context

`src/output.rs::infer_filename` named a download after the last segment of the
media URL. For a direct file that is exactly right — `lecture-three.mp4` is a
better name than anything we could invent. For HLS it is useless: the
overwhelming majority of playlists are called `index.m3u8`, `playlist.m3u8` or
`master.m3u8`, so every stream on the web wanted the same three filenames.

`check_output_path` then refused to write over an existing file unless
`--overwrite` was given, and `extension/background.js::runDownload` hard-coded
`overwrite: false`. The two rules met in the defect: the **second** HLS download
a user ever started failed with "output already exists", from a different site,
about a different video, with no way to proceed from the popup at all — the
extension exposed no overwrite control, and overwriting was the wrong answer
anyway, because the existing file was someone else's download.

The material for a better name was already crossing the wire's doorstep.
`extension/content.js::scanMedia` has returned `document.title` and `sourceUrl`
since it was written; `background.js` simply never sent them.

## Decision

### The URL wins when it says something; the title fills the gap

Naming consults the caller's hints **only** when the URL-derived stem is
generic: `index`, `playlist`, `master`, `download`, `video`, `media`, or digits
only (`1080.m3u8` names a rendition, not a video). Anything else — a real
filename on a direct download — keeps naming itself exactly as it did, so this
change is invisible to the case that already worked.

When the stem is generic, the file is named after the sanitised page title, and
failing that after the source host, which at least distinguishes two sites whose
playlists are both `index.m3u8`. Title and host both go through the unchanged
`sanitize_filename`, so nothing the naming policy produces can escape the
destination directory or land on a Windows device name.

`video` earns its place on that list even though, unlike the others, it is a
plausible filename a site really chose. It was questioned and deliberately kept:
the path that matters is the extension, which always sends a title, and there
`…/video.mp4` becoming the page title is plainly the better name. The one case
it costs is the CLI without `--name`, where `video.mp4` becomes
`example.test.mp4` — a wash rather than a harm, and the minority path. If a
manual pass ever finds real page titles to be mostly site boilerplate, the whole
fallback order wants revisiting and this list should be reconsidered then, not
piecemeal now.

The rejected alternative was a naming *template* — `{title} - {host} - {date}`,
configurable. It is the obvious next request and it is deliberately not here:
KEI-60 scopes per-site templates out, and a template language is a much larger
contract to keep than a fallback chain.

### A title is user data, so its contribution is bounded

A page title can carry an account name, a customer name, or a document
reference. Naming a file after one puts that string in the user's Downloads
folder and in `job.path` inside the extension's `storage.local`. That is the
same exposure any filename has — the user sees their own filenames — but it is a
*new* path for page content to reach the disk, so it is bounded rather than
trusted: whitespace collapses, `sanitize_filename` applies unchanged, and the
result is cut to 80 characters on a character boundary.

What the bound deliberately does **not** do is redact. A filename is not a URL,
and `src/redact.rs` exists for FFmpeg's stderr (ADR-0003), not for names the
user chose to give their own pages. The rule that matters is the negative one,
and it is pinned by
`tests/native_host.rs::no_host_event_echoes_the_title_except_in_the_output_path`:
the title is naming material, never diagnostics. It appears in no `log`, no
`progress` and no `error` — only in the `path` of a `terminal` event, which is
the file the user is about to open.

### Rename by default for an inferred name, fail for an exact path

This reverses the `AGENTS.md` rule "Refuse output collisions unless
`--overwrite` is explicitly supplied", deliberately and only for names *we*
chose. The distinction is who named the file:

* An **inferred** name is ours. The user asked for a video, not for a path, so a
  collision is our problem to solve and ` (2)` solves it without destroying
  anything. This is the default for `--dir`, for a bare invocation, and for
  every extension download, because the native host never receives an exact
  path.
* An **exact** `--output` path is a place the user named. A collision there is a
  statement about their filesystem that we have no business resolving, so it
  stays an error, and `--overwrite` still means overwrite.

`--on-conflict fail|rename|overwrite` makes the choice explicit in both
directions, and a Settings select does the same for the extension.

### Rename reserves its choice

`reserve_unused_path` walks ` (2)`, ` (3)`, … and takes the first free name by
**creating it exclusively**, not by probing with `metadata` and then handing the
name to FFmpeg. One download runs per host process, so two downloads started
together are two processes racing for the same directory; a probe leaves a
window in which both pick ` (2)` and one silently overwrites the other — the
exact outcome this ADR exists to prevent.

A reservation is a placeholder, not output, so a download that fails without
using it **releases** it: `release_reservation` deletes the file and the next
attempt gets the name it expected rather than ` (2)`. Leaving it was considered
first, on the grounds that "preserve partial output and diagnostic files after
download failures" says not to clean up after a failure — but that rule protects
*FFmpeg's* bytes, and an empty file this process created to claim a name is not
among them. It is residue: a file the user never asked for, in their Downloads
folder, that also walks the name forward on every retry.

The two are told apart by the only signal that cannot be wrong about it: the
file is removed **only while it is still empty**, the state the reservation
created it in. The moment FFmpeg writes a byte the file stops being a
reservation and is kept, so a partial download survives its failure exactly as
before. A path that was not reserved — every `fail` and `overwrite` target — is
never touched at all, so an empty file that was already the user's stays put.

### The protocol gains two optional fields and stays at version 1

`download` accepts `title` (string) and `on_conflict`
(`fail` | `rename` | `overwrite`). Both are optional, no field is renamed or
removed, and no response changes, so this is exactly the case ADR-0001 and
`docs/protocol.md`'s *Optional fields* section already provide for: additive
fields without a version bump.

Bumping to 2 was considered and rejected. It buys one thing — an old host would
answer `unsupported_protocol_version` instead of quietly ignoring `title` and
naming everything `index.mp4`, which is the "stale release binary" failure
`AGENTS.md` calls the most common "it worked before" report. It costs a hard
lockstep upgrade for every user and, per `docs/protocol.md`, retires the
legacy-version-0 allowance that keeps pre-handshake extensions working. The
degraded behaviour of an old host is the *previous* behaviour, which is not
dangerous, only unhelpful; that is not worth a breaking change.

Two details follow from the fields being optional:

* **An absent `on_conflict` means `rename`, not `fail`.** This is an additive
  field that nonetheless changes behaviour for an unchanged client, which needs
  saying out loud. It is deliberate: the host only ever infers a name into
  `output_dir`, so `rename` is the correct default for every request it can
  receive, and an extension too old to send the field is precisely the buggy
  client this issue exists to fix. `overwrite: true` still maps to
  `OnConflict::Overwrite`, so nothing that asked to replace a file stops doing
  so.
* **An unrecognised `on_conflict` fails the frame** with
  `rejected` / `invalid_request` rather than being ignored. "Ignore fields you
  do not recognise" is the rule for unknown *fields*; a known field with an
  unreadable *value* is different, because the value decides whether a file
  survives. Guessing there is the one mistake that costs a user data.

`overwrite` stays on the wire, documented as superseded. `on_conflict` wins when
both are present. The extension keeps sending `overwrite: false` so that an
older host — one that understands neither field — keeps its strict behaviour
rather than being talked into replacing a file.

## Consequences

* The defect is closed: two pages whose playlists are both `index.m3u8` produce
  two differently named files, and the same page downloaded twice produces
  `Name.mp4` and `Name (2).mp4`.
* `AGENTS.md`'s collision rule changes with this ADR rather than going stale.
* Downloads that used to be named `video.mp4` from a `…/video.mp4` URL are now
  named after the page title or the source host, because `video` is a generic
  stem. This is intended, and it is why two lifecycle tests in
  `tests/native_host.rs` moved to a distinctive URL: they are about the event
  sequence, and naming is covered on its own.
* A failed download leaves nothing behind, and a retry gets the same name it
  would have had the first time. Partial output is unaffected: the release is
  conditional on the file still being empty.

## Unverified

* No part of this has run in a real Firefox. The popup→background→port path is
  covered by `tests/extension/output-naming.test.js` in `jsdom` with a stubbed
  `browser` and a fake native port, which is not the same as a real
  `runtime.connectNative`. Whether `document.title` on a real media page is
  worth naming a file after — as opposed to being a site's boilerplate — is a
  judgement only a manual pass can make.
* The naming and rename paths were exercised against fake FFmpeg executables
  and against the real FFmpeg 6.1.1 available here for direct files. They have
  **not** been exercised against a real HLS download, because FFmpeg below 7.1
  cannot run this project's HLS path at all (KEI-81). The naming decision is
  made before FFmpeg is invoked and is independent of the demuxer, but the
  end-to-end statement "a real `index.m3u8` download lands at the title-derived
  name" is untested here.
* The ` (2)` reservation race is argued, not demonstrated: no test starts two
  host processes against one directory simultaneously.
* Releasing a reservation is a delete, and deletes deserve suspicion. The
  narrowness of it — reserved by this process, in this run, still zero bytes —
  is pinned from both sides: `releasing_never_deletes_output_ffmpeg_actually_wrote`
  and `a_partially_written_download_is_still_preserved` fail if the emptiness
  check is dropped, and `releasing_never_touches_a_path_we_did_not_reserve`
  covers a target we did not create.

  There is no test for another process writing into our reservation between the
  failure and the release, and that is a deliberate omission rather than a gap
  left open. `release_reservation` reads the file's length **fresh at release
  time**, not from when the reservation was made, so bytes written by anyone at
  any point before that read are seen and the file is kept. Another *Downer*
  process cannot be the writer either: it would have to choose the same path,
  and `create_new` fails on a path that already exists, so it takes ` (2)`
  instead. What remains is the window between that `metadata` call and
  `remove_file` — microseconds, requiring a non-Downer process to write into a
  path Downer created moments earlier. Covering it would mean injecting a seam
  or a sleep into the release path: production complexity for a sequence this
  program cannot itself produce. Read this as a bounded risk that was measured,
  not one that was skipped.
