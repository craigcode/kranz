#!/usr/bin/env python3
"""Package one cross-compiled CLI with its project, dependency and runtime notices."""
import argparse
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import tempfile
import zipfile

ROOT = Path(__file__).resolve().parent.parent


def rust_library_notices():
    sysroot = Path(subprocess.check_output(["rustc", "--print", "sysroot"], text=True).strip())
    # Modern rustc ships a library-only inventory. Older toolchains carry
    # the broader compiler inventory; retain that rather than drop notices.
    for name in ("COPYRIGHT-library.html", "COPYRIGHT.html", "COPYRIGHT"):
        source = sysroot / "share/doc/rust" / name
        if source.is_file() and source.stat().st_size:
            return source
    raise RuntimeError("Rust toolchain copyright inventory is missing")


def package(binary, asset, out_dir, rust_notices):
    if not re.fullmatch(r"kranz-(linux|macos|windows)-(x86_64|aarch64)", asset):
        raise ValueError(f"unexpected asset name: {asset}")
    files = {
        "kranz.exe" if asset.startswith("kranz-windows-") else "kranz": binary,
        "LICENSE": ROOT / "LICENSE",
        "THIRD_PARTY_NOTICES.txt": ROOT / "crates/cli/assets/THIRD_PARTY_NOTICES.txt",
        "DASHBOARD_THIRD_PARTY_NOTICES.txt": ROOT / "crates/cli/assets/dashboard/dist/THIRD_PARTY_NOTICES.txt",
        "README.txt": ROOT / "packaging/release/README.txt",
        "RUST_LIBRARY_NOTICES" + (rust_notices.suffix or ".txt"): rust_notices,
    }
    for source in files.values():
        if not source.is_file() or not source.stat().st_size:
            raise ValueError(f"required release file missing or empty: {source}")
    out_dir.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=out_dir) as scratch:
        staged = Path(scratch) / asset
        staged.mkdir()
        for name, source in files.items():
            shutil.copy2(source, staged / name)
        if asset.startswith("kranz-windows-"):
            output = out_dir / f"{asset}.zip"
            with zipfile.ZipFile(output, "w", zipfile.ZIP_DEFLATED) as archive:
                for path in sorted(staged.iterdir()):
                    archive.write(path, f"{asset}/{path.name}")
        else:
            output = out_dir / f"{asset}.tar.gz"
            with tarfile.open(output, "w:gz") as archive:
                archive.add(staged, arcname=asset)
    return output


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--asset", required=True)
    parser.add_argument("--out-dir", type=Path, default=Path("dist"))
    args = parser.parse_args()
    print(package(args.binary, args.asset, args.out_dir, rust_library_notices()))
