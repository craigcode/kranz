#!/usr/bin/env bash
set -euo pipefail

# Cargo packages are independent copies of the software. Keep each package's
# MIT notice byte-identical to the repository license and prove it is actually
# present in the archive Cargo will upload.
packages=(
  'kranz-engine:crates/engine'
  'kranz-server:crates/server'
  'kranz-slack:crates/slack'
  'kranz:crates/cli'
)
listing="$(mktemp)"
trap 'rm -f "$listing"' EXIT

for entry in "${packages[@]}"; do
  package="${entry%%:*}"
  crate_dir="${entry#*:}"
  cmp LICENSE "$crate_dir/LICENSE" || {
    echo "package license check: $crate_dir/LICENSE differs from the root LICENSE" >&2
    exit 1
  }
  cargo package --locked --allow-dirty --list -p "$package" > "$listing"
  grep -qx 'LICENSE' "$listing" || {
    echo "package license check: $package archive omits LICENSE" >&2
    exit 1
  }
  if [ "$package" = kranz ]; then
    grep -qx 'assets/THIRD_PARTY_NOTICES.txt' "$listing"
    grep -qx 'assets/dashboard/dist/THIRD_PARTY_NOTICES.txt' "$listing"
  fi
done

echo 'package license check: all four archives contain the canonical MIT notice'
