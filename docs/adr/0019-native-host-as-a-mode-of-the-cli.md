# ADR-0019: The native host is a mode of the CLI binary, not a second program

* Status: Accepted
* Date: 2026-09-20
* Issue: [KEI-63](https://linear.app/kzhq/issue/KEI-63)
* Builds on: [ADR-0001](0001-native-messaging-protocol.md), the wire contract
  this mode speaks, and [ADR-0008](0008-relocatable-native-host-installation.md),
  which decides where the binary lives and how `--native-host` is supplied

## Context

`src/main.rs` scans `std::env::args()` for `--native-host` **before** clap runs,
and on finding it hands the process to `downer::native::run_stdio()` and never
returns to the CLI. One binary, two entry points.

Two records already touch this and neither decides it. ADR-0001 fixes the
protocol and refers throughout to `downer --native-host`. ADR-0008 decides that
`install-host` registers a generated launcher which supplies that flag, and
explicitly rejects the alternative of having `main` infer host mode from the
shape of Firefox's argv. Both take the single binary as given — the question
"should the host be a separate program?" is answered nowhere, and it is a real
fork: `Cargo.toml` could declare a second `[[bin]]` tomorrow and nothing written
down would object.

The pre-parse is also the kind of code that looks like a mistake. It bypasses
the argument parser the rest of the CLI is built on, and a contributor tidying
it into a clap subcommand would break every installed add-on, because Firefox
appends its own arguments — the manifest path and the extension ID — to whatever
the launcher runs. The comment at `src/main.rs:6` says so; this record says why
the situation it describes exists at all.

## Decision

**One binary, entered as a host by `--native-host` before argument parsing.**

The argument that settles it is version agreement. ADR-0001 exists because the
extension and the host drift — a stale `target/release/downer` is this
project's most common "it worked before" report — and its remedy is a handshake
in which the host states its protocol version. That remedy is only as good as
the answer to "which host?". With one binary there is one answer: the `downer`
the user installed is the `downer` Firefox launches, so `cargo install`,
a released tarball and `make extension-install` cannot leave a CLI from one
version beside a host from another. A second binary would add a drift axis
*inside* the project, which is the very failure ADR-0001 was written to detect
between the project and the browser.

Everything else follows cheaply from that, and each piece would have to be built
twice if the host were separate:

* **Installation.** ADR-0008's `install-host` copies **the running binary** to a
  durable location and registers the copy. That is only coherent because the
  running binary is also the host. Split them, and the CLI has to locate, carry
  or download a host it is not.
* **Release.** `scripts/release_artifacts.sh` ships one executable per platform.
  Two would mean two, in every tarball, in every checksum, in every install
  path.
* **Code.** The host is not a thin wrapper — it shares the scraper, the FFmpeg
  layer, the output naming rules of ADR-0004 and ADR-0005, redaction, and
  diagnostics. Those live in `src/lib.rs` and both entry points use them. A
  separate binary in this workspace would share them through the same library,
  so the split would buy nothing at the level where the code actually is.

### Why the flag is read before clap, and must stay that way

Firefox runs the manifest's `path` with arguments of its own choosing. Those
arguments are Firefox's convention, not ours, and ADR-0008 declined to make the
entry point depend on them. `--native-host` is therefore our own marker, written
by a launcher we generate, and the pre-parse looks only for that one string and
ignores everything beside it.

clap cannot do this job. `AGENTS.md` fixes the CLI's interface as
`downer URL [options]`, so a URL is required — and Firefox's invocation supplies
no URL, plus two positional arguments clap would reject. The process would exit
on a usage error before the host ever read a byte from stdin, and the symptom in
the popup would be "native host disconnected".

### Why not a second binary anyway

It would buy separable concerns and a smaller host, and it costs the version
guarantee above. The size argument is weak here: the shared code *is* most of
the binary, so two would together be larger than one. The separation argument is
weaker still — the boundary already exists, as `src/native.rs` against
`src/cli.rs`, and a module boundary is where it does the work.

## Consequences

* The CLI version **is** the host version. `downer --version` answers for both,
  which is what makes ADR-0001's handshake a check on the browser boundary
  rather than on an internal one.
* `--native-host` is effectively reserved. It cannot become a normal CLI flag,
  and a URL that happened to contain it as a whole argument would enter host
  mode — not reachable in practice, since it must be an exact argument.
* The host inherits the CLI's dependencies whether it needs them or not, which
  is the price of one binary and is small: ADR-0013 measured the host's idle
  footprint at ~3.5 MB.
* Anyone reading `src/main.rs` sees a parser bypass before they see the reason.
  The comment there, ADR-0008 and this record are the three places that explain
  it; none of them should be removed without the others.
* Making the host a separate program later is possible but is not a refactor:
  it would need its own version negotiation against the CLI, a second install
  path, and an amendment to this record and to ADR-0008.

## Unverified

* **Reconstructed, not chosen afresh.** The single binary has been true since
  the host existed. This record states the reasons the code and the surrounding
  ADRs make load-bearing; no one weighed the two-binary option at the time and
  wrote it down, so it is possible the original reason was simply that it was
  easier.
* **No measurement of what a split would cost.** The binary-size claim is an
  inference from the shared library, not from building a second binary and
  comparing.
