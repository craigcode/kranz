#!/usr/bin/env bash
# Validator for finding f-2-2 acceptance_bar item
# 'model_availability_failures_deterministic_readable' / preflight 'avoid
# spending' rationale (feature ms-2-fix-1-2).
#
# The finding: preflight.md claimed `agent --print` was "intentionally NOT
# run (would spend money)" / "never invoked", contradicting
# cursor-cli-backend.md's record that `--print` was run twice (json and
# stream-json) and failed free at the auth gate with "Authentication
# required" before any billed turn. This script asserts that contradiction
# no longer exists: preflight.md and probe-result.json must record the free
# auth-gate failure and must not claim --print was never invoked.
set -euo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MD="$DIR/preflight.md"
JSON="$DIR/probe-result.json"
BACKEND_MD="$DIR/../cursor-cli-backend.md"
FAIL=0

fail() { echo "FAIL: $1" >&2; FAIL=1; }
pass() { echo "PASS: $1"; }

# 1. preflight.md must document the free auth-gate --print failure.
if grep -qi "Authentication required" "$MD"; then
  pass "preflight.md records the free 'Authentication required' auth-gate failure"
else
  fail "preflight.md does not record the 'Authentication required' auth-gate failure"
fi

# 2. preflight.md must no longer claim --print was never invoked / not run
#    (would spend money) without qualification.
if grep -Eqi "print.{0,20}(was )?(intentionally )?(never|not) (run|invoked)" "$MD" \
   && ! grep -qi "Authentication required" "$MD"; then
  fail "preflight.md still contains an unqualified '--print never run/invoked' claim"
else
  pass "preflight.md does not contain an unqualified '--print never run/invoked' claim"
fi

if grep -qi "was never invoked" "$MD" && ! grep -qi "run twice" "$MD"; then
  fail "preflight.md still asserts --print 'was never invoked' without reconciling the two runs"
else
  pass "preflight.md reconciles the two --print invocations"
fi

# 3. probe-result.json blocker/acceptance_bar must not claim --print was
#    never run either.
if [[ -f "$JSON" ]]; then
  blocker=$(jq -r '.blocker' "$JSON")
  if echo "$blocker" | grep -qi "was never run\|intentionally not run to avoid spending"; then
    fail "probe-result.json.blocker still claims --print was never run"
  else
    pass "probe-result.json.blocker does not claim --print was never run"
  fi
  if echo "$blocker" | grep -qi "Authentication required"; then
    pass "probe-result.json.blocker records the free auth-gate failure"
  else
    fail "probe-result.json.blocker does not record the free auth-gate failure"
  fi

  terminal=$(jq -r '.acceptance_bar.terminal_text_stitching' "$JSON")
  if echo "$terminal" | grep -qi "agent --print was never run"; then
    fail "probe-result.json.acceptance_bar.terminal_text_stitching still claims --print was never run"
  else
    pass "probe-result.json.acceptance_bar.terminal_text_stitching does not claim --print was never run"
  fi

  model_avail=$(jq -r '.acceptance_bar.model_availability_failures_deterministic_readable' "$JSON")
  if echo "$model_avail" | grep -qi "Authentication required"; then
    pass "probe-result.json.acceptance_bar.model_availability_failures_deterministic_readable records the auth-gate evidence"
  else
    fail "probe-result.json.acceptance_bar.model_availability_failures_deterministic_readable missing auth-gate evidence"
  fi

  jq -e . "$JSON" >/dev/null && pass "probe-result.json is valid JSON" || fail "probe-result.json is not valid JSON"
fi

# 4. cursor-cli-backend.md (already reconciled by ms-2-fix-1-1) must stay
#    consistent with preflight.md: both must agree --print failed free at
#    the auth gate rather than being wholly un-invoked.
if [[ -f "$BACKEND_MD" ]]; then
  if grep -qi "Authentication required" "$BACKEND_MD" && grep -qi "Authentication required" "$MD"; then
    pass "cursor-cli-backend.md and preflight.md agree on the free auth-gate --print failure"
  else
    fail "cursor-cli-backend.md and preflight.md disagree on whether --print was invoked"
  fi
fi

if [[ "$FAIL" -eq 0 ]]; then
  echo "All checks passed."
  exit 0
else
  echo "One or more checks failed." >&2
  exit 1
fi
