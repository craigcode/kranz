#!/usr/bin/env python3
"""Malformed derives must produce normal diagnostics, never a macro panic."""
from pathlib import Path
import subprocess

root = Path(__file__).resolve().parent.parent
manifest = root / "apps/dashboard/src-tauri/security-tests/Cargo.toml"
for example, expected in [
    ("invalid-glib", "#[derive(glib::Enum)] only supports enums"),
    ("invalid-gtk", "derive(CompositeTemplate) requires #[template(...)]"),
]:
    result = subprocess.run([
        "cargo", "check", "--locked", "--release", "--manifest-path", str(manifest),
        "--example", example, "--features", "compile-fail",
    ], cwd=root, text=True, capture_output=True, timeout=180)
    if result.returncode == 0 or expected not in result.stderr or "proc-macro derive panicked" in result.stderr:
        raise SystemExit(f"{example}: expected compiler diagnostic was not emitted\n{result.stderr}")
    print(f"{example}: rejected with the expected span-aware compiler diagnostic")
