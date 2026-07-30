#!/usr/bin/env bash
# Regression test for finding f-1-1: docs/scoping/beads-bridge-dialect.md must
# state, on an EXECUTED basis, whether `bd init` can create a self-contained
# store in an arbitrary empty directory. The doc previously (commit 14a49ab)
# answered this from `--help` prose only, saying so plainly: "this was **not
# actually run** in this feature (per the read-only-probe constraint)". That
# matters because Milestone 2.1 depends on the answer being yes — it
# initialises a store inside a `mktemp -d` as a load-bearing first step.
# Commit 497e685 fixed this by running `bd init --non-interactive` in a
# `mktemp -d` sandbox and recording the executed result. This check guards
# against a regression back to the inference-only answer.
set -euo pipefail

DOC="$(dirname "$0")/../../../docs/scoping/beads-bridge-dialect.md"

fail() {
  echo "FAIL: $1" >&2
  exit 1
}

[ -f "$DOC" ] || fail "$DOC not found"

grep -q "EXECUTED, 2026-07-30" "$DOC" \
  || fail "§5 heading no longer marks the bd init probe as executed"

grep -q "This was \*\*actually run\*\* on 2026-07-30" "$DOC" \
  || fail "§5 no longer states the bd init probe was actually run"

grep -q "Confirmed by executed run, not inferred from \`--help\` text" "$DOC" \
  || fail "§5 no longer distinguishes the executed confirmation from --help inference"

if grep -q "probed via \`--help\` only — not run" "$DOC"; then
  fail "§5 heading has regressed back to the '(probed via --help only — not run)' title"
fi

if grep -qF 'was **not actually run** in this feature (per the read-only-probe' "$DOC"; then
  fail "§5 has regressed back to the inference-only ('not actually run') answer"
fi

grep -q "(no other flags) successfully creates a self-contained, standalone" "$DOC" \
  || fail "§5 no longer states the executed conclusion that bd init creates a self-contained standalone store"

echo "DIALECT-DOC-BD-INIT-EXECUTED: PASS (bd init self-contained-store answer is backed by an executed probe, not --help inference)"
