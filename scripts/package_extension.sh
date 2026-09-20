#!/bin/sh
# Package extension/ as a zip archive at the given path (a .zip or an .xpi).
#
# An .xpi is a zip with a different extension, so one packaging rule serves
# both `make extension-package` and `make extension-xpi`.
#
# The archive is built to be reproducible: the same commit packaged on two
# machines gives the same bytes, so the SHA256SUMS published with a release can
# be checked by rebuilding instead of being a number only the release workflow
# can produce (KEI-58). Two things would otherwise vary. File modification
# times are recorded in a zip and differ between a laptop and a fresh CI
# checkout, so the staged copy's timestamps are normalised to the earliest a
# zip can store. Directory read order decides member order and is filesystem
# dependent, so the member list is sorted in the C locale rather than left to
# `zip -r`.
set -eu

if [ $# -ne 1 ]; then
    echo "usage: $0 OUTPUT" >&2
    exit 2
fi

output=$1
repo_dir=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
case $output in
    /*) ;;
    *) output=$repo_dir/$output ;;
esac

staging=$(mktemp -d)
trap 'rm -rf "$staging"' EXIT INT TERM

cp -R "$repo_dir/extension/." "$staging/"
# Finder metadata is not part of the extension and would differ between
# machines packaging the same commit.
find "$staging" -name .DS_Store -delete
# The DOS timestamp a zip stores cannot predate 1980, so that is the floor.
find "$staging" -exec touch -t 198001010000 {} +

mkdir -p "$(dirname "$output")"
rm -f "$output"
(
    cd "$staging"
    find . -type f | sed 's|^\./||' | LC_ALL=C sort | zip -qX "$output" -@
)

echo "Created ${output#"$repo_dir"/}"
