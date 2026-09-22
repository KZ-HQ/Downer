#!/usr/bin/env python3
"""Tests for the release tooling, run by `make check` and therefore by CI.

The release workflow itself cannot be proven without pushing a tag, so the
parts that can fail for an ordinary reason — the version gate, the release
notes, merging per-platform artifacts, and the packaging rule — are kept out of
the YAML and tested here instead (KEI-58). AGENTS.md requires a new check to
live in `make check` rather than only in a workflow, so that a local run and CI
cannot drift apart.
"""

import pathlib
import subprocess
import sys
import unittest

ROOT = pathlib.Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "scripts"))

import changelog_section  # noqa: E402
import check_versions  # noqa: E402
import release_collect  # noqa: E402

CARGO = '[package]\nname = "downer"\nversion = "1.2.3"\n\n[dependencies]\nserde = "1"\n'
MANIFEST = '{"version": "1.2.3"}'


class VersionGate(unittest.TestCase):
    def test_agreeing_versions_resolve(self):
        self.assertEqual(check_versions.resolve(CARGO, MANIFEST), "1.2.3")

    def test_a_matching_tag_resolves(self):
        self.assertEqual(check_versions.resolve(CARGO, MANIFEST, "v1.2.3"), "1.2.3")

    def test_cargo_and_manifest_must_agree(self):
        with self.assertRaises(check_versions.VersionError) as raised:
            check_versions.resolve(CARGO, '{"version": "1.2.4"}')
        self.assertIn("1.2.4", str(raised.exception))

    def test_the_tag_must_name_the_same_version(self):
        with self.assertRaises(check_versions.VersionError) as raised:
            check_versions.resolve(CARGO, MANIFEST, "v2.0.0")
        message = str(raised.exception)
        # The failure has to name all three sides, because the fix differs
        # depending on which one is wrong.
        self.assertIn("v2.0.0", message)
        self.assertIn("Cargo.toml", message)
        self.assertIn("extension/manifest.json", message)

    def test_a_tag_must_look_like_a_release_tag(self):
        for tag in ["1.2.3", "v1.2", "v1.2.3-rc1", "release-1.2.3", "v1.2.3 "]:
            with self.subTest(tag=tag), self.assertRaises(check_versions.VersionError):
                check_versions.resolve(CARGO, MANIFEST, tag)

    def test_a_cargo_file_without_a_version_is_an_error(self):
        with self.assertRaises(check_versions.VersionError):
            check_versions.resolve('[package]\nname = "downer"\n', MANIFEST)

    def test_the_real_repository_agrees_with_itself(self):
        # What `make check` is actually for; the cases above are about the
        # messages, this one is about the repository.
        version = check_versions.resolve(
            check_versions.CARGO_TOML.read_text(encoding="utf-8"),
            check_versions.MANIFEST.read_text(encoding="utf-8"),
        )
        self.assertRegex(version, r"^\d+\.\d+\.\d+$")


class ReleaseNotes(unittest.TestCase):
    CHANGELOG = (
        "# Changelog\n\nPreamble.\n\n"
        "## [Unreleased]\n\n### Added\n\n- Something new.\n\n"
        "## [1.2.3] - 2026-09-20\n\n### Fixed\n\n- Something old.\n\n"
        "## [1.2.2] - 2026-09-01\n\n- The first one.\n"
    )

    def test_it_takes_one_section(self):
        self.assertEqual(
            changelog_section.section(self.CHANGELOG, "1.2.3"),
            "### Fixed\n\n- Something old.",
        )

    def test_the_last_section_runs_to_the_end_of_the_file(self):
        self.assertEqual(
            changelog_section.section(self.CHANGELOG, "1.2.2"), "- The first one."
        )

    def test_unreleased_is_a_section_like_any_other(self):
        self.assertEqual(
            changelog_section.section(self.CHANGELOG, "unreleased"),
            "### Added\n\n- Something new.",
        )

    def test_a_missing_section_names_what_is_there(self):
        with self.assertRaises(changelog_section.ChangelogError) as raised:
            changelog_section.section(self.CHANGELOG, "9.9.9")
        self.assertIn("1.2.3", str(raised.exception))

    def test_an_empty_section_is_an_error(self):
        # Releasing notes that say nothing is worse than failing the release.
        with self.assertRaises(changelog_section.ChangelogError):
            changelog_section.section("## [1.0.0]\n\n## [0.9.0]\n\n- Old.\n", "1.0.0")

    def test_the_real_changelog_has_an_unreleased_section(self):
        # The heading must exist, so the next change has somewhere to go.
        #
        # Its body may be empty, and this deliberately does not call `section()`,
        # which treats an empty body as an error. Immediately after a release an
        # empty `Unreleased` is the *correct* state — the previous contents have
        # just been moved under the new version number — and requiring content
        # here would mean the repository could never be left in the state a
        # release leaves it in. The rule that matters is unchanged and still
        # tested by `test_an_empty_section_is_an_error`: publishing a *version*
        # whose notes say nothing is an error.
        text = changelog_section.CHANGELOG.read_text(encoding="utf-8")
        headings = [
            match.group("name").casefold()
            for line in text.splitlines()
            if (match := changelog_section.HEADING.match(line))
        ]
        self.assertIn("unreleased", headings)


class Packaging(unittest.TestCase):
    """The packaging rule itself; tests/e2e/xpi.test.mjs checks Firefox accepts it."""

    def package(self, destination):
        subprocess.run(
            [str(ROOT / "scripts" / "package_extension.sh"), str(destination)],
            check=True,
            capture_output=True,
        )
        return destination.read_bytes()

    def test_packaging_is_reproducible(self):
        import tempfile

        with tempfile.TemporaryDirectory() as temp:
            temp = pathlib.Path(temp)
            first = self.package(temp / "one.xpi")
            second = self.package(temp / "two.xpi")
        self.assertEqual(
            first,
            second,
            "a published SHA256SUMS is only useful if the same commit packages "
            "to the same bytes",
        )

    def test_the_package_carries_the_manifest(self):
        import tempfile
        import zipfile

        with tempfile.TemporaryDirectory() as temp:
            path = pathlib.Path(temp) / "downer.xpi"
            self.package(path)
            with zipfile.ZipFile(path) as archive:
                names = archive.namelist()
                self.assertIn("manifest.json", names)
                self.assertEqual(names, sorted(names), "members are in a stable order")
                # Every shipped file, and nothing from outside extension/.
                shipped = sorted(p.name for p in (ROOT / "extension").iterdir() if p.is_file())
                self.assertEqual(sorted(names), shipped)


class Collecting(unittest.TestCase):
    def stage(self, root, target, xpi_bytes=b"xpi"):
        """One platform's staging directory, as release_artifacts.sh leaves it."""
        import hashlib

        directory = root / f"release-{target}"
        directory.mkdir(parents=True)
        files = {f"downer-1.2.3-{target}.tar.gz": f"binary for {target}".encode(),
                 "downer-1.2.3.xpi": xpi_bytes}
        for name, content in files.items():
            (directory / name).write_bytes(content)
        (directory / f"SHA256SUMS.{target}").write_text(
            "".join(
                f"{hashlib.sha256(content).hexdigest()}  {name}\n"
                for name, content in files.items()
            ),
            encoding="utf-8",
        )
        return directory

    def test_it_keeps_one_of_each_and_writes_one_checksum_file(self):
        import tempfile

        with tempfile.TemporaryDirectory() as temp:
            temp = pathlib.Path(temp)
            source, destination = temp / "staged", temp / "release"
            self.stage(source, "aarch64-apple-darwin")
            self.stage(source, "x86_64-unknown-linux-gnu")

            release_collect.collect(source, destination)

            names = sorted(p.name for p in destination.iterdir())
            self.assertEqual(
                names,
                [
                    "SHA256SUMS",
                    "downer-1.2.3-aarch64-apple-darwin.tar.gz",
                    "downer-1.2.3-x86_64-unknown-linux-gnu.tar.gz",
                    "downer-1.2.3.xpi",
                ],
            )
            sums = (destination / "SHA256SUMS").read_text(encoding="utf-8")
            self.assertEqual(len(sums.splitlines()), 3)

    def test_the_combined_checksums_verify_with_sha256sum(self):
        import shutil
        import tempfile

        tool = shutil.which("sha256sum") or shutil.which("shasum")
        if tool is None:
            self.skipTest("SKIP: no sha256sum or shasum to verify against")
        command = [tool] + ([] if tool.endswith("sha256sum") else ["-a", "256"]) + ["-c"]

        with tempfile.TemporaryDirectory() as temp:
            temp = pathlib.Path(temp)
            source, destination = temp / "staged", temp / "release"
            self.stage(source, "x86_64-unknown-linux-gnu")
            release_collect.collect(source, destination)
            # The point of publishing checksums is that a downloader can run
            # exactly this, so run exactly this.
            subprocess.run(
                command + ["SHA256SUMS"], cwd=destination, check=True, capture_output=True
            )

    def test_differing_copies_of_the_extension_fail(self):
        import tempfile

        with tempfile.TemporaryDirectory() as temp:
            temp = pathlib.Path(temp)
            source = temp / "staged"
            self.stage(source, "aarch64-apple-darwin", xpi_bytes=b"one")
            self.stage(source, "x86_64-unknown-linux-gnu", xpi_bytes=b"other")
            with self.assertRaises(release_collect.CollectError) as raised:
                release_collect.collect(source, temp / "release")
            self.assertIn("downer-1.2.3.xpi", str(raised.exception))

    def test_a_damaged_artifact_fails_its_own_checksum(self):
        import tempfile

        with tempfile.TemporaryDirectory() as temp:
            temp = pathlib.Path(temp)
            source = temp / "staged"
            directory = self.stage(source, "x86_64-unknown-linux-gnu")
            (directory / "downer-1.2.3.xpi").write_bytes(b"damaged in transit")
            with self.assertRaises(release_collect.CollectError) as raised:
                release_collect.collect(source, temp / "release")
            self.assertIn("does not match", str(raised.exception))

    def test_nothing_staged_is_an_error(self):
        import tempfile

        with tempfile.TemporaryDirectory() as temp:
            temp = pathlib.Path(temp)
            (temp / "staged").mkdir()
            with self.assertRaises(release_collect.CollectError):
                release_collect.collect(temp / "staged", temp / "release")


if __name__ == "__main__":
    unittest.main(verbosity=2)
