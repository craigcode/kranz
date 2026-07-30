#!/usr/bin/env bash
# Validator for finding f-1-1 (feature ms-1-fix-1-4):
# "records the probed `bd --version` output" / plan 1.1 "recording the
# VERBATIM output (or a faithful excerpt) of read-only probes ... `bd
# --help` (top-level subcommand list)".
#
# The finding: docs/scoping/beads-bridge-dialect.md named `bd --help` as a
# required probe but recorded no output for it anywhere -- the Appendix
# contained only a one-line hand-summary ("confirms `ready`, `show`,
# `update`, `comment`, `close`, `set-state`, `init` all exist") with no verb
# list to check it against, and none of the six VERIFIED rows in the §2
# table had a transcript backing the verdict. This script asserts:
#   1. the Appendix contains the actual `bd --help` top-level subcommand
#      listing (not just the hand-summary), covering many more verbs than
#      the six the bridge cares about;
#   2. every one of the six §2-table subcommands has a fenced `--help`
#      transcript block backing its VERIFIED verdict;
#   3. the summary line's claims (`ready`, `show`, `update`, `comment`,
#      `close`, `set-state`, `init`) are each actually present as verbs in
#      the captured listing, so the pointer resolves to real evidence.
set -euo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MD="$DIR/beads-bridge-dialect.md"
FAIL=0

fail() { echo "FAIL: $1" >&2; FAIL=1; }
pass() { echo "PASS: $1"; }

if [[ ! -f "$MD" ]]; then
  fail "beads-bridge-dialect.md is missing"
  echo "One or more checks failed." >&2
  exit 1
fi

# 1. The Appendix must contain the actual bd --help top-level subcommand
#    listing, not just the one-line hand-summary. Check for a representative
#    sample of verbs that appear ONLY in the real listing, never in the
#    six-verb hand-summary sentence.
listing_verbs=(assign children gate sql doctor worktree federation ping)
missing_listing_verbs=()
for v in "${listing_verbs[@]}"; do
  if ! grep -qE "^\s*${v}\s" "$MD"; then
    missing_listing_verbs+=("$v")
  fi
done
if [[ "${#missing_listing_verbs[@]}" -eq 0 ]]; then
  pass "Appendix contains the full bd --help top-level subcommand listing (sample verbs present: ${listing_verbs[*]})"
else
  fail "Appendix is missing full bd --help listing -- absent verbs: ${missing_listing_verbs[*]}"
fi

# 2. Every §2 subcommand must have a fenced --help transcript block.
subcommands=("bd ready" "bd show" "bd update" "bd comment" "bd close" "bd set-state")
for sub in "${subcommands[@]}"; do
  if grep -qF "<code>${sub} --help</code>" "$MD"; then
    pass "transcript block present for '${sub} --help'"
  else
    fail "no fenced transcript block found for '${sub} --help'"
  fi
done

# 3. The summary-line verb claims must each resolve to a real verb in the
#    captured listing (not just be asserted in the summary sentence itself).
summary_verbs=(ready show update comment close set-state init)
for v in "${summary_verbs[@]}"; do
  # require at least 2 occurrences: one in the summary sentence, one in the
  # actual captured listing block.
  count=$(grep -cE "^\s*${v}\s|\`${v}\`" "$MD" || true)
  if [[ "$count" -ge 2 ]]; then
    pass "summary claim for '${v}' is backed by more than just the summary sentence"
  else
    fail "summary claim for '${v}' is not backed by a captured listing entry"
  fi
done

if [[ "$FAIL" -eq 0 ]]; then
  echo "All checks passed."
  exit 0
else
  echo "One or more checks failed." >&2
  exit 1
fi
