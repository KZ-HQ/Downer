# ADR-0009: Setup checks are a separate `status` command, shared with `downer doctor`

* Status: Accepted
* Date: 2026-09-19
* Issue: [KEI-59](https://linear.app/kzhq/issue/KEI-59)

## Context

Until now a broken installation announced itself only after the user pressed
Download, as `native host disconnected` (Firefox's wording for "no such native
application") or an FFmpeg error string. Neither says which FFmpeg was chosen,
whether the host is registered, or whether the download directory can be
written to. `src/native.rs::ffmpeg_path` picks a binary and tells nobody which.

KEI-59 asked for a `status` command "or fold into the protocol `hello`", a
Settings panel, and a `downer doctor`. Two other issues wanted a piece of the
same payload: KEI-85 (the FFmpeg version, for Settings) and KEI-65
(`capabilities.pause_resume`).

## Decision

**The checks live in Rust, in `src/diagnostics.rs`, and both surfaces render the
same structure.** `status` and `downer doctor` call one function; neither
decides what a failure means. A check is `{name, title, outcome, detail,
remedy}`, and the caller lays it out.

**`status` is a separate command, not extra fields on `hello`.** The protocol
already states that the handshake "does no I/O", and says why: a host process is
started per download, so the handshake is on the path of every download. These
checks are all I/O — a process spawn for `ffmpeg -version`, and a file written
and removed to test the download directory. Putting them in `hello` would charge
every download for a diagnostic nobody asked for. `status` runs when a user
presses a button.

Compatibility did not decide this. The protocol documents that optional fields
may be added without a version bump (`title` and `on_conflict` arrived that
way), and a new command is equally additive. The protocol version is unchanged.

**Three outcomes, not two.** `pass`, `warn`, `fail`. An FFmpeg below the
supported minimum is the case that forces this: it still downloads (ADR-0006),
so `fail` would be a lie, and it needs saying, so `pass` would be too. `downer
doctor` exits non-zero only on `fail` — a warning that broke a script would make
the command unusable in one.

**A failure always carries a remedy.** `remedy` is present whenever `outcome` is
not `pass`. A diagnostic the user cannot act on is the thing this issue exists
to remove, so the type makes the omission visible rather than optional by habit.

**`detail` is present even on a pass**, because "which FFmpeg is it actually
using?" is a question people ask when nothing is broken.

**The unreachable case is the extension's to report.** When the host is not
registered there is no host to ask, so the Settings panel renders that itself as
a failed `host_connection` check and maps Firefox's `lastError` wording to
instructions. It is the one check whose answer cannot come from `status`.

**An `ffmpeg` request field** on `download` and `status` carries the Settings
page's FFmpeg path. Firefox starts the host with a minimal environment, so
neither `PATH` nor `DOWNER_FFMPEG` can reach it. This does not widen who may run
what: the host already runs an FFmpeg named by its own config file (ADR-0008),
and only the pinned extension ID can open the port at all.

## Consequences

* KEI-85 is subsumed: the FFmpeg version reaches Settings through `status`,
  with no separate protocol change and no second ADR.
* KEI-65's capability bullet needed nothing — `hello` has carried
  `capabilities.pause_resume` since ADR-0001. Only its UI half remains.
* The directory check writes a file. It is uniquely named, removed immediately,
  and a unit test asserts nothing survives: a diagnostic that litters the user's
  Downloads folder would be its own bug.
* Adding a check means adding it in one place. The wire vocabulary
  (`check_names`, `check_outcomes`) is pinned in `tests/fixtures/protocol.json`.
* `downer doctor` gains exit code 6, distinct from 4 (FFmpeg unavailable),
  because a setup failure is not always FFmpeg's.

## Unverified

* **The Settings panel has not been seen in Firefox.** Its logic is covered in
  jsdom and the host side end to end, but no automated test renders it in a real
  browser, and KEI-59's acceptance criterion asks for manual confirmation of
  four scenarios: host not installed, FFmpeg missing, protocol version mismatch,
  and an unwritable directory.
* **`log_path` is not reported**, though KEI-59's scope lists it. There is no
  host log file yet — KEI-64 creates one. A field that always says "none" would
  be noise, so it is left out until there is something to point at.
* The protocol-version-mismatch path reuses the existing handshake check rather
  than being a check of its own, so it is reported as a connection failure
  rather than as a named check.
