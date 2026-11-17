#!/usr/bin/env python3
"""Fail when a test run left anything behind in its temp directory.

CI gives the workspace test suite a temp directory of its own (`TMPDIR`) and
runs this script on that directory once the suite has passed. Every entry
still there is a file or folder a test created and did not remove, which on a
developer's machine would pile up in the system temp directory.

Usage:
    check-test-temp-leftovers.py <directory>
    check-test-temp-leftovers.py --self-test

The script only lists. It never deletes anything.

A test that needs a directory owns it with `tempfile::TempDir`, which removes
it when the test returns or panics. A directory that must outlive one test
(a home shared by a whole test binary, or the scratch root sessions write to)
goes through `kranz_engine::test_dirs`, which removes it when the test process
exits.

The check is only meaningful after a passing run: a failing or killed test
process may leave its directories, and this script does not excuse or hide
that. To run it locally:

    dir="$(mktemp -d /tmp/kranz-test-tmp.XXXXXX)"
    TMPDIR="$dir" cargo test --workspace --no-fail-fast \\
      && python3 scripts/check-test-temp-leftovers.py "$dir"
"""

from __future__ import annotations

import argparse
import os
from pathlib import Path
import sys
import tempfile
import time
import unittest

sys.dont_write_bytecode = True

# A child process that a test started can still be exiting, and removing its
# files, for a moment after the test binary itself has returned.
DEFAULT_SETTLE_SECONDS = 10.0
POLL_SECONDS = 0.5


def leftovers(directory: Path) -> list[str]:
    """Names of the entries directly inside `directory`, sorted."""
    return sorted(entry.name for entry in os.scandir(directory))


def wait_for_empty(directory: Path, settle_seconds: float) -> list[str]:
    """Entries still inside `directory` after waiting up to `settle_seconds`
    for it to become empty."""
    deadline = time.monotonic() + settle_seconds
    while True:
        found = leftovers(directory)
        if not found or time.monotonic() >= deadline:
            return found
        time.sleep(POLL_SECONDS)


def describe(directory: Path, name: str) -> str:
    path = directory / name
    if path.is_symlink():
        kind = "symlink"
    elif path.is_dir():
        kind = "directory"
    else:
        kind = "file"
    return f"  {path} ({kind})"


def check(directory: Path, settle_seconds: float) -> int:
    if not directory.is_dir():
        print(f"FAIL: {directory} is not a directory", file=sys.stderr)
        return 1
    found = wait_for_empty(directory, settle_seconds)
    if not found:
        print(f"OK: the test run left nothing in {directory}")
        return 0
    print(
        f"FAIL: the test run left {len(found)} entr{'y' if len(found) == 1 else 'ies'} "
        f"in its temp directory {directory}:",
        file=sys.stderr,
    )
    for name in found:
        print(describe(directory, name), file=sys.stderr)
    print(
        "A test must remove what it creates. Own a per-test directory with "
        "tempfile::TempDir; register one that a whole test binary shares with "
        "kranz_engine::test_dirs. See the module documentation of "
        "crates/engine/src/test_dirs.rs.",
        file=sys.stderr,
    )
    return 1


class SelfTest(unittest.TestCase):
    def test_an_empty_directory_passes(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            self.assertEqual(check(Path(raw), 0.0), 0)

    def test_a_leftover_directory_fails_and_is_listed(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            directory = Path(raw)
            (directory / "kranz-worker-home-abc").mkdir()
            (directory / "stray.txt").write_text("x", encoding="utf-8")
            self.assertEqual(
                leftovers(directory), ["kranz-worker-home-abc", "stray.txt"]
            )
            self.assertEqual(check(directory, 0.0), 1)
            # The check lists; it never removes.
            self.assertEqual(
                leftovers(directory), ["kranz-worker-home-abc", "stray.txt"]
            )

    def test_a_dangling_symlink_counts_as_a_leftover(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            directory = Path(raw)
            try:
                os.symlink(directory / "absent", directory / "link")
            except (OSError, NotImplementedError):
                self.skipTest("symlinks are not available here")
            self.assertEqual(check(directory, 0.0), 1)

    def test_a_missing_directory_fails(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            self.assertEqual(check(Path(raw) / "absent", 0.0), 1)

    def test_an_entry_that_goes_away_while_settling_passes(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            directory = Path(raw)
            entry = directory / "still-exiting"
            entry.mkdir()
            original_sleep = time.sleep

            def remove_then_sleep(seconds: float) -> None:
                if entry.exists():
                    entry.rmdir()
                original_sleep(0)

            time.sleep = remove_then_sleep
            try:
                self.assertEqual(wait_for_empty(directory, 5.0), [])
            finally:
                time.sleep = original_sleep


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("directory", nargs="?", type=Path)
    parser.add_argument(
        "--settle-seconds",
        type=float,
        default=DEFAULT_SETTLE_SECONDS,
        help="how long to wait for the directory to become empty "
        f"(default {DEFAULT_SETTLE_SECONDS:g})",
    )
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    if args.self_test:
        suite = unittest.defaultTestLoader.loadTestsFromTestCase(SelfTest)
        result = unittest.TextTestRunner(verbosity=1).run(suite)
        return 0 if result.wasSuccessful() else 1
    if args.directory is None:
        parser.error("a directory is required unless --self-test is given")
    return check(args.directory, args.settle_seconds)


if __name__ == "__main__":
    sys.exit(main())
