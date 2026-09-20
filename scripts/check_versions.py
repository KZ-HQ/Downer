#!/usr/bin/env python3
"""Verify that the Cargo package, the Firefox extension, and a release tag agree.

The versioning rule is documented in AGENTS.md: `version` in Cargo.toml and
`version` in extension/manifest.json are the source of truth for the product
version and are always bumped together. This script is run by `make check`,
and therefore by CI.

With `--tag vX.Y.Z` it additionally requires the tag to name that same version,
which is what `.github/workflows/release.yml` runs before it builds anything.
The rule lives here rather than in the workflow so it can be run locally, and
so there is one implementation of "these three must agree" (KEI-58).
"""

import argparse
import json
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
CARGO_TOML = ROOT / "Cargo.toml"
MANIFEST = ROOT / "extension" / "manifest.json"

#: A release tag is the product version with a leading `v`, nothing else.
TAG_PATTERN = re.compile(r"^v(?P<version>\d+\.\d+\.\d+)$")


class VersionError(Exception):
    """A disagreement worth failing a build over."""


def cargo_version(text: str) -> str:
    """Read `version` from the [package] table without a TOML dependency."""
    package = re.search(r"(?ms)^\[package\][^\[]*", text)
    if package is None:
        raise VersionError(f"error: no [package] table in {CARGO_TOML}")
    version = re.search(r'(?m)^version\s*=\s*"([^"]+)"', package.group(0))
    if version is None:
        raise VersionError(f"error: no version in the [package] table of {CARGO_TOML}")
    return version.group(1)


def manifest_version(text: str) -> str:
    version = json.loads(text).get("version")
    if not isinstance(version, str):
        raise VersionError(f"error: no string version in {MANIFEST}")
    return version


def version_from_tag(tag: str) -> str:
    """The version a release tag names, or an error naming the expected shape."""
    match = TAG_PATTERN.match(tag)
    if match is None:
        raise VersionError(
            f"error: {tag!r} is not a release tag\n"
            "  a release tag is the product version with a leading v, "
            "for example v0.5.0"
        )
    return match.group("version")


def resolve(cargo_toml: str, manifest_json: str, tag: str | None = None) -> str:
    """The one product version, or a VersionError naming every side that differs."""
    cargo = cargo_version(cargo_toml)
    manifest = manifest_version(manifest_json)
    if cargo != manifest:
        raise VersionError(
            "error: the Cargo package and the extension manifest must carry the "
            "same version\n"
            f"  Cargo.toml:              {cargo}\n"
            f"  extension/manifest.json: {manifest}\n"
            'See "Versioning" in AGENTS.md; bump both together.'
        )
    if tag is not None:
        tagged = version_from_tag(tag)
        if tagged != cargo:
            raise VersionError(
                "error: the release tag and the product version must agree\n"
                f"  tag:                     {tag} (version {tagged})\n"
                f"  Cargo.toml:              {cargo}\n"
                f"  extension/manifest.json: {manifest}\n"
                "Bump both versions in the commit the tag points at, or tag "
                f"v{cargo} instead."
            )
    return cargo


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--tag",
        help="a release tag (vX.Y.Z) that must name the same version",
    )
    parser.add_argument(
        "--print",
        action="store_true",
        dest="print_only",
        help="print the agreed version alone, for scripts to read",
    )
    args = parser.parse_args()

    try:
        version = resolve(
            CARGO_TOML.read_text(encoding="utf-8"),
            MANIFEST.read_text(encoding="utf-8"),
            args.tag,
        )
    except VersionError as error:
        sys.exit(str(error))

    if args.print_only:
        print(version)
    elif args.tag:
        print(f"Cargo package, extension manifest and tag {args.tag} agree on version {version}")
    else:
        print(f"Cargo package and extension manifest agree on version {version}")


if __name__ == "__main__":
    main()
