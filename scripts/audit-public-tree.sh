#!/usr/bin/env bash
set -euo pipefail

# Fast, deterministic privacy lint for the tree that a public clone receives.
# Operator-specific markers are sensitive too: they must come from an
# untracked file or CI secret, never be disguised in the public source.
markers=()
marker_count=0
marker_file="${KRANZ_PUBLIC_AUDIT_MARKERS_FILE:-}"
require_markers="${KRANZ_REQUIRE_OPERATOR_MARKERS:-0}"
if [ -n "$marker_file" ]; then
  if [ ! -f "$marker_file" ]; then
    printf 'public-tree audit: marker file is not readable: %s\n' "$marker_file" >&2
    exit 1
  fi
  while IFS= read -r marker || [ -n "$marker" ]; do
    marker="${marker%$'\r'}"
    if [ -n "$marker" ]; then
      markers+=("$marker")
      marker_count=$((marker_count + 1))
    fi
  done < "$marker_file"
fi
if [ "$require_markers" = 1 ] && [ "$marker_count" -eq 0 ]; then
  echo 'public-tree audit: release requires a non-empty external operator-marker file' >&2
  exit 1
fi

failed=0
if [ "$marker_count" -gt 0 ]; then
  for marker in "${markers[@]}"; do
    if git grep -I -q -F -e "$marker" -- .; then
      echo 'public-tree audit: tracked content contains a forbidden external operator marker' >&2
      failed=1
    fi
  done
fi

if [ "$failed" -ne 0 ]; then
  exit 1
fi

printf 'public-tree audit: ok (%s external operator marker(s))\n' "$marker_count"
