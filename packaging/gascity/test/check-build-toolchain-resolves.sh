#!/usr/bin/env bash
# Regression test for finding a11: `cargo build --workspace` failed in
# validation environments with no rustup default toolchain configured, with
# the same error as findings a8, a9, and a10:
#
#   error: rustup could not choose a version of cargo to run, because one
#   wasn't specified explicitly, and no default is configured.
#
# a8's fix (a repo-root rust-toolchain.toml, guarded by
# check-rust-toolchain-pinned.sh) and its siblings (check-clippy-toolchain-
# resolves.sh, check-fmt-toolchain-resolves.sh) cover generic `cargo` and the
# clippy/fmt gates, but none exercises `cargo build --workspace` specifically.
# This script proves `cargo build --workspace` itself resolves a toolchain
# via the rust-toolchain.toml directory override and runs cleanly even with
# no rustup default configured.
#
# Usage: check-build-toolchain-resolves.sh [path-to-repo-root]
# Defaults to the repo root resolved relative to this script's location.
set -euo pipefail

ROOT="${1:-$(cd "$(dirname "$0")/../../.." && pwd)}"

fail() {
  echo "FAIL [$1]: expected $2, observed $3" >&2
  exit 1
}

command -v rustup >/dev/null 2>&1 || fail "build-toolchain-resolves" \
  "rustup to be installed" "rustup not found on PATH"

REAL_RUSTUP_HOME="$(rustup show home 2>/dev/null || echo "${RUSTUP_HOME:-$HOME/.rustup}")"
[ -d "$REAL_RUSTUP_HOME/toolchains" ] || fail "build-toolchain-resolves" \
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

OUTPUT="$(cd "$ROOT" && RUSTUP_HOME="$FAKE_HOME" cargo build --workspace 2>&1)" \
  || fail "build-toolchain-resolves" \
    "cargo build --workspace to succeed from $ROOT with no rustup default configured" \
    "$OUTPUT"

echo "$OUTPUT" | grep -q "no default is configured" && fail \
  "build-toolchain-resolves" \
  "the rust-toolchain.toml directory override to resolve a toolchain" \
  "rustup reported no default is configured: $OUTPUT"

echo "BUILD-TOOLCHAIN-RESOLVES: PASS (cargo build --workspace resolves a toolchain via rust-toolchain.toml even with no rustup default configured, so finding a11's error no longer reproduces)"
