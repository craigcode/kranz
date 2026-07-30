#!/usr/bin/env bash
# Consolidated regression test for docs/scoping/beads-bridge-dialect.md.
#
# Combines two prose invariants that both live in this one document, so a
# single gate/script guards the whole file instead of one bespoke script per
# finding:
#
# 1. gc-bd-unverified-scope (finding a5): §2's verified-shapes table must
#    state it covers upstream `bd` only and NOT the `gc bd` pass-through every
#    bridge script actually calls. Previously silently deleted (14a49ab) and
#    restored (497e685) without a correction note.
#
# 2. bd-init-answer-executed (finding f-1-1): §5 must state, on an EXECUTED
#    basis, whether `bd init` can create a self-contained store in an
#    arbitrary empty directory. Previously (14a49ab) answered from `--help`
#    prose only ("not actually run"); fixed by 497e685 running
#    `bd init --non-interactive` in a `mktemp -d` sandbox and recording the
#    executed result.
#
# Usage: check-dialect-doc.sh [path-to-doc]
# Defaults to the real doc path resolved relative to this script's location.
set -euo pipefail

DOC="${1:-$(dirname "$0")/../../../docs/scoping/beads-bridge-dialect.md}"

fail() {
  echo "FAIL [$1]: expected $2, observed $3" >&2
  exit 1
}

[ -f "$DOC" ] || fail "doc-exists" "$DOC to exist" "not found"

# --- Check 1: gc-bd-unverified-scope -----------------------------------

grep -q "verified against \*\*upstream \`bd\` 1.0.5 invoked" "$DOC" \
  || fail "gc-bd-unverified-scope" \
    "§2 to state the table is verified against upstream bd only" \
    "no such statement found"

grep -q "gc bd\` pass-through's fidelity to the shapes below is" "$DOC" \
  || fail "gc-bd-unverified-scope" \
    "§2 to flag the gc bd pass-through as unverified" \
    "no such disclosure found"

grep -q "gc bd: not in a city directory" "$DOC" \
  || fail "gc-bd-unverified-scope" \
    "§4 to record the executed gc bd --help failure" \
    "no such record found"

echo "DIALECT-DOC-SCOPE: PASS (gc bd pass-through explicitly flagged as unverified)"

# --- Check 2: bd-init-answer-executed -----------------------------------

grep -q "EXECUTED, 2026-07-30" "$DOC" \
  || fail "bd-init-answer-executed" \
    "§5 heading to mark the bd init probe as executed" \
    "heading does not mark it executed"

grep -q "This was \*\*actually run\*\* on 2026-07-30" "$DOC" \
  || fail "bd-init-answer-executed" \
    "§5 to state the bd init probe was actually run" \
    "no such statement found"

grep -q "Confirmed by executed run, not inferred from \`--help\` text" "$DOC" \
  || fail "bd-init-answer-executed" \
    "§5 to distinguish the executed confirmation from --help inference" \
    "no such distinction found"

if grep -q "probed via \`--help\` only — not run" "$DOC"; then
  fail "bd-init-answer-executed" \
    "§5 heading to not regress to the --help-only title" \
    "'(probed via --help only — not run)' heading present"
fi

if grep -qF 'was **not actually run** in this feature (per the read-only-probe' "$DOC"; then
  fail "bd-init-answer-executed" \
    "§5 to not regress to the inference-only answer" \
    "'not actually run' inference-only text present"
fi

grep -q "(no other flags) successfully creates a self-contained, standalone" "$DOC" \
  || fail "bd-init-answer-executed" \
    "§5 to state the executed conclusion that bd init creates a self-contained standalone store" \
    "no such conclusion found"

echo "DIALECT-DOC-BD-INIT-EXECUTED: PASS (bd init self-contained-store answer is backed by an executed probe, not --help inference)"
