"""Run only the expected, attested release binary on its matching native host."""

import hashlib
import os
import pathlib
import struct
import subprocess
import tarfile
import tempfile
import zipfile


PLATFORMS = {
    "kranz-linux-x86_64": ("tar.gz", "Linux", "X64", "elf", 62),
    "kranz-macos-aarch64": ("tar.gz", "macOS", "ARM64", "macho", 0x100000C),
    "kranz-macos-x86_64": ("tar.gz", "macOS", "X64", "macho", 0x1000007),
    "kranz-windows-x86_64": ("zip", "Windows", "X64", "pe", 0x8664),
    "kranz-windows-aarch64": ("zip", "Windows", "ARM64", "pe", 0xAA64),
}


def check_binary(data, kind, machine):
    if kind == "elf":
        assert data[:6] == b"\x7fELF\x02\x01"
        assert struct.unpack_from("<H", data, 18)[0] == machine
    elif kind == "macho":
        assert struct.unpack_from("<II", data) == (0xFEEDFACF, machine)
    else:
        assert data[:2] == b"MZ"
        offset = struct.unpack_from("<I", data, 0x3C)[0]
        assert data[offset:offset + 4] == b"PE\0\0"
        assert struct.unpack_from("<H", data, offset + 4)[0] == machine


def check_cli(binary, cwd, version):
    # The archive needs no workflow credentials or operator configuration.
    environment = {
        key: value for key, value in os.environ.items()
        if key.upper() in {"PATH", "SYSTEMROOT", "WINDIR"}
    }
    environment.update({key: str(cwd) for key in
                        ["HOME", "USERPROFILE", "TMPDIR", "TEMP", "TMP"]})

    def output(*args):
        return subprocess.check_output(
            [str(binary), *args], cwd=cwd, env=environment,
            encoding="utf-8", timeout=30
        )

    actual = output("--version").strip()
    assert actual == f"kranz {version}", actual
    help_text = output("--help")
    assert all(command in help_text for command in ["serve", "draft", "licenses"])
    notices = output("licenses")
    assert f"kranz {version}" in notices and "MIT License" in notices
    print(f"{actual}: --version, --help, and licenses passed outside a checkout")


def main():
    asset = os.environ["KRANZ_SMOKE_ASSET"]
    ext, host_os, host_arch, kind, machine = PLATFORMS[asset]
    assert os.environ["RUNNER_OS"] == host_os
    assert os.environ["RUNNER_ARCH"] == host_arch
    archive = pathlib.Path("archive") / f"{asset}.{ext}"
    repo = os.environ["GITHUB_REPOSITORY"]
    sha = os.environ["KRANZ_SMOKE_SOURCE_SHA"]
    ref = os.environ["KRANZ_SMOKE_SOURCE_REF"]
    # Failed provenance verification stops execution; no unverified fallback.
    subprocess.run(
        ["gh", "attestation", "verify", str(archive), "--repo", repo,
         "--signer-workflow", repo + "/.github/workflows/release.yml",
         "--source-ref", ref, "--source-digest", sha, "--deny-self-hosted-runners"],
        check=True, timeout=120,
    )
    binary_name = "kranz.exe" if kind == "pe" else "kranz"
    member = asset + "/" + binary_name
    if ext == "zip":
        with zipfile.ZipFile(archive) as package:
            assert package.namelist().count(member) == 1
            data = package.read(member)
    else:
        with tarfile.open(archive) as package:
            entries = [entry for entry in package.getmembers() if entry.name == member]
            assert len(entries) == 1 and entries[0].isfile()
            data = package.extractfile(entries[0]).read()
    check_binary(data, kind, machine)
    with tempfile.TemporaryDirectory(prefix="kranz-archive-smoke-") as directory:
        root = pathlib.Path(directory)
        binary = root / binary_name
        binary.write_bytes(data)
        binary.chmod(0o755)
        check_cli(binary, root, os.environ["KRANZ_SMOKE_VERSION"])
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    receipt = f"{asset}: native {host_os} {host_arch}; source {sha}; archive SHA-256 {digest}"
    print(receipt)
    with open(os.environ["GITHUB_STEP_SUMMARY"], "a", encoding="utf-8") as summary:
        summary.write(receipt + "\n")


if __name__ == "__main__":
    main()
