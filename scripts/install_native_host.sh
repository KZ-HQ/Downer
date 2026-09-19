#!/bin/sh
# Register the *development* build as Firefox's native messaging host.
#
# This is a thin wrapper now: `downer install-host` does the work, and it lives
# in the binary so a user who installed Downer without the repository can run it
# too (see docs/adr/0006-relocatable-native-host-installation.md). `--dev`
# registers the freshly built binary where it sits in `target/release`, which is
# what a checkout wants — reinstalling after `cargo build --release` is not
# needed — and labels the manifest so an unexpected registration is easy to
# recognise. A real installation uses `downer install-host` with no flags and
# keeps working when the checkout is gone.
set -eu

repo_dir=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
cargo build --release --manifest-path "$repo_dir/Cargo.toml"
exec "$repo_dir/target/release/downer" install-host --dev "$@"
