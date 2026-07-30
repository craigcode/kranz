#!/usr/bin/env bash
# Regression test for finding a9: `cargo clippy --workspace --all-targets --
# -D warnings` (the merge-gates.json clippy gate) failed in validation
# environments with no rustup default toolchain configured, with the same
# error as finding a8:
#
#   error: rustup could not choose a version of cargo to run, because one
#   wasn't specified explicitly, and no default is configured.
#
# a8's fix (a repo-root rust-toolchain.toml, guarded by
# check-rust-toolchain-pinned.sh) only checks that the file exists and pins a
# channel -- it does not prove cargo actually resolves a toolchain via the
# directory override when no rustup default is set. This script proves it
# end-to-end: it builds an isolated RUSTUP_HOME with installed toolchains
# symlinked in but no default_toolchain configured, then runs `cargo
# --version` from the repo root under that environment and confirms it
# resolves via the rust-toolchain.toml override instead of failing.
#
# Usage: check-clippy-toolchain-resolves.sh [path-to-repo-root]
# Defaults to the repo root resolved relative to this script's location.
set -euo pipefail

ROOT="${1:-$(cd "$(dirname "$0")/../../.." && pwd)}"

fail() {
  echo "FAIL [$1]: expected $2, observed $3" >&2
  exit 1
}

command -v rustup >/dev/null 2>&1 || fail "clippy-toolchain-resolves" \
  "rustup to be installed" "rustup not found on PATH"

REAL_RUSTUP_HOME="$(rustup show home 2>/dev/null || echo "${RUSTUP_HOME:-$HOME/.rustup}")"
[ -d "$REAL_RUSTUP_HOME/toolchains" ] || fail "clippy-toolchain-resolves" \
  "an installed toolchain to exist under $REAL_RUSTUP_HOME/toolchains" \
  "no toolchains directory found"

FAKE_HOME="$(mktemp -d)"
cleanup() { rm -rf "$FAKE_HOME"; }
trap cleanup EXIT

mkdir -p "$FAKE_HOME/toolchains"
for tc in "$REAL_RUSTUP_HOME"/toolchains/*; do
  ln -s "$tc" "$FAKE_HOME/toolchains/$(basename "$tc")"
done

# Deliberately omit `default_toolchain` to reproduce the validation
# environment's "no default is configured" condition.
cat > "$FAKE_HOME/settings.toml" <<'EOF'
default_host_triple = "aarch64-apple-darwin"
profile = "default"
version = "12"
EOF

OUTPUT="$(cd "$ROOT" && RUSTUP_HOME="$FAKE_HOME" cargo --version 2>&1)" \
  || fail "clippy-toolchain-resolves" \
    "cargo --version to succeed from $ROOT with no rustup default configured" \
    "$OUTPUT"

echo "$OUTPUT" | grep -q "no default is configured" && fail \
  "clippy-toolchain-resolves" \
  "the rust-toolchain.toml directory override to resolve a toolchain" \
  "rustup reported no default is configured: $OUTPUT"

echo "CLIPPY-TOOLCHAIN-RESOLVES: PASS (cargo resolves a toolchain via rust-toolchain.toml even with no rustup default configured, so cargo clippy --workspace --all-targets -- -D warnings no longer fails with finding a9's error)"
