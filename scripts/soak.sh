#!/usr/bin/env bash
# M3 corruption-soak harness (roadmap M3 "done when", corruption half).
#
# Runs the #[ignore]d soak_parallel_corruption test in release mode for
# N iterations (default 20), cycling CLEAN / CRASH+RESUME / CONFLICT parallel
# missions and asserting the full event-log invariant set after every one.
#
# Usage: scripts/soak.sh [iterations]
set -uo pipefail
cd "$(dirname "$0")/.."

ITERS="${1:-20}"
start=$(date +%s)

if KRANZ_SOAK_ITERS="$ITERS" cargo test --release -p kranz-engine --test soak_test -- --ignored --nocapture; then
  status=PASS
  rc=0
else
  status=FAIL
  rc=1
fi

elapsed=$(( $(date +%s) - start ))
echo "soak: ${status} (${ITERS} iterations, ${elapsed}s elapsed)"
exit "$rc"
