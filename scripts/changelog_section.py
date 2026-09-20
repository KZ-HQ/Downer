#!/usr/bin/env python3
"""Print one version's section of CHANGELOG.md, for use as release notes.

`.github/workflows/release.yml` hands the result to `gh release create
--notes-file`, so a tagged release says what the changelog says rather than
repeating it somewhere a reader would have to reconcile. Keeping the extraction
here rather than in the workflow means it can be run and tested locally
(KEI-58).
"""

import argparse
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
CHANGELOG = ROOT / "CHANGELOG.md"

#: `## [0.5.0] - 2026-09-20`, `## [0.5.0]`, or `## [Unreleased]`.
HEADING = re.compile(r"^## \[(?P<name>[^\]]+)\]")


class ChangelogError(Exception):
    """The changelog does not say what a release needs it to say."""


def section(text: str, name: str) -> str:
    """The body under `## [name]`, without the heading, stripped of blank edges."""
    lines = text.splitlines()
    starts = [
        (index, match.group("name"))
        for index, line in enumerate(lines)
        if (match := HEADING.match(line))
    ]
    for position, (index, heading) in enumerate(starts):
        if heading.casefold() != name.casefold():
            continue
        end = starts[position + 1][0] if position + 1 < len(starts) else len(lines)
        body = "\n".join(lines[index + 1 : end]).strip("\n")
        if not body.strip():
            raise ChangelogError(f"error: the [{heading}] section of {CHANGELOG} is empty")
        return body

    known = ", ".join(heading for _, heading in starts) or "none"
    raise ChangelogError(
        f"error: {CHANGELOG} has no [{name}] section\n"
        f"  sections found: {known}\n"
        'See "Versioning" in AGENTS.md; move Unreleased under the new version '
        "when you bump it."
    )


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("version", help="the version to extract, for example 0.5.0")
    args = parser.parse_args()

    try:
        print(section(CHANGELOG.read_text(encoding="utf-8"), args.version))
    except ChangelogError as error:
        sys.exit(str(error))


if __name__ == "__main__":
    main()
