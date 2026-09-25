#!/usr/bin/env python3
"""Test the packaged ACP crate outside workspace feature unification."""
import json
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile


ROOT = Path(__file__).resolve().parent.parent


def run(args, cwd=ROOT, **kwargs):
    return subprocess.run(args, cwd=cwd, check=True, text=True, **kwargs)


def main():
    metadata = json.loads(run(
        ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"],
        capture_output=True,
    ).stdout)
    package = next(p for p in metadata["packages"] if p["name"] == "kranz-acp")
    run(["cargo", "package", "--locked", "--allow-dirty", "--no-verify", "-p", "kranz-acp"])
    name = f"kranz-acp-{package['version']}"
    archive = Path(metadata["target_directory"]) / "package" / f"{name}.crate"
    with tempfile.TemporaryDirectory(prefix="kranz-acp-isolated-") as temporary:
        with tarfile.open(archive) as source:
            source.extractall(temporary, filter="data")
        crate = Path(temporary) / name
        # Its own target prevents accidental reuse of workspace-unified artifacts.
        env = dict(os.environ, CARGO_TARGET_DIR=str(Path(temporary) / "target"))
        isolated = json.loads(run(
            ["cargo", "metadata", "--locked", "--format-version", "1"],
            cwd=crate, env=env, capture_output=True,
        ).stdout)
        names = {p["name"] for p in isolated["packages"]}
        forbidden = names.intersection({"kranz-engine", "cap-std", "cap-primitives", "libc", "windows-sys"})
        if forbidden:
            raise SystemExit(f"unexpected runtime/engine dependencies: {sorted(forbidden)}")
        tokio_id = next(p["id"] for p in isolated["packages"] if p["name"] == "tokio")
        tokio = next(n for n in isolated["resolve"]["nodes"] if n["id"] == tokio_id)
        forbidden_features = set(tokio["features"]).intersection({"full", "process", "net", "fs", "signal", "rt-multi-thread"})
        if forbidden_features:
            raise SystemExit(f"unexpected Tokio features: {sorted(forbidden_features)}")
        print(f"isolated dependency graph: {len(names)} packages; Tokio features {tokio['features']}", flush=True)
        for args in [
            ["test", "--locked", "--all-features", "--all-targets"],
            ["run", "--locked", "--features", "conformance", "--example", "threaded_client"],
            ["clippy", "--locked", "--all-features", "--all-targets", "--", "-D", "warnings"],
        ]:
            run(["cargo", *args], cwd=crate, env=env)
        run(["cargo", "doc", "--locked", "--no-deps", "--all-features"], cwd=crate,
            env=dict(env, RUSTDOCFLAGS="-D warnings"))


if __name__ == "__main__":
    main()
