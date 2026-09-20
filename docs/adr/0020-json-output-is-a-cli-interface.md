# ADR-0020: `--json` output is a public interface, versioned in the document

* Status: Accepted
* Date: 2026-09-20
* Issue: [KEI-62](https://linear.app/kzhq/issue/KEI-62)
* Follows: [ADR-0018](0018-stable-cli-exit-codes.md), which made the exit codes
  a contract for the same reason

## Context

KEI-62 adds `--json`: a machine-readable form of `--list` and of a download's
result. That is a second surface a script reads, alongside the exit codes
ADR-0018 has just pinned.

The issue is silent on whether the JSON is a contract or convenience output,
and silence is the problem. A `--json` nobody promised is a `--json` a
contributor reshapes while tidying a struct — renaming `elapsed_ms`, folding
`bytes` into a nested object, dropping a key that looked redundant. The
breakage is invisible from inside the repository: `make check` passes, and a
user's pipeline starts producing empty strings.

This is precisely the gap ADR-0018 closed for the exit codes, three days
earlier and for the same reason: *"a code nobody has promised is a code a
contributor renumbers while tidying an enum, and the breakage is silent."*
The argument does not get weaker because the surface is JSON instead of an
integer. If anything it gets stronger, because a JSON document has more parts
to rename.

## Decision

**The `--json` documents are a public interface of the CLI. Their field names
and types are stable, and an incompatible change is a breaking change.**

Three parts.

### A `schema_version` in every document

An integer, currently `1`, in `src/discovery.rs`. Adding a field is
compatible and does not bump it; renaming one, removing one, or changing a
type is incompatible and does.

The name and the idea are the native protocol's `protocol_version`, because
they answer the same question for their own surface. A reader who knows one
knows the other.

### Enforced by exact-key tests, not by a shared fixture

`tests/cli.rs` asserts the **whole key set** of each document — sorted, so the
assertion is the full shape rather than the fields the author happened to think
of — and the values that carry meaning. Adding a key fails the test, which is
the point: the change is then deliberate.

**The `tests/fixtures/protocol.json` mechanism was considered and rejected.**
That file exists because the native messaging vocabulary is implemented
**twice, in two languages**, and neither side may rename a term unilaterally
(ADR-0011 put the media-extension list under the same arrangement for the same
reason). `--json` has one implementation, in Rust, and no second side to drift
from. A fixture there would be ceremony that looks like single-sourcing while
single-sourcing nothing: the file and the struct would both live in this
repository, both be edited by the same person in the same commit, and agree by
discipline rather than by construction. The exact-key test catches what the
fixture would, without implying a second implementation exists.

The one piece of genuinely shared vocabulary *is* pinned that way:
`kind` is `hls`/`dash`/`file`, the words the extension's candidates already
carry, and `MediaKind::as_str` is asserted against `media-extensions.json`'s
`kinds` array in `scraper`'s own tests.

### Borrowed vocabulary, and one field deliberately absent

The rendition fields — `url`, `bandwidth`, `width`, `height`, `codecs`,
`audio_url`, `default` — are `RenditionInfo`'s in `src/native.rs`, unchanged.
The popup and a shell script are looking at the same thing and two names for it
would be two things to keep in step.

**`confidence` is not reported.** The extension ranks a candidate `observed`
(a `performance` entry), `declared` (a DOM attribute) or `inferred` (a regex
hit on markup). The CLI cannot make that distinction: it fetches HTML and runs
the markup pass, so every candidate would carry the same value. A constant
field that looks like a ranking is worse than an absent one, so it is absent
until there is something to put in it.

### Failures are not JSON

A failed run writes its message to stderr and exits with its ADR-0018 code, as
it always has, whether or not `--json` was passed.

The rejected alternative is an `{"error": ...}` document on stdout. It loses on
ADR-0018's own argument: a failure **already has** a machine-readable form that
this project has just promised to keep stable, and the whole reason the codes
classify by *what the user must do next* is so a script does not have to parse
English. Adding a second failure contract would create two things to keep in
step, and the new one would be the less useful of the two.

## Consequences

* `tests/cli.rs` is the enforcement, as it is for ADR-0018. Renaming a field
  breaks the suite rather than a user's pipeline.
* Adding a field stays cheap and needs no version bump, so the surface can grow
  — `engine` is already there for a downloader that does not exist yet
  (KEI-71's native HLS engine), reported as the constant `"ffmpeg"` that
  ADR-0015 makes it today, so a script reading it needs no change when that
  lands.
* `--json` implies quiet: the prose a download prints would otherwise make
  stdout unparseable. FFmpeg's own progress is unaffected, being on stderr
  under `Reporting::Cli`.
* An absent key is meaningful and a consumer must handle it. `bytes` is absent
  when the finished file could not be stat'd, and `renditions` is absent for
  anything that is not a master playlist — an empty array there would claim the
  choice existed and was empty. `experimental` is the deliberate exception: it
  is always present, because a boolean a consumer can read unconditionally is
  worth more than the bytes it saves.
* The promise costs a test and a constant. That is the same bargain ADR-0018
  struck, and the same one it judged worth making.

## Unverified

* **No user of this output is known**, exactly as ADR-0018 says of the exit
  codes. The case rests on the shape of the tool — a downloader called from
  scripts, which is what `--json` is *for* — rather than on an observed
  consumer. Nothing is lost if none appears.
* **The compatibility rule is asserted, not exercised.** No field has been
  added since `schema_version` was introduced, so "adding a key does not bump
  it" has never actually been done.
* **`bytes` has only been observed for files a fake FFmpeg wrote.** Whether a
  real download's byte count is ever read before the file is fully flushed is
  untested; the call happens after FFmpeg exits, so the shape is right, but no
  large real download has been measured against it.
