#!/usr/bin/env bash
set -euo pipefail

# Fast, deterministic privacy lint for the tree that a public clone receives.
# Secret detection is a separate gate; these markers catch operator metadata
# that is not normally classified as a credential.
markers=(
  '/Users/'craig'martin'
  'craigmartin8008''@''gmail.com'
  'craig''@''craigmartin.com'
  'everything-''evenhub'
)

failed=0
for marker in "${markers[@]}"; do
  if git grep -I -n -F "$marker" -- .; then
    printf 'public-tree audit: tracked content contains forbidden operator marker: %s\n' "$marker" >&2
    failed=1
  fi
done

if [ "$failed" -ne 0 ]; then
  exit 1
fi

printf 'public-tree audit: ok\n'
