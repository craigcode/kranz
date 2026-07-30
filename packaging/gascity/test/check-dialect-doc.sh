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
# 3. set-state-resolution-note (finding a5, ms-2-fix-2-6): §4's kranz-run-bead
#    set-state bullet asserted in the present tense that the live script falls
#    through to `bd update --status blocked` via `||`. ms-2 removed that
#    fallback chain entirely (bin/kranz-run-bead:46 is now a bare `bd update`
#    call), leaving the doc describing code that no longer exists. Guards that
#    a dated resolution note is attached instead of the doc being silently
#    left stale.
#
# Usage: check-dialect-doc.sh [path-to-doc]
# Defaults to the real doc path resolved relative to this script's location.
set -euo pipefail

DOC="${1:-$(dirname "$0")/../../../docs/scoping/beads-bridge-dialect.md}"
RUN_BEAD="$(dirname "$0")/../bin/kranz-run-bead"

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

# --- Check 3: set-state-resolution-note ----------------------------------

grep -q "PROBE FINDING, 2026-07-29 — HISTORICAL" "$DOC" \
  || fail "set-state-resolution-note" \
    "§4's kranz-run-bead set-state bullet to be marked HISTORICAL" \
    "no such marker found"

grep -q "Resolution note (2026-07-30, ms-2-fix-2-3)" "$DOC" \
  || fail "set-state-resolution-note" \
    "§4 to carry a dated resolution note for the set-state bullet" \
    "no resolution note found"

grep -q "\`set-state\` call and its \`||\` fallback chain were removed entirely" "$DOC" \
  || fail "set-state-resolution-note" \
    "the resolution note to state the || fallback chain was removed, not just reordered" \
    "no such statement found"

grep -q "fails the build if it reappears" "$DOC" \
  || fail "set-state-resolution-note" \
    "the resolution note to record that check-bridge-hygiene.sh guards the regression" \
    "no such guard reference found"

grep -q 'bd update "\$ID" --status blocked' "$RUN_BEAD" \
  || fail "set-state-resolution-note" \
    "the live script to use a bare bd update --status blocked call" \
    "no bare bd update --status blocked call found in $RUN_BEAD"

if grep -q "set-state" "$RUN_BEAD"; then
  fail "set-state-resolution-note" \
    "the live script to contain no set-state call" \
    "'set-state' found in $RUN_BEAD"
fi

echo "DIALECT-DOC-SET-STATE-RESOLUTION: PASS (set-state bullet is marked historical with a resolution note, and the live script matches)"
