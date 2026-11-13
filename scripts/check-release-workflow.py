#!/usr/bin/env python3
"""Verify the release workflow restores no build cache.

A release artifact must be built only from the tagged source, never from a
cache populated by a prior run. This scans `.github/workflows/release.yml`
line by line for anything that restores a cache: a `uses:` of
`Swatinem/rust-cache` or `actions/cache` (including `actions/cache/restore`),
or a `cache:`/`cache-dependency-path:` mapping key (as used by
`actions/setup-node` and similar actions).
"""

from __future__ import annotations

import argparse
import sys
import tempfile
import unittest
from pathlib import Path

sys.dont_write_bytecode = True

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_WORKFLOW = ROOT / ".github" / "workflows" / "release.yml"

CACHE_USES_PREFIXES = ("Swatinem/rust-cache", "actions/cache")
CACHE_KEYS = ("cache:", "cache-dependency-path:")


def strip_trailing_comment(stripped: str) -> str:
    idx = stripped.find(" #")
    if idx == -1:
        return stripped
    return stripped[:idx].rstrip()


def find_cache_restores(path: Path) -> list[str]:
    findings: list[str] = []
    for lineno, raw_line in enumerate(path.read_text(encoding="utf-8").splitlines(), start=1):
        stripped = raw_line.strip()
        if not stripped or stripped.startswith("#"):
            continue

        code = strip_trailing_comment(stripped)

        uses_value = code
        if uses_value.startswith("- "):
            uses_value = uses_value[2:].strip()
        if uses_value.startswith("uses:"):
            value = uses_value[len("uses:"):].strip()
            if len(value) >= 2 and value[0] == value[-1] and value[0] in {"'", '"'}:
                value = value[1:-1]
            if value.startswith(CACHE_USES_PREFIXES):
                findings.append(
                    f"{path}:{lineno}: {code} restores a cache; "
                    "release builds must start clean"
                )
                continue

        key_part = code
        if key_part.startswith("- "):
            key_part = key_part[2:].strip()
        if key_part.startswith(CACHE_KEYS):
            findings.append(
                f"{path}:{lineno}: {code} restores a cache; "
                "release builds must start clean"
            )

    return findings


class CheckReleaseWorkflowTests(unittest.TestCase):
    def _write(self, body: str) -> Path:
        tmp = tempfile.NamedTemporaryFile(
            mode="w", suffix=".yml", delete=False, encoding="utf-8"
        )
        tmp.write(body)
        tmp.close()
        return Path(tmp.name)

    def test_clean_workflow_passes(self) -> None:
        path = self._write(
            "jobs:\n"
            "  build:\n"
            "    steps:\n"
            "      - uses: actions/checkout@abc\n"
            "      - uses: actions/setup-node@abc\n"
            "        with:\n"
            "          node-version: 22\n"
            "      - run: cargo build --release --locked -p kranz\n"
        )
        try:
            self.assertEqual(find_cache_restores(path), [])
        finally:
            path.unlink()

    def test_rust_cache_step_refused(self) -> None:
        path = self._write(
            "jobs:\n"
            "  build:\n"
            "    steps:\n"
            "      - uses: Swatinem/rust-cache@d1527c8f67ef9af357290475d4af4b0fab8f12e0 # v2.9.3\n"
        )
        try:
            findings = find_cache_restores(path)
            self.assertEqual(len(findings), 1)
            self.assertIn("Swatinem/rust-cache", findings[0])
        finally:
            path.unlink()

    def test_actions_cache_restore_refused(self) -> None:
        path = self._write(
            "jobs:\n"
            "  build:\n"
            "    steps:\n"
            "      - uses: actions/cache/restore@abc\n"
        )
        try:
            findings = find_cache_restores(path)
            self.assertEqual(len(findings), 1)
            self.assertIn("actions/cache/restore", findings[0])
        finally:
            path.unlink()

    def test_setup_node_cache_npm_refused(self) -> None:
        path = self._write(
            "jobs:\n"
            "  build:\n"
            "    steps:\n"
            "      - uses: actions/setup-node@abc\n"
            "        with:\n"
            "          node-version: 22\n"
            "          cache: npm\n"
        )
        try:
            findings = find_cache_restores(path)
            self.assertEqual(len(findings), 1)
            self.assertIn("cache: npm", findings[0])
        finally:
            path.unlink()

    def test_commented_out_rust_cache_not_refused(self) -> None:
        path = self._write(
            "jobs:\n"
            "  build:\n"
            "    steps:\n"
            "      # - uses: Swatinem/rust-cache@d1527c8f67ef9af357290475d4af4b0fab8f12e0 # v2.9.3\n"
            "      - uses: actions/checkout@abc\n"
        )
        try:
            self.assertEqual(find_cache_restores(path), [])
        finally:
            path.unlink()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("path", type=Path, nargs="?", default=DEFAULT_WORKFLOW)
    parser.add_argument("--self-test", action="store_true", help="Run embedded unit tests")
    args = parser.parse_args()

    if args.self_test:
        suite = unittest.defaultTestLoader.loadTestsFromTestCase(CheckReleaseWorkflowTests)
        result = unittest.TextTestRunner(verbosity=1).run(suite)
        return 0 if result.wasSuccessful() else 1

    findings = find_cache_restores(args.path)
    if findings:
        for finding in findings:
            print(f"check-release-workflow: {finding}", file=sys.stderr)
        return 1

    print(f"check-release-workflow: {args.path} restores no cache")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
