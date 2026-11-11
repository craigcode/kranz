#!/usr/bin/env python3
"""Verify GitHub Actions workflow `uses:` pins and repository allowlist policy.

Every `uses:` reference across `.github/workflows/*.yml` and `*.yaml` must:
1. Pin an immutable full-length 40-character lowercase hexadecimal commit SHA
   (`<owner>/<repo>(/<subpath>)?@<40-hex-sha>`).
2. Be permitted by `.github/actions-allowlist.json` (`github_owned_allowed` or
   `patterns_allowed`, with `verified_allowed` required to stay `false` so policy
   enforcement remains offline and deterministic).
"""

from __future__ import annotations

import argparse
from fnmatch import fnmatchcase
import json
from pathlib import Path
import re
import sys
import tempfile
import unittest

sys.dont_write_bytecode = True

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_ALLOWLIST = ROOT / ".github" / "actions-allowlist.json"
DEFAULT_WORKFLOWS_DIR = ROOT / ".github" / "workflows"

USES_LINE_RE = re.compile(r"^(?P<prefix>\s*(?:-\s+)?)uses\s*:\s*(?P<rest>.*?)\s*$")
BLOCK_SCALAR_RE = re.compile(
    r"^(?P<indent>\s*)(?:-\s*(?P<seq_key>[A-Za-z0-9_.-]+\s*:\s*)?|[A-Za-z0-9_.-]+\s*:\s*)"
    r"[|>][0-9+-]*(?:\s+#.*)?\s*$"
)
FLOW_USES_RE = re.compile(r"(?<!\$)\{[^{}]*\buses\s*:")
# A quoted mapping key (`"uses": x`, `'uses': x`) or an explicit one (`? uses`).
# YAML reads these as the same key the plain-key pattern above matches, so a
# checker that only knows the plain spelling would wave the reference through.
QUOTED_KEY_RE = re.compile(r"""^\s*(?:-\s+)?(?P<quote>["'])(?P<key>.*?)(?P=quote)\s*:""")
EXPLICIT_KEY_RE = re.compile(r"^\s*(?:-\s+)?\?\s")
ACTION_REF_RE = re.compile(
    r"^(?P<owner>[A-Za-z0-9_.-]+)/(?P<repo>[A-Za-z0-9_.-]+)(?P<subpath>(?:/[A-Za-z0-9_.-]+)*)@(?P<ref>.+)$"
)
SHA40_RE = re.compile(r"^[0-9a-f]{40}$")
GITHUB_OWNED_ORGS = frozenset({"actions", "github"})


def strip_yaml_comment(raw: str) -> str:
    """Strip a trailing YAML `#` comment while respecting single/double quotes."""
    in_single = False
    in_double = False
    escaped = False
    for idx, ch in enumerate(raw):
        if escaped:
            escaped = False
            continue
        if in_double and ch == "\\":
            escaped = True
            continue
        if ch == "'" and not in_double:
            in_single = not in_single
            continue
        if ch == '"' and not in_single:
            in_double = not in_double
            continue
        if ch == "#" and not in_single and not in_double:
            if idx == 0 or raw[idx - 1].isspace():
                return raw[:idx].rstrip()
    return raw.rstrip()


def unquote_yaml_scalar(raw: str) -> str:
    value = strip_yaml_comment(raw).strip()
    if len(value) >= 2 and value[0] == value[-1] and value[0] in {"'", '"'}:
        quote = value[0]
        inner = value[1:-1]
        if quote == "'":
            return inner.replace("''", "'")
        return inner
    return value


def load_allowlist(path: Path) -> tuple[bool, list[str]]:
    data = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(data, dict):
        raise ValueError(f"{path}: allowlist root must be a JSON object")
    expected_keys = {"github_owned_allowed", "verified_allowed", "patterns_allowed"}
    unknown = set(data) - expected_keys
    missing = expected_keys - set(data)
    if unknown or missing:
        raise ValueError(
            f"{path}: allowlist keys must be exactly {sorted(expected_keys)} "
            f"(missing={sorted(missing)}, unknown={sorted(unknown)})"
        )
    github_owned = data["github_owned_allowed"]
    verified_allowed = data["verified_allowed"]
    patterns = data["patterns_allowed"]
    if not isinstance(github_owned, bool) or not isinstance(verified_allowed, bool):
        raise ValueError(f"{path}: github_owned_allowed and verified_allowed must be booleans")
    if verified_allowed:
        raise ValueError(
            f"{path}: verified_allowed=true cannot be enforced deterministically offline; keep false"
        )
    if not isinstance(patterns, list) or not all(
        isinstance(p, str) and p.strip() and p.strip() != "*" for p in patterns
    ):
        raise ValueError(f"{path}: patterns_allowed must be a list of explicit action patterns")
    return github_owned, [p.strip() for p in patterns]


def extract_workflow_uses(path: Path) -> tuple[list[tuple[int, str]], list[str]]:
    uses_entries: list[tuple[int, str]] = []
    errors: list[str] = []
    block_min_indent: int | None = None

    for lineno, raw_line in enumerate(path.read_text(encoding="utf-8").splitlines(), start=1):
        stripped = raw_line.strip()
        if not stripped:
            continue
        indent = len(raw_line) - len(raw_line.lstrip(" "))
        if block_min_indent is not None:
            if indent > block_min_indent:
                continue
            block_min_indent = None

        if stripped.startswith("#"):
            continue

        block_match = BLOCK_SCALAR_RE.match(raw_line)
        if block_match is not None:
            # For `- run: |`, the mapping key column is after `- `; any sibling or
            # subsequent step key has indent <= that column.
            indent_prefix = block_match.group("indent")
            if raw_line.lstrip(" ").startswith("-"):
                dash_offset = raw_line.index("-")
                after_dash = len(raw_line[dash_offset + 1 :]) - len(
                    raw_line[dash_offset + 1 :].lstrip(" ")
                )
                block_min_indent = (
                    dash_offset + 1 + after_dash if block_match.group("seq_key") else dash_offset
                )
            else:
                block_min_indent = len(indent_prefix)
            continue

        uncommented = strip_yaml_comment(raw_line)
        if FLOW_USES_RE.search(uncommented):
            errors.append(
                f"{path}:{lineno}: flow-mapping `uses:` syntax is forbidden; use standard block YAML"
            )
            continue

        quoted = QUOTED_KEY_RE.match(raw_line)
        if quoted is not None:
            key = quoted.group("key")
            # A double-quoted key can spell `uses` with escapes (`"u\x73es"`);
            # no workflow key needs one, so any escape is refused unread.
            if key.strip() == "uses" or (quoted.group("quote") == '"' and "\\" in key):
                errors.append(
                    f"{path}:{lineno}: quoted `uses` key (or an escaped quoted key) is forbidden; "
                    "write a plain `uses:`"
                )
                continue
        if EXPLICIT_KEY_RE.match(raw_line):
            errors.append(
                f"{path}:{lineno}: explicit `? key` mapping syntax is forbidden; use standard block YAML"
            )
            continue

        match = USES_LINE_RE.match(raw_line)
        if match is None:
            continue

        value = unquote_yaml_scalar(match.group("rest"))
        uses_entries.append((lineno, value))

    return uses_entries, errors


def check_action_ref(
    workflow: Path,
    lineno: int,
    ref_value: str,
    github_owned_allowed: bool,
    patterns_allowed: list[str],
) -> str | None:
    if not ref_value:
        return f"{workflow}:{lineno}: empty `uses:` reference"
    if ref_value.startswith("./") or ref_value.startswith("docker://"):
        return f"{workflow}:{lineno}: unpinned local/docker `uses:` reference `{ref_value}` is not allowed"

    match = ACTION_REF_RE.match(ref_value)
    if match is None:
        return (
            f"{workflow}:{lineno}: invalid `uses:` reference `{ref_value}` "
            "(expected `<owner>/<repo>[/<subpath>]@<40-hex-sha>`)"
        )

    owner = match.group("owner")
    repo = match.group("repo")
    subpath = match.group("subpath")
    pinned_ref = match.group("ref")

    segments = [owner, repo, *[seg for seg in subpath.split("/") if seg]]
    if any(seg in {".", ".."} for seg in segments):
        return f"{workflow}:{lineno}: path traversal segment in `uses:` reference `{ref_value}`"

    if SHA40_RE.fullmatch(pinned_ref) is None:
        return (
            f"{workflow}:{lineno}: `uses: {ref_value}` is not pinned to a 40-character "
            "lowercase hexadecimal commit SHA"
        )

    if github_owned_allowed and owner in GITHUB_OWNED_ORGS:
        return None

    if any(fnmatchcase(ref_value, pattern) for pattern in patterns_allowed):
        return None

    return (
        f"{workflow}:{lineno}: `uses: {ref_value}` is not permitted by "
        ".github/actions-allowlist.json"
    )


def verify_workflows(allowlist_path: Path, workflows_dir: Path) -> tuple[int, list[str]]:
    github_owned_allowed, patterns_allowed = load_allowlist(allowlist_path)
    workflow_files = sorted(
        [*workflows_dir.glob("*.yml"), *workflows_dir.glob("*.yaml")]
    )
    if not workflow_files:
        return 0, [f"{workflows_dir}: no workflow files found"]

    total_uses = 0
    errors: list[str] = []
    for workflow in workflow_files:
        entries, parse_errors = extract_workflow_uses(workflow)
        errors.extend(parse_errors)
        for lineno, ref_value in entries:
            total_uses += 1
            err = check_action_ref(
                workflow, lineno, ref_value, github_owned_allowed, patterns_allowed
            )
            if err is not None:
                errors.append(err)

    if total_uses == 0 and not errors:
        errors.append(f"{workflows_dir}: zero `uses:` references found across workflows")
    return total_uses, errors


class CheckActionsAllowlistTests(unittest.TestCase):
    SHA_A = "3d3c42e5aac5ba805825da76410c181273ba90b1"
    SHA_B = "4be7066ada62dd38de10e7b70166bc74ed198c30"

    def _write_fixture(
        self,
        root: Path,
        workflow_body: str,
        allowlist: dict[str, object] | None = None,
    ) -> tuple[Path, Path]:
        gh = root / ".github"
        wf = gh / "workflows"
        wf.mkdir(parents=True, exist_ok=True)
        allowlist_path = gh / "actions-allowlist.json"
        allowlist_path.write_text(
            json.dumps(
                allowlist
                if allowlist is not None
                else {
                    "github_owned_allowed": True,
                    "verified_allowed": False,
                    "patterns_allowed": ["dtolnay/rust-toolchain@*"],
                }
            ),
            encoding="utf-8",
        )
        (wf / "ci.yml").write_text(workflow_body, encoding="utf-8")
        return allowlist_path, wf

    def test_accepts_sha_pinned_allowlisted_actions_and_ignores_statuses_and_run_blocks(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            allowlist_path, wf = self._write_fixture(
                Path(tmp),
                f"""name: test
permissions:
  statuses: write
jobs:
  check:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@{self.SHA_A} # v7.0.1
      - uses: github/codeql-action/init@{self.SHA_A} # v4
      - uses: "dtolnay/rust-toolchain@{self.SHA_B}" # stable
      - name: Shell script mentioning uses inside block scalar
        run: |
          echo "uses: unpinned/action@v1"
""",
            )
            count, errors = verify_workflows(allowlist_path, wf)
            self.assertEqual(errors, [])
            self.assertEqual(count, 3)

    def test_rejects_tag_short_sha_uppercase_sha_and_unallowlisted_repo(self) -> None:
        cases = [
            "actions/checkout@v4",
            "actions/checkout@3d3c42e",
            f"actions/checkout@{self.SHA_A.upper()}",
            f"untrusted/custom-action@{self.SHA_A}",
            "./.github/actions/local",
            "docker://alpine:3.20",
        ]
        for bad_ref in cases:
            with self.subTest(bad_ref=bad_ref), tempfile.TemporaryDirectory() as tmp:
                allowlist_path, wf = self._write_fixture(
                    Path(tmp),
                    f"jobs:\n  a:\n    steps:\n      - uses: {bad_ref}\n",
                )
                _, errors = verify_workflows(allowlist_path, wf)
                self.assertEqual(len(errors), 1, f"expected failure for {bad_ref}: {errors}")

    def test_sibling_uses_after_inline_sequence_run_block_is_not_skipped(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            allowlist_path, wf = self._write_fixture(
                Path(tmp),
                """jobs:
  a:
    steps:
      - run: |
          echo ok
        uses: actions/checkout@v4
""",
            )
            _, errors = verify_workflows(allowlist_path, wf)
            self.assertEqual(len(errors), 1)
            self.assertIn("40-character", errors[0])

    def test_rejects_uses_keys_the_plain_pattern_cannot_see(self) -> None:
        # Audit M21: each of these is a real `uses` key to a YAML parser. One
        # valid plain reference alongside keeps the "zero references" backstop
        # from being what fails the file.
        evasions = [
            '"uses": untrusted/action@v1',
            "'uses': untrusted/action@v1",
            '"uses" : untrusted/action@v1',
            '"u\\x73es": untrusted/action@v1',
            "? uses\n        : untrusted/action@v1",
        ]
        for evasion in evasions:
            with self.subTest(evasion=evasion), tempfile.TemporaryDirectory() as tmp:
                allowlist_path, wf = self._write_fixture(
                    Path(tmp),
                    "jobs:\n  a:\n    steps:\n"
                    f"      - uses: actions/checkout@{self.SHA_A}\n"
                    f"      - {evasion}\n",
                )
                _, errors = verify_workflows(allowlist_path, wf)
                self.assertEqual(len(errors), 1, f"expected one refusal for {evasion!r}: {errors}")
                self.assertIn("forbidden", errors[0])

        # An ordinary quoted key that is not `uses` stays legal.
        with tempfile.TemporaryDirectory() as tmp:
            allowlist_path, wf = self._write_fixture(
                Path(tmp),
                '"on": push\njobs:\n  a:\n    steps:\n'
                f"      - uses: actions/checkout@{self.SHA_A}\n"
                '        "with":\n          "fetch-depth": 0\n',
            )
            _, errors = verify_workflows(allowlist_path, wf)
            self.assertEqual(errors, [])

    def test_rejects_flow_mapping_and_verified_allowed(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            allowlist_path, wf = self._write_fixture(
                Path(tmp),
                "jobs:\n  a:\n    steps:\n      - { uses: actions/checkout@v4 }\n",
            )
            _, errors = verify_workflows(allowlist_path, wf)
            self.assertEqual(len(errors), 1)
            self.assertIn("flow-mapping", errors[0])

        with tempfile.TemporaryDirectory() as tmp:
            allowlist_path, wf = self._write_fixture(
                Path(tmp),
                f"jobs:\n  a:\n    steps:\n      - uses: actions/checkout@{self.SHA_A}\n",
                allowlist={
                    "github_owned_allowed": True,
                    "verified_allowed": True,
                    "patterns_allowed": [],
                },
            )
            with self.assertRaises(ValueError):
                verify_workflows(allowlist_path, wf)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--allowlist", type=Path, default=DEFAULT_ALLOWLIST)
    parser.add_argument("--workflows-dir", type=Path, default=DEFAULT_WORKFLOWS_DIR)
    parser.add_argument("--self-test", action="store_true", help="Run embedded unit tests")
    args = parser.parse_args()

    if args.self_test:
        suite = unittest.defaultTestLoader.loadTestsFromTestCase(CheckActionsAllowlistTests)
        result = unittest.TextTestRunner(verbosity=1).run(suite)
        return 0 if result.wasSuccessful() else 1

    try:
        count, errors = verify_workflows(args.allowlist, args.workflows_dir)
    except Exception as exc:  # noqa: BLE001
        print(f"check-actions-allowlist: error: {exc}", file=sys.stderr)
        return 1

    if errors:
        for err in errors:
            print(f"check-actions-allowlist: {err}", file=sys.stderr)
        return 1

    print(f"check-actions-allowlist: verified {count} SHA-pinned allowlisted uses references")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
