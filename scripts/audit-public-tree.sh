#!/usr/bin/env bash
set -euo pipefail

# Operator vocabulary is supplied privately; never encode it in public source.
python3 "$(dirname "$0")/audit-operator-markers.py" tree

# Deterministically reject real macOS home paths (/Users/<user>) in tracked content,
# allowing only documented synthetic placeholders and reviewed audit/gate regex citations.
python3 - <<'PY'
import os
from pathlib import Path
import re
import subprocess
import sys

USER_PATH_RE = re.compile(rb"/Users/([A-Za-z0-9._-]+)")
ALLOWED_USERS = {b"alice", b"op", b"x"}
EXEMPT_PATHS = {
    "scripts/audit-public-tree.sh",
    "crates/engine/src/contract_gates.rs",
    ".kranz/missions/m-73ada5/plan.json",
    ".kranz/missions/m-73ada5/plan.md",
    ".kranz/missions/m-73ada5/report.md",
}


def git(*args: str) -> bytes:
    return subprocess.check_output(
        ["git", "-c", "core.fsmonitor=false", *args],
        stderr=subprocess.DEVNULL,
    )


def line_exempt(rel_str: str, line: bytes) -> bool:
    if rel_str in EXEMPT_PATHS:
        return True
    if (
        rel_str == "docs/reviews/adversarial-audit-2026-09-01/audit-surface.md"
        and b"`-F` is case-sensitive:" in line
    ):
        return True
    return False


root = Path(os.fsdecode(git("rev-parse", "--show-toplevel").strip()))
paths = git("ls-files", "--cached", "-z").split(b"\0")
hits = 0

for relative in paths:
    if not relative:
        continue
    rel_str = os.fsdecode(relative)
    path = root / rel_str
    if path.is_symlink():
        data = os.fsencode(os.readlink(path))
    elif path.exists():
        data = path.read_bytes()
    else:
        data = git("show", f":{rel_str}")

    if any(m.group(1) not in ALLOWED_USERS for m in USER_PATH_RE.finditer(relative)):
        hits += 1
        continue

    file_hit = False
    for line in data.splitlines():
        if line_exempt(rel_str, line):
            continue
        for match in USER_PATH_RE.finditer(line):
            if match.group(1) not in ALLOWED_USERS:
                file_hit = True
                break
        if file_hit:
            break
    if file_hit:
        hits += 1

if hits:
    print(
        f"public-tree home-path audit: {hits} tracked file(s) contain disallowed /Users/<name> paths",
        file=sys.stderr,
    )
    sys.exit(1)
PY

printf 'public-tree audit: ok\n'
