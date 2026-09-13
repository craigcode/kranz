#!/usr/bin/env python3
"""Scan tracked content or all reachable Git objects for private operator markers.

Markers are case-sensitive UTF-8 literals from KRANZ_PUBLIC_AUDIT_MARKERS_FILE and/or
KRANZ_PUBLIC_AUDIT_MARKERS. Blank lines and lines starting with # are ignored.
Only counts are reported: private vocabulary and matched content stay private.
"""
import argparse
import os
from pathlib import Path
import subprocess
import sys


class AuditError(Exception):
    """A public diagnostic that contains no vocabulary, paths or Git data."""


def load_markers():
    required = os.environ.get("KRANZ_REQUIRE_OPERATOR_MARKERS", "0")
    if required not in ("0", "1"):
        raise AuditError("KRANZ_REQUIRE_OPERATOR_MARKERS must be 0 or 1")
    sources = [os.environ.get("KRANZ_PUBLIC_AUDIT_MARKERS", "")]
    filename = os.environ.get("KRANZ_PUBLIC_AUDIT_MARKERS_FILE")
    if filename:
        try:
            sources.append(Path(filename).read_text(encoding="utf-8-sig"))
        except (OSError, UnicodeError) as error:
            raise AuditError("operator markers file cannot be read as UTF-8") from error
    markers = set()
    for source in sources:
        for line in source.removeprefix("\ufeff").splitlines():
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            if "\0" in line:
                raise AuditError("operator markers may not contain NUL")
            markers.add(line.encode("utf-8"))
    if required == "1" and not markers:
        raise AuditError("operator markers are required but no vocabulary was supplied")
    return sorted(markers)


def git(*args):
    return subprocess.check_output(["git", "-c", "core.fsmonitor=false", *args],
                                   stderr=subprocess.DEVNULL)


def scan_tree(markers):
    root = Path(os.fsdecode(git("rev-parse", "--show-toplevel").strip()))
    paths = git("ls-files", "--cached", "-z").split(b"\0")
    hits = 0
    for relative in paths:
        if not relative:
            continue
        path = root / os.fsdecode(relative)
        # A symlink's target text is what Git publishes, not the external
        # file it might point at on this operator's machine.
        data = os.fsencode(os.readlink(path)) if path.is_symlink() else path.read_bytes()
        if any(marker in relative or marker in data for marker in markers):
            hits += 1
    return hits


def scan_history(markers):
    if git("rev-parse", "--is-shallow-repository").strip() != b"false":
        raise AuditError("full history is required")
    objects = git("rev-list", "--objects", "--all", "--no-object-names").splitlines()
    if not objects:
        raise AuditError("history inventory is empty")
    refs = git("for-each-ref", "--format=%(refname)%00%(taggername)%00%(taggeremail)%00%(contents)")
    hits = int(any(marker in refs for marker in markers))
    process = subprocess.Popen(["git", "cat-file", "--batch"], stdin=subprocess.PIPE,
                               stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
    try:
        for oid in objects:
            process.stdin.write(oid + b"\n")
            process.stdin.flush()
            header = process.stdout.readline().split()
            if len(header) != 3 or header[0] != oid:
                raise AuditError("Git object inventory cannot be verified")
            size = int(header[2])
            data = process.stdout.read(size)
            if len(data) != size or process.stdout.read(1) != b"\n":
                raise AuditError("Git object read was incomplete")
            if any(marker in data for marker in markers):
                hits += 1
        process.stdin.close()
        if process.wait() != 0:
            raise AuditError("Git object scan failed")
    finally:
        if process.poll() is None:
            process.kill()
            process.wait()
        process.stdout.close()
        if not process.stdin.closed:
            process.stdin.close()
    return hits


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("scope", choices=("tree", "history"))
    args = parser.parse_args()
    try:
        markers = load_markers()
        if not markers:
            print("operator-marker audit: no additional vocabulary configured; optional scan skipped")
            return 0
        hits = scan_tree(markers) if args.scope == "tree" else scan_history(markers)
    except AuditError as error:
        print(f"operator-marker audit: {error}", file=sys.stderr)
        return 2
    except (OSError, ValueError, subprocess.SubprocessError):
        # Exceptions can carry a private path, vocabulary or Git output.
        print("operator-marker audit: configuration or Git/content read failed; "
              "supply a readable nonempty vocabulary when required", file=sys.stderr)
        return 2
    if hits:
        print(f"operator-marker audit: {hits} matching {args.scope} entries; "
              "review privately before publication", file=sys.stderr)
        return 1
    print(f"operator-marker audit: {args.scope} passed with {len(markers)} supplied markers")
    return 0


if __name__ == "__main__":
    sys.exit(main())
