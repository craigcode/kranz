#!/usr/bin/env python3
"""Verify the narrow desktop patches and prevent a registry fallback."""
import hashlib
import json
from pathlib import Path
import sys
import tomllib

root = Path(__file__).resolve().parent.parent
base = root / "apps/dashboard/src-tauri"
manifest = json.loads((base / "vendor/backports.json").read_text())
locks = [tomllib.loads((base / path).read_text()) for path in ["Cargo.lock", "security-tests/Cargo.lock"]]
packages = locks[0]["package"]
for crate in manifest["crates"]:
    name, version = crate["name"], crate["version"]
    directory = base / "vendor" / name
    actual = {p.relative_to(directory).as_posix(): hashlib.sha256(p.read_bytes()).hexdigest()
              for p in directory.rglob("*") if p.is_file() and "target" not in p.relative_to(directory).parts}
    if actual != crate["patchedFiles"]:
        sys.exit(f"desktop backport contents drifted: {name}")
    changed = {path for path in set(crate["upstreamFiles"]) | set(actual)
               if crate["upstreamFiles"].get(path) != actual.get(path)}
    expected = set(crate["patchFiles"])
    if changed != expected:
        sys.exit(f"unexpected desktop backport patch scope: {name}")
    resolved = [p for p in packages if p["name"] == name]
    if len(resolved) != 1 or resolved[0]["version"] != version or "source" in resolved[0]:
        sys.exit(f"desktop backport bypassed by lockfile: {name}")
if any(p["name"] in {"proc-macro-error", "proc-macro-error-attr", "proc-macro-error2", "proc-macro-error-attr2"}
       for lock in locks for p in lock["package"]):
    sys.exit("unmaintained macro helper returned to the desktop dependency graph")
print("desktop backports: verified source hashes, narrow patch scope, and resolved path dependencies")
