#!/bin/sh
# Build one platform's release artifacts into a staging directory.
#
# `.github/workflows/release.yml` is a thin wrapper around this script, on
# purpose: a workflow step that exists only as YAML cannot be run or debugged
# from a laptop, and this repository's GitHub job logs are not always readable
# after the fact. Everything a release does that could fail for a reason other
# than "GitHub did not run it" therefore lives here (KEI-58).
#
# Produces, for the platform it runs on:
#
#   downer-<version>-<target>.tar.gz   the release binary, README and LICENSE
#   downer-<version>.xpi               the extension, byte-identical anywhere
#   SHA256SUMS.<target>                checksums for both of the above
#
# The XPI is reproducible (see scripts/package_extension.sh), so every platform
# building a release produces the same one; release_collect.py checks that they
# agree rather than trusting it. The binary is not reproducible and is not
# claimed to be.
set -eu

usage() {
    echo "usage: $0 --out DIR [--tag vX.Y.Z] [--skip-build]" >&2
    exit 2
}

out=""
tag=""
skip_build=""
while [ $# -gt 0 ]; do
    case $1 in
        --out) [ $# -ge 2 ] || usage; out=$2; shift 2 ;;
        --tag) [ $# -ge 2 ] || usage; tag=$2; shift 2 ;;
        --skip-build) skip_build=yes; shift ;;
        -h|--help) usage ;;
        *) echo "$0: unknown argument $1" >&2; usage ;;
    esac
done
[ -n "$out" ] || usage

repo_dir=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
cd "$repo_dir"

# Checksums: GNU coreutils and macOS disagree on the command but not on the
# format, so either output verifies with either tool's `-c`.
sha256() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$@"
    else
        shasum -a 256 "$@"
    fi
}

# The version gate runs first and with the tag when there is one, so a
# mismatched tag fails before anything is built.
if [ -n "$tag" ]; then
    python3 scripts/check_versions.py --tag "$tag"
else
    python3 scripts/check_versions.py
fi
version=$(python3 scripts/check_versions.py --print)
target=$(rustc -vV | sed -n 's/^host: //p')
[ -n "$target" ] || { echo "error: rustc did not report a host target" >&2; exit 1; }

echo "Staging downer $version for $target"

if [ -z "$skip_build" ]; then
    cargo build --release --locked
fi
[ -x target/release/downer ] || {
    echo "error: target/release/downer is missing; run without --skip-build" >&2
    exit 1
}

case $out in
    /*) out_dir=$out ;;
    *) out_dir=$repo_dir/$out ;;
esac
rm -rf "$out_dir"
mkdir -p "$out_dir"

# A tarball rather than a bare binary: it carries the licence and the README
# next to the thing they describe, and it survives a download without losing
# the executable bit the way a bare file from a browser can.
bundle="downer-$version-$target"
staging=$(mktemp -d)
trap 'rm -rf "$staging"' EXIT INT TERM
mkdir "$staging/$bundle"
cp target/release/downer "$staging/$bundle/downer"
cp README.md LICENSE "$staging/$bundle/"
(cd "$staging" && tar czf "$out_dir/$bundle.tar.gz" "$bundle")

./scripts/package_extension.sh "$out_dir/downer-$version.xpi"

(cd "$out_dir" && sha256 "$bundle.tar.gz" "downer-$version.xpi" > "SHA256SUMS.$target")

echo "Staged in ${out_dir#"$repo_dir"/}:"
ls -1 "$out_dir"
