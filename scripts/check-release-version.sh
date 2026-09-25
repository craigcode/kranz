#!/usr/bin/env bash
set -euo pipefail

tag="${1:-${GITHUB_REF_NAME:-}}"
if [[ ! "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+([+-][0-9A-Za-z.-]+)?$ ]]; then
  echo "release check: expected an exact semver tag (vX.Y.Z), got '$tag'" >&2
  exit 1
fi

tag_ref="refs/tags/${tag}"
if git show-ref --verify --quiet "$tag_ref"; then
  if [ "${GITHUB_REF_TYPE:-}" != tag ]; then
    echo "release check: $tag_ref already exists; never reuse or move a release tag" >&2
    exit 1
  fi
  if [ "$(git cat-file -t "$tag_ref")" != tag ]; then
    echo "release check: $tag must be an annotated tag" >&2
    exit 1
  fi
  if [ "$(git rev-parse "${tag_ref}^{commit}")" != "$(git rev-parse 'HEAD^{commit}')" ]; then
    echo "release check: $tag does not resolve to the checked-out release commit" >&2
    exit 1
  fi
fi

workspace_version="$(awk '
  /^\[workspace\.package\]$/ { in_package=1; next }
  /^\[/ { in_package=0 }
  in_package && /^version[[:space:]]*=/ {
    gsub(/^[^\"]*\"|\".*/, ""); print; exit
  }
' Cargo.toml)"

tauri_version="$(jq -r .version apps/dashboard/src-tauri/tauri.conf.json)"
tauri_crate_version="$(awk '
  /^\[package\]$/ { in_package=1; next }
  /^\[/ { in_package=0 }
  in_package && /^version[[:space:]]*=/ {
    gsub(/^[^\"]*\"|\".*/, ""); print; exit
  }
' apps/dashboard/src-tauri/Cargo.toml)"

expected_tag="v${workspace_version}"
if [ "$tag" != "$expected_tag" ]; then
  echo "release check: tag $tag does not match workspace version $workspace_version" >&2
  exit 1
fi

if [ "$tauri_version" != "$workspace_version" ] || [ "$tauri_crate_version" != "$workspace_version" ]; then
  echo "release check: root, Tauri config, and Tauri crate versions must match" >&2
  printf 'root=%s tauri-config=%s tauri-crate=%s\n' \
    "$workspace_version" "$tauri_version" "$tauri_crate_version" >&2
  exit 1
fi

for crate in kranz-acp kranz-engine kranz-server kranz-slack; do
  if ! grep -Eq "^${crate} = \\{ version = \"${workspace_version//./\\.}\", path = " Cargo.toml; then
    echo "release check: workspace dependency $crate is not pinned to $workspace_version" >&2
    exit 1
  fi
done

if ! grep -Fq "## ${workspace_version} -" CHANGELOG.md; then
  echo "release check: CHANGELOG.md has no dated ${workspace_version} release section" >&2
  exit 1
fi

# README is also the crates.io readme: every install command and versioned
# package/release link must describe the version being published.
python3 - "$workspace_version" <<'PYTHON'
import pathlib
import re
import sys

version = sys.argv[1]
readme = pathlib.Path("README.md").read_text()
patterns = [
    r"cargo install kranz --version ([^\s]+)",
    r"https://crates\.io/crates/(?:kranz|kranz-acp|kranz-engine|kranz-server|kranz-slack)/([^/)\s]+)",
    r"https://github\.com/craigcode/kranz/releases/tag/v([^/)\s]+)",
]
for pattern in patterns:
    found = re.findall(pattern, readme)
    if not found or any(value != version for value in found):
        sys.exit(f"release check: README install/package/release versions must all be {version}")
PYTHON

if [ "${KRANZ_RELEASE_SKIP_MAIN_CHECK:-0}" != 1 ]; then
  if [ "${GITHUB_ACTIONS:-}" = true ] && [ -n "${GH_TOKEN:-}" ]; then
    # CI supplies authentication only to this fetch, never repository config.
    git -c credential.helper= -c 'credential.helper=!gh auth git-credential' \
      fetch --no-tags origin main
  else
    git fetch --no-tags origin main
  fi
  release_sha="$(git rev-parse 'HEAD^{commit}')"
  main_sha="$(git rev-parse 'FETCH_HEAD^{commit}')"
  if [ "$release_sha" != "$main_sha" ]; then
    echo "release check: tag commit $release_sha is not the current origin/main $main_sha" >&2
    exit 1
  fi
fi

printf 'release version check: %s is aligned at %s\n' "$tag" "$(git rev-parse HEAD)"
