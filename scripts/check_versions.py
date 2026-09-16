#!/usr/bin/env python3
"""Verify that the Cargo package and the Firefox extension share one version.

The versioning rule is documented in AGENTS.md: `version` in Cargo.toml and
`version` in extension/manifest.json are the source of truth for the product
version and are always bumped together. This script is run by `make check`,
and therefore by CI.
"""

import json
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
CARGO_TOML = ROOT / "Cargo.toml"
MANIFEST = ROOT / "extension" / "manifest.json"


def cargo_version() -> str:
    """Read `version` from the [package] table without a TOML dependency."""
    text = CARGO_TOML.read_text(encoding="utf-8")
    package = re.search(r"(?ms)^\[package\][^\[]*", text)
    if package is None:
        sys.exit(f"error: no [package] table in {CARGO_TOML}")
    version = re.search(r'(?m)^version\s*=\s*"([^"]+)"', package.group(0))
    if version is None:
        sys.exit(f"error: no version in the [package] table of {CARGO_TOML}")
    return version.group(1)


def manifest_version() -> str:
    manifest = json.loads(MANIFEST.read_text(encoding="utf-8"))
    version = manifest.get("version")
    if not isinstance(version, str):
        sys.exit(f"error: no string version in {MANIFEST}")
    return version


def main() -> None:
    cargo = cargo_version()
    manifest = manifest_version()
    if cargo != manifest:
        sys.exit(
            "error: the Cargo package and the extension manifest must carry the "
            "same version\n"
            f"  Cargo.toml:              {cargo}\n"
            f"  extension/manifest.json: {manifest}\n"
            "See \"Versioning\" in AGENTS.md; bump both together."
        )
    print(f"Cargo package and extension manifest agree on version {cargo}")


if __name__ == "__main__":
    main()
