#!/usr/bin/env python3
import importlib.util
from pathlib import Path
import tarfile
import tempfile
import unittest
import zipfile
import sys

sys.dont_write_bytecode = True

spec = importlib.util.spec_from_file_location("package_release", Path(__file__).with_name("package-release.py"))
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)


class ReleaseArchiveTests(unittest.TestCase):
    def test_all_release_archives_retain_binary_permissions_and_notice_bytes(self):
        targets = ("linux-x86_64", "macos-aarch64", "macos-x86_64", "windows-x86_64", "windows-aarch64")
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            binary = root / "binary"
            binary.write_bytes(b"fixture executable")
            binary.chmod(0o755)
            rust = root / "COPYRIGHT-library.html"
            rust.write_text("<p>fixture runtime copyright</p>")
            for target in targets:
                asset = "kranz-" + target
                output = release.package(binary, asset, root / "out", rust)
                if target.startswith("windows-"):
                    with zipfile.ZipFile(output) as archive:
                        contents = {Path(p).name: archive.read(p) for p in archive.namelist()}
                else:
                    with tarfile.open(output) as archive:
                        contents = {Path(p.name).name: archive.extractfile(p).read() for p in archive if p.isfile()}
                        self.assertEqual(archive.getmember(asset + "/kranz").mode & 0o777, 0o755)
                self.assertEqual(len(contents), 6)
                self.assertEqual(contents["kranz.exe" if target.startswith("windows-") else "kranz"], binary.read_bytes())
                self.assertEqual(contents["LICENSE"], (release.ROOT / "LICENSE").read_bytes())
                self.assertEqual(contents["THIRD_PARTY_NOTICES.txt"], (release.ROOT / "crates/cli/assets/THIRD_PARTY_NOTICES.txt").read_bytes())
                self.assertEqual(contents["DASHBOARD_THIRD_PARTY_NOTICES.txt"], (release.ROOT / "crates/cli/assets/dashboard/dist/THIRD_PARTY_NOTICES.txt").read_bytes())
                self.assertEqual(contents["RUST_LIBRARY_NOTICES.html"], rust.read_bytes())

    def test_missing_runtime_notice_refuses_archive(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            binary = root / "binary"
            binary.write_bytes(b"fixture executable")
            with self.assertRaises(ValueError):
                release.package(binary, "kranz-linux-x86_64", root / "out", root / "absent")
            self.assertFalse((root / "out").exists())


if __name__ == "__main__":
    unittest.main()
