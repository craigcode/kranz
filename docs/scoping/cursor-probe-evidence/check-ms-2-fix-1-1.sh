#!/usr/bin/env bash
# Validator for finding 'ms-2 milestone diff (a13 / f-2-2 deliverables)'
# (feature ms-2-fix-1-1).
#
# The finding: the ms-2 milestone range (2325b58..HEAD) was empty --
# `git rev-list --count 2325b58..HEAD` returned 0 and
# `git diff --name-only 2325b58 HEAD` returned nothing -- even though every
# ms-2 deliverable (the finalized backend_cursor route decision and its
# implementation brief) already existed, authored by ms-1 commits at or
# before the range base. This script asserts the range is no longer empty
# and that ms-2's status (inherited/no-op vs. new work) is recorded
# explicitly rather than left implicit.

set -euo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$DIR/../../.." && pwd)"
STATUS_MD="$DIR/ms-2-status.md"
BASE_SHA="2325b58"
FAIL=0

fail() { echo "FAIL: $1" >&2; FAIL=1; }
pass() { echo "PASS: $1"; }

# 1. The milestone range must no longer be empty.
cd "$REPO_ROOT"
if git cat-file -e "${BASE_SHA}^{commit}" 2>/dev/null; then
  count=$(git rev-list --count "${BASE_SHA}..HEAD" 2>/dev/null || echo 0)
  if [[ "$count" -gt 0 ]]; then
    pass "milestone range ${BASE_SHA}..HEAD is non-empty ($count commit(s))"
  else
    fail "milestone range ${BASE_SHA}..HEAD is still empty (0 commits)"
  fi

  changed=$(git diff --name-only "${BASE_SHA}" HEAD 2>/dev/null || true)
  if [[ -n "$changed" ]]; then
    pass "git diff --name-only ${BASE_SHA} HEAD is non-empty"
  else
    fail "git diff --name-only ${BASE_SHA} HEAD is still empty"
  fi
else
  fail "base commit ${BASE_SHA} is not reachable in this checkout; cannot verify range"
fi

# 2. ms-2's status (inherited/no-op) must be recorded explicitly.
if [[ -f "$STATUS_MD" ]]; then
  pass "ms-2-status.md exists"
  if grep -qi "inherited" "$STATUS_MD" && grep -qi "no-op" "$STATUS_MD"; then
    pass "ms-2-status.md records ms-2 as inherited/no-op"
  else
    fail "ms-2-status.md does not clearly record ms-2 as inherited/no-op"
  fi
  if grep -q "direct-parser" "$STATUS_MD"; then
    pass "ms-2-status.md references the direct-parser route decision"
  else
    fail "ms-2-status.md does not reference the direct-parser route decision"
  fi
else
  fail "ms-2-status.md is missing"
fi

if [[ "$FAIL" -eq 0 ]]; then
  echo "All checks passed."
  exit 0
else
  echo "One or more checks failed." >&2
  exit 1
fi
