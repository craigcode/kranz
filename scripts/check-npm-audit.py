#!/usr/bin/env python3
"""Fail on any high or critical npm advisory that is not waived below.

CI ran `npm audit --audit-level=high` in each JavaScript package. That
command has no way to set one advisory aside, so an advisory with no fixed
version failed every pull request until the affected tools were downgraded.
This script keeps the same bar and adds a short, dated list of waivers.

Usage:
    check-npm-audit.py <package directory>
    check-npm-audit.py --self-test

What it checks, in the package directory:

1. `npm audit --omit=dev --json`: the dependencies that ship. Any high or
   critical advisory fails. Waivers never apply here.
2. `npm audit --json`: everything, build and test tools included. Any high
   or critical advisory fails unless its GHSA id is in WAIVERS.

A waiver names one advisory. It does not cover a package, a severity or a
later advisory against the same package. Remove an entry as soon as the
script reports it is no longer needed.
"""

from __future__ import annotations

import json
import subprocess
import sys

# GHSA id -> why it is waived. Keep the date and the removal condition.
WAIVERS = {
    "GHSA-fc5q-39xc-vjpf": (
        "picomatch ReDoS through repeated extglobs. Waived by the operator on "
        "2026-11-17. No fixed picomatch exists; npm's only fix is a forced "
        "downgrade of vite and vitest by two major versions. picomatch is "
        "reached only through build and test tools, which check 1 confirms, "
        "and the pattern would have to come from the repository's own build "
        "configuration. Remove when a fixed picomatch is released."
    ),
}

BLOCKING = {"high", "critical"}


def ghsa_id(url: str) -> str:
    return url.rstrip("/").rsplit("/", 1)[-1]


def blocking_advisories(report: dict) -> dict[str, str]:
    """Map each high or critical advisory's GHSA id to "<package>: <title>".

    An entry in `vulnerabilities` lists what makes a package vulnerable in
    `via`: an advisory object for a direct cause, or another package's name
    for an inherited one. Only the advisory objects are counted, so one
    advisory is one finding however many packages inherit it.
    """
    found: dict[str, str] = {}
    for package, entry in report.get("vulnerabilities", {}).items():
        for via in entry.get("via", []):
            if isinstance(via, dict) and via.get("severity") in BLOCKING:
                found[ghsa_id(str(via.get("url", "")))] = (
                    f"{package}: {via.get('title', 'no title')}"
                )
    return found


def blocking_count(report: dict) -> int:
    counts = report.get("metadata", {}).get("vulnerabilities", {})
    return sum(int(counts.get(level, 0)) for level in BLOCKING)


def judge(shipped: dict, everything: dict) -> tuple[list[str], list[str]]:
    """Return (failures, notes) for the two audit reports."""
    failures: list[str] = []
    notes: list[str] = []

    for label, report in (("shipped", shipped), ("all", everything)):
        if "vulnerabilities" not in report or "metadata" not in report:
            failures.append(f"{label} dependencies: npm audit gave no report")
        elif blocking_count(report) and not blocking_advisories(report):
            # Packages are flagged but no advisory object explains them. Do
            # not pass on a report this script cannot read.
            failures.append(
                f"{label} dependencies: {blocking_count(report)} high or critical "
                "package(s) flagged with no advisory named"
            )

    for ghsa, what in sorted(blocking_advisories(shipped).items()):
        failures.append(f"shipped dependency, never waived: {ghsa} {what}")

    seen = blocking_advisories(everything)
    for ghsa, what in sorted(seen.items()):
        if ghsa in WAIVERS:
            notes.append(f"waived: {ghsa} {what}")
        elif ghsa not in blocking_advisories(shipped):
            failures.append(f"{ghsa} {what}")
    for ghsa in sorted(set(WAIVERS) - set(seen)):
        notes.append(f"waiver no longer needed, remove it: {ghsa}")
    return failures, notes


def run_audit(directory: str, extra: list[str]) -> dict:
    # npm audit exits non-zero when it finds anything, so the exit code is
    # not the signal here. The JSON report is.
    result = subprocess.run(
        ["npm", "audit", "--json", *extra],
        cwd=directory,
        capture_output=True,
        text=True,
        check=False,
    )
    try:
        return json.loads(result.stdout)
    except json.JSONDecodeError:
        sys.stderr.write(result.stderr)
        return {}


def self_test() -> int:
    def report(advisories: dict[str, tuple[str, str]], inherited: int = 0) -> dict:
        vulnerabilities = {
            package: {
                "severity": severity,
                "via": [
                    {
                        "severity": severity,
                        "title": "t",
                        "url": f"https://github.com/advisories/{ghsa}",
                    }
                ],
            }
            for ghsa, (package, severity) in advisories.items()
        }
        for index in range(inherited):
            vulnerabilities[f"inherits-{index}"] = {"severity": "high", "via": ["x"]}
        counts = {"high": 0, "critical": 0}
        for entry in vulnerabilities.values():
            if entry["severity"] in counts:
                counts[entry["severity"]] += 1
        return {"vulnerabilities": vulnerabilities, "metadata": {"vulnerabilities": counts}}

    waived = next(iter(WAIVERS))
    clean = report({})
    cases = [
        ("nothing found", clean, clean, 0),
        ("waived advisory in tools only", clean, report({waived: ("picomatch", "high")}, 5), 0),
        (
            "unwaived advisory in tools",
            clean,
            report({waived: ("picomatch", "high"), "GHSA-aaaa-bbbb-cccc": ("other", "high")}),
            1,
        ),
        ("critical advisory in tools", clean, report({"GHSA-aaaa-bbbb-cccc": ("o", "critical")}), 1),
        ("moderate advisory is below the bar", clean, report({"GHSA-aaaa-bbbb-cccc": ("o", "moderate")}), 0),
        (
            "waived advisory in a shipped dependency",
            report({waived: ("picomatch", "high")}),
            report({waived: ("picomatch", "high")}),
            1,
        ),
        ("flagged packages with no advisory named", clean, report({}, 2), 1),
        ("no report at all", {}, {}, 2),
    ]
    failed = False
    for name, shipped, everything, expected in cases:
        failures, _ = judge(shipped, everything)
        if len(failures) != expected:
            failed = True
            print(f"SELF-TEST FAIL: {name}: expected {expected} failure(s), got {failures}")
    if failed:
        return 1
    print(f"self-test ok: {len(cases)} cases")
    return 0


def main(argv: list[str]) -> int:
    if len(argv) != 2:
        sys.stderr.write(__doc__ or "")
        return 2
    if argv[1] == "--self-test":
        return self_test()
    directory = argv[1]
    failures, notes = judge(
        run_audit(directory, ["--omit=dev"]),
        run_audit(directory, []),
    )
    for note in notes:
        print(f"NOTE: {note}")
    for failure in failures:
        print(f"FAIL: {failure}")
    if failures:
        return 1
    print(f"OK: no unwaived high or critical npm advisory in {directory}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
