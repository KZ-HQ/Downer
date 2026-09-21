# ADR-0021: Windows is unsupported, and the crate refuses to build there

* Status: Accepted
* Date: 2026-09-21
* Issue: [KEI-67](https://linear.app/kzhq/issue/KEI-67)
* Follows: [ADR-0012](0012-control-semantics.md), whose promises are the reason
  this is a build-time refusal rather than a runtime one;
  [ADR-0008](0008-relocatable-native-host-installation.md), whose registration
  mechanism is the second thing Windows lacks

## Context

Downer had never been built for Windows, and had never said so. The code was
full of the shape of the decision without the decision itself:

* `src/ffmpeg.rs` implemented pause and resume as `libc::kill` with `SIGSTOP`
  and `SIGCONT` under `#[cfg(unix)]`, and carried a `#[cfg(not(unix))]` twin
  that returned `Err("pause and resume are supported only on Unix platforms")`.
* `src/host.rs::manifest_dir` resolved Firefox's per-user native messaging
  directory for macOS and Linux and returned an error for anything else — its
  own comment already said "KEI-67 records that decision", for a record that did
  not exist.
* `src/host.rs` wrote the launcher Firefox executes as a `/bin/sh` script, and
  set its mode bits through `PermissionsExt` under `#[cfg(unix)]`, with a
  `#[cfg(not(unix))]` twin that did nothing and returned `Ok`.
* `tests/cli.rs`, `tests/host_install.rs`, `tests/ffmpeg_argv.rs` and
  `tests/hls_variant.rs` were `#![cfg(unix)]` in their entirety.

So a Windows build would have *compiled*. It would then have installed a
launcher Firefox could not find, because Windows registers native messaging
hosts under `HKCU\Software\Mozilla\NativeMessagingHosts` rather than in a
directory, and it would have answered every pause with an error string. Both
failures arrive at run time, in front of a user, with nothing the user can do
about either.

Doing nothing was not an option because the first real release (KEI-93) puts
artifacts on a page where someone can try to install them. "Which platforms is
this for?" becomes a question with consequences the moment anything is
published, and a build that succeeds is an implicit answer.

## What was measured

Nothing was measured. No Windows machine was involved in this decision, and it
would not have changed it: the question was not whether a Windows port *could*
be made to work but whether one would be maintained, and the owner decided on
2026-09-16 that it would not.

What the decision rests on instead is the code above, which is a measurement of
a sort — four files and four test suites already assume Unix, and none of them
was written for this record.

## Decision

### Windows is not a supported platform

Not in the artifacts, not in CI, not in the documentation. `README.md`,
`docs/user-guide.md` and `docs/architecture.md` already said so in prose; this
record is what they now point at.

### The crate refuses to build on a non-Unix target

`src/lib.rs` carries a `compile_error!` under `#[cfg(not(unix))]`, naming both
reasons and this record.

The alternative — build, and degrade like the existing `#[cfg(not(unix))]`
stubs did — was rejected because of what is being degraded.
[ADR-0012](0012-control-semantics.md) makes pause, resume and cancel
*promises*, written down as a contract the extension's controls rely on. A
build that keeps the promise's signature and returns an error from its body has
not degraded the feature; it has made the contract untrue while leaving every
appearance that it holds. That failure is invisible until a user presses Pause,
and the remedy at that point is "use a different operating system", which is not
a remedy.

A compile-time refusal moves the same sentence to the one moment it is useful —
in front of whoever is building it, who is the only person positioned to act on
it — and costs a user nothing, because there was never going to be a working
binary at the end of that build.

### A Unix that is neither macOS nor Linux builds, and degrades at run time

The gate is `cfg(not(unix))`, not `cfg(windows)`, and the difference is the
decision rather than an implementation detail.

On FreeBSD or illumos every mechanism above still exists: `SIGSTOP` and
`SIGCONT`, `/bin/sh`, Unix permission bits. What is unknown there is only *where
Firefox keeps its native messaging manifests* — one directory path. The CLI is
entirely functional; a user who never installs the extension would never notice.
Refusing to build for a platform whose only defect is an unconfirmed directory
would throw away a working program to make a point.

So the two failure modes get the two different answers they deserve:

| | What is wrong | Answer |
| --- | --- | --- |
| Windows, and any other non-Unix | The process model. Pause and resume cannot exist. | Refuse to compile |
| A Unix that is not macOS or Linux | One directory path. Everything else works. | Build; refuse the registration at run time, by name |

### The unsupported-platform refusal is a value, not a `cfg`

`host::Platform` is an enum with `MacOs`, `Linux` and `Unsupported(&str)`, and
`manifest_dir` and the new `platform` diagnostic check both take one as an
argument rather than consulting `cfg!`.

This follows from the refusal above. Once Windows cannot compile, a `cfg`-gated
refusal message is unreachable from every build that exists, so no test can
reach it either — the acceptance criterion "installer and doctor report
unsupported platform" would have been satisfiable only by inspection. Passing
the platform in makes the refusal ordinary code with ordinary tests, asserted
from macOS or Linux against `Platform::Unsupported("windows")`.

### What was rejected and why it lost

**Degrade on Windows like everywhere else.** Above: it makes ADR-0012 untrue
while appearing to hold.

**Gate on `cfg(windows)`.** Narrower than the problem. The problem is the
process model, and every non-Unix target lacks it; naming one of them would
leave the rest compiling into the same broken binary.

**A `DownerError::UnsupportedPlatform` with its own exit code.**
[ADR-0018](0018-stable-cli-exit-codes.md) makes the exit codes a public
interface and adding one a breaking change. No script could usefully act
differently on it than on the existing code 1 — there is no remedy to automate —
so it would have spent contract surface to say nothing. The refusal stays a
`DownerError::Host`.

**Keep the `#[cfg(not(unix))]` stubs as documentation.** They are deleted. A
stub that answers for a platform we refuse is exactly the half-support this
record rejects, and leaving it means a future port begins by deleting one
`compile_error!` and getting a binary that builds and silently does not work.
With the stubs gone, that port begins by writing the implementations, which is
where it should begin.

## Consequences

* `cargo check --target x86_64-pc-windows-msvc` now fails with the message
  above. This is the intended behaviour, not a regression: the answer to "does
  this build on Windows?" is the sentence, not a binary.
* `src/ffmpeg.rs` has one `signal_process` and one `signal_if_running` rather
  than two of each, and `mod unix_signal` is no longer duplicated. The
  `#[cfg(unix)]` on `pause_and_resume_can_be_queued_before_ffmpeg_starts` is
  gone, so the test runs unconditionally.
* `src/native.rs` advertises `capabilities.pause_resume` as `true` rather than
  `cfg!(unix)`. The field stays on the wire: it is the protocol's, and a future
  platform or a future scheduler could still make it false.
* `downer doctor` and the `status` response gain a `platform` check, first in
  the list because on an unsupported platform "FFmpeg is fine" is true and
  useless. The `platform` field itself is unchanged and still reports `macos`,
  `linux` or `unsupported`.
* `Cargo.toml` keeps `libc` under `[target.'cfg(unix)'.dependencies]`. The
  target gate is now redundant with the `compile_error!`, but it is still a true
  statement about where `libc` is needed, and moving it would touch
  `Cargo.lock` for no behavioural gain.
* A contributor on a non-Unix machine cannot build or test Downer at all, not
  even the parts that have nothing to do with downloading — the scraper, the
  redaction rules, the output naming. That is a real cost, and it is accepted:
  the population it affects is the population that was never going to get a
  working product anyway.

## Revisiting

The trigger is a maintainer who uses Windows. Not a user request, and not a
contribution offering a port — the cost being avoided here is maintenance, and
neither of those supplies it. Concretely, a port would need:

1. A process-suspension mechanism that is not a signal, and a decision about
   whether ADR-0012's promises survive the substitution.
2. Registry-based native host registration in `src/host.rs`, replacing the
   launcher script and the manifest directory.
3. A Windows CI job, because an unexercised platform regresses silently.
4. An amendment to this record, and to ADR-0012 if item 1 changes what pause
   promises.

Until then, the refusal stands.

## Unverified

* **That Windows lacks a usable equivalent of `SIGSTOP`.** Windows has no
  process-stop signal, which is why the existing stub returned an error, but
  process suspension is achievable by other means — suspending every thread, or
  the undocumented `NtSuspendProcess`. Whether either would satisfy ADR-0012's
  promises about what a paused download guarantees is untested and unargued
  here. The decision does not depend on the answer: it rests on nobody
  maintaining the port, not on it being impossible.
* **That Firefox on Windows registers native hosts only through the registry.**
  Taken from Mozilla's documentation, not from a Windows Firefox.
* **The behaviour of the degrading branch on a real FreeBSD or illumos.** The
  claim that everything but the manifest directory works there is an inference
  from what the code uses — `libc::kill`, `/bin/sh`, `PermissionsExt` — and has
  never been run. `Platform::Unsupported` is asserted in tests by being named,
  which proves the message, not the platform.
