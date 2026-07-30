#!/usr/bin/env bash
# Regression test for finding a8: validation environments with no rustup
# default configured (RUSTUP_HOME pointed at a fresh, empty home) fail every
# `cargo ...` invocation with:
#
#   error: rustup could not choose a version of cargo to run, because one
#   wasn't specified explicitly, and no default is configured.
#
# A repo-root rust-toolchain.toml makes rustup pick a toolchain via directory
# override, independent of whatever (if any) default is configured in the
# invoking environment's rustup install. This script guards against that file
# being removed or left without a channel pin.
#
# Usage: check-rust-toolchain-pinned.sh [path-to-repo-root]
# Defaults to the repo root resolved relative to this script's location.
set -euo pipefail

ROOT="${1:-$(dirname "$0")/../../..}"
FILE="$ROOT/rust-toolchain.toml"

fail() {
  echo "FAIL [$1]: expected $2, observed $3" >&2
  exit 1
}

[ -f "$FILE" ] || fail "rust-toolchain-pinned" "$FILE to exist" "not found"

grep -Eq '^\[toolchain\]' "$FILE" \
  || fail "rust-toolchain-pinned" \
    "$FILE to declare a [toolchain] table" \
    "no [toolchain] table found"

grep -Eq '^channel\s*=\s*"[^"]+"' "$FILE" \
  || fail "rust-toolchain-pinned" \
    "$FILE to pin a channel (e.g. channel = \"stable\")" \
    "no channel key found"

echo "RUST-TOOLCHAIN-PINNED: PASS (rust-toolchain.toml pins a channel, so cargo resolves a toolchain regardless of the environment's rustup default)"
