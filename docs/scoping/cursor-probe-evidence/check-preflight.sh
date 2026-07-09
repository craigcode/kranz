#!/usr/bin/env bash
# Offline validator for the Cursor CLI preflight probe evidence (feature f-1-1).
# Run from anywhere; paths are resolved relative to this script.
set -euo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
JSON="$DIR/probe-result.json"
MD="$DIR/preflight.md"
FAIL=0

fail() {
  echo "FAIL: $1" >&2
  FAIL=1
}

pass() {
  echo "PASS: $1"
}

if [[ ! -f "$JSON" ]]; then
  fail "probe-result.json does not exist at $JSON"
else
  pass "probe-result.json exists"
fi

if [[ ! -f "$MD" ]]; then
  fail "preflight.md does not exist at $MD"
else
  pass "preflight.md exists"
fi

if [[ -f "$JSON" ]]; then
  if jq -e 'has("cli_version") and (.flags|length>0) and (.auth_usable!=null)' "$JSON" >/dev/null 2>&1; then
    pass "probe-result.json has cli_version, non-empty flags, and non-null auth_usable"
  else
    fail "probe-result.json missing cli_version / flags / auth_usable per required shape"
  fi

  for flag in --print --output-format --model --workspace --sandbox; do
    if jq -e --arg f "$flag" '.flags | index($f) != null' "$JSON" >/dev/null 2>&1; then
      pass "flags array contains $flag"
    else
      fail "flags array missing required flag: $flag"
    fi
  done

  for key in default grok invalid; do
    val=$(jq -r --arg k "$key" '.model_matrix[$k] // ""' "$JSON")
    if [[ -n "$val" ]]; then
      pass "model_matrix.$key is non-empty"
    else
      fail "model_matrix.$key is empty or missing"
    fi
  done
fi

if [[ -f "$MD" ]]; then
  for needle in "agent --version" "agent status" "agent models"; do
    if grep -q -- "$needle" "$MD"; then
      pass "preflight.md references '$needle'"
    else
      fail "preflight.md missing a reference to '$needle'"
    fi
  done

  # Sanity: captured output content should be present, not just the command names.
  if grep -q "2026.04.13-a9d7fb5" "$MD"; then
    pass "preflight.md contains captured --version output"
  else
    fail "preflight.md does not contain captured --version output"
  fi

  if grep -qi "no models available" "$MD"; then
    pass "preflight.md contains captured agent models output"
  else
    fail "preflight.md does not contain captured agent models output"
  fi

  if grep -qi "login successful" "$MD"; then
    pass "preflight.md contains captured agent status output"
  else
    fail "preflight.md does not contain captured agent status output"
  fi
fi

if [[ -n "${KRANZ_BASE_SHA:-}" ]]; then
  if command -v git >/dev/null 2>&1; then
    CRATES_DIFF="$(git diff --name-only "$KRANZ_BASE_SHA" -- 'crates/' 2>/dev/null || true)"
    if [[ -z "$CRATES_DIFF" ]]; then
      pass "no files under crates/ modified relative to \$KRANZ_BASE_SHA"
    else
      fail "files under crates/ were modified: $CRATES_DIFF"
    fi
  fi
else
  echo "SKIP: KRANZ_BASE_SHA not set, cannot check crates/ diff"
fi

if [[ "$FAIL" -eq 0 ]]; then
  echo "All checks passed."
  exit 0
else
  echo "One or more checks failed." >&2
  exit 1
fi
