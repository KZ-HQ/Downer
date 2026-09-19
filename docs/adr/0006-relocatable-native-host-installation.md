# ADR-0006: Install the native host from the binary, not from the checkout

* Status: Accepted
* Date: 2026-09-19
* Issue: [KEI-57](https://linear.app/kzhq/issue/KEI-57)

## Context

Firefox reaches the downloader through a native messaging manifest: a JSON file
in a per-user directory naming a program Firefox may launch, plus the extension
IDs allowed to talk to it. The first implementation wrote that manifest from
`scripts/install_native_host.sh`, pointing it at `scripts/native-host.sh`
**inside the repository checkout**, which in turn ran
`<checkout>/target/release/downer --native-host`.

Everything about that registration was tied to the checkout. Moving it,
deleting it, or running `cargo clean` left a manifest Firefox still found and a
program that no longer existed, and the only symptom was "native host
disconnected" in the popup. There was no way to install from a released binary
or from `cargo install`, and no way to undo an installation at all.

Two smaller problems came with it. The extension ID was spelled out in a
`printf` format string, with nothing keeping it equal to
`extension/manifest.json`. And FFmpeg discovery had only `DOWNER_FFMPEG` as an
escape hatch, which a user cannot use: Firefox launches the host itself, with a
minimal environment nobody gets to edit.

## Decision

### Installation lives in the binary

`downer install-host` and `downer uninstall-host` are subcommands of the
program being installed, so they work from a release artifact or
`cargo install` with no repository present. They are clap subcommands with
`args_conflicts_with_subcommands` and `subcommand_negates_reqs`, so
`downer URL [options]` — the interface AGENTS.md fixes — is unchanged and the
URL stays required for it.

`scripts/install_native_host.sh` remains as a development wrapper: it builds
the release binary and runs `downer install-host --dev`. `scripts/native-host.sh`
is deleted; the launcher is now generated.

### The registered program is a generated launcher

Firefox invokes the manifest's `path` with its own arguments — the manifest
path and the extension ID — and never with `--native-host`. Something must
supply that flag. The alternative was to have `main` infer host mode from the
shape of Firefox's argv, which makes the entry point depend on an undocumented
argument convention that Firefox is free to change.

So the installer writes a two-line `/bin/sh` launcher next to the binary and
points the manifest at it. The host is still entered through exactly the
command line ADR-0001 documents, and the indirection is visible in a file
anyone can read.

### A copy, unless the binary is already somewhere durable

`install-host` copies the running binary to `<data dir>/downer/bin/downer` and
registers the copy, because the common case is a binary in `target/release` or
an unpacked download, and both are transient. It registers in place when the
binary already is that copy or lives in the user's Cargo or executable bin
directory — a second copy there would just go stale — and `--link` asks for
that explicitly.

`--dev` implies `--link` and labels the manifest's description, so a developer's
rebuild is picked up with no reinstall and an unexpected registration is easy
to recognise later.

### An FFmpeg path recorded at install time

`downer install-host --ffmpeg PATH` records an absolute path in a host config
file (`<config dir>/downer/config.json`). Discovery order becomes
`DOWNER_FFMPEG`, then the config file, then the Homebrew locations, then
`ffmpeg` on whatever PATH the browser passed down. The environment variable
keeps winning because every test and script already uses it.

JSON, not TOML: the crate already speaks JSON on the wire and in the manifest,
and a two-field config file does not justify a dependency or a hand-rolled
parser. A bare name (`--ffmpeg ffmpeg`) is refused, because resolving it would
need the PATH that the config file exists to work around, and a path that does
not exist is refused too, since writing it down would surface much later as an
FFmpeg failure.

### One extension ID

`downer::host::EXTENSION_ID` is the only place the ID is written in Rust, and
`tests/host_install.rs` reads `extension/manifest.json` and fails if they
differ — the same tactic `scripts/check_versions.py` uses for the version.

## Consequences

* A user installs with `cargo install --path .` (or a released binary) followed
  by `downer install-host`, and can delete the checkout.
* Uninstall exists and is idempotent: manifest, launcher and config, plus the
  copied binary with `--binary`. Removing something already gone is not an
  error.
* A developer's loop is unchanged: `make extension` still registers the build
  it just produced.
* Two copies of the binary can exist (the build and the installed copy). That
  is the price of a registration that survives `cargo clean`; `--dev` and
  `--link` opt out where it matters.
* Windows stays refused, now in Rust rather than in a shell script. KEI-67
  records the platform decision itself.
* What is tested is the manifest, the launcher, the copy, the config, and that
  the launcher really starts the host protocol. The end of the acceptance
  criterion — a real Firefox connecting on a clean account — still needs a
  human with a Firefox profile, as `tests/e2e/native-download.test.mjs` needs
  one today.
