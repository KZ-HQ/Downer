#!/usr/bin/env python3
"""Merge per-platform release staging directories into one set of release files.

`release_artifacts.sh` runs once per platform and each run uploads its own
staging directory. This collects them into the files a GitHub Release carries:
one tarball per platform, one `.xpi`, and one `SHA256SUMS` covering all of them.

Two things are checked rather than assumed (KEI-58):

* Every platform packages the extension, and the packaging is reproducible, so
  the copies of `downer-<version>.xpi` must be byte-identical. If they are not,
  something is unreproducible and the checksum published for the XPI would be
  true of only one build, so the release fails here instead.
* Each platform's own `SHA256SUMS.<target>` is re-verified against the bytes
  that arrived, which catches a file damaged between the build and here.
"""

# Annotations are not evaluated at runtime, so this file's type hints may use
# syntax newer than the Python running it. That matters because these scripts
# are meant to run on a laptop as well as in CI (AGENTS.md), and macOS ships
# Python 3.9 — where `str | None` in a signature is a TypeError at import, not
# a hint. See KEI-93.
from __future__ import annotations

import argparse
import hashlib
import pathlib
import shutil
import sys


class CollectError(Exception):
    """The staged artifacts do not add up to a release."""


def digest(path: pathlib.Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def staged_files(source: pathlib.Path) -> dict[str, list[pathlib.Path]]:
    """Artifact files by name, gathered from every per-platform directory."""
    found: dict[str, list[pathlib.Path]] = {}
    for path in sorted(source.rglob("*")):
        if not path.is_file() or path.name.startswith("SHA256SUMS"):
            continue
        found.setdefault(path.name, []).append(path)
    if not found:
        raise CollectError(f"error: no artifacts under {source}")
    return found


def verify_staged_checksums(source: pathlib.Path) -> int:
    """Re-check each platform's own checksum file. Returns how many it covered."""
    checked = 0
    for sums in sorted(source.rglob("SHA256SUMS*")):
        for line in sums.read_text(encoding="utf-8").splitlines():
            if not line.strip():
                continue
            expected, _, name = line.partition(" ")
            name = name.lstrip("* ").strip()
            artifact = sums.parent / name
            if not artifact.is_file():
                raise CollectError(f"error: {sums} lists {name}, which is not beside it")
            actual = digest(artifact)
            if actual != expected:
                raise CollectError(
                    f"error: {artifact} does not match {sums.name}\n"
                    f"  recorded: {expected}\n"
                    f"  actual:   {actual}"
                )
            checked += 1
    return checked


def collect(source: pathlib.Path, destination: pathlib.Path) -> list[pathlib.Path]:
    """Copy one of each artifact into `destination` and write SHA256SUMS."""
    checked = verify_staged_checksums(source)
    files = staged_files(source)

    destination.mkdir(parents=True, exist_ok=True)
    released = []
    for name, copies in sorted(files.items()):
        digests = {digest(copy) for copy in copies}
        if len(digests) > 1:
            listed = "\n".join(f"  {digest(copy)}  {copy}" for copy in copies)
            raise CollectError(
                f"error: the copies of {name} differ, so no one checksum "
                f"describes it\n{listed}\n"
                "The extension package is meant to be reproducible; see "
                "scripts/package_extension.sh."
            )
        target = destination / name
        shutil.copy2(copies[0], target)
        released.append(target)

    sums = destination / "SHA256SUMS"
    sums.write_text(
        "".join(f"{digest(path)}  {path.name}\n" for path in released),
        encoding="utf-8",
    )
    print(f"Verified {checked} staged checksum(s); collected {len(released)} file(s):")
    for path in released:
        print(f"  {path.name}")
    print(f"  {sums.name}")
    return released


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=pathlib.Path, help="directory of staged artifacts")
    parser.add_argument("destination", type=pathlib.Path, help="where the release files go")
    args = parser.parse_args()

    try:
        collect(args.source, args.destination)
    except CollectError as error:
        sys.exit(str(error))


if __name__ == "__main__":
    main()
