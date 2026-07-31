#!/bin/sh
# kranz-milestone-scope-selftest.sh — regression test for finding
# 'milestone-scope': validation observed that `git rev-parse HEAD` and
# `git rev-parse 2c3e26b0d715a5c9e452b43e725a90d6af22fe41` resolved to the
# same commit, i.e. the milestone-3 lease-aware-claim work described in the
# mission goal had not actually been authored/committed on this branch yet
# (`git diff --stat` over that range for packaging/gascity/bin/ and
# packaging/gascity/test/ produced no output).
#
# This guards against silently regressing back to that state: it fails if
# HEAD ever again collapses onto (or predates) that base commit for the
# bridge's bin/ and test/ trees, and it fails if the concrete milestone
# deliverables — an atomic claim call, liveness-first recovery
# documentation, and a round-trip fixture test that runs against a live bd
# — are missing from the tree. It is deliberately independent of exact
# prose (see kranz-header-liveness-selftest.sh's reword-tolerance rationale)
# and greps for substance, not frozen phrases.
set -u

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
BIN_DIR=$(CDPATH= cd -- "$SCRIPT_DIR/../bin" && pwd)
GASCITY_DIR=$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)
REPO_ROOT=$(CDPATH= cd -- "$GASCITY_DIR/../.." && pwd)
BASE_SHA=2c3e26b0d715a5c9e452b43e725a90d6af22fe41

fail() {
    echo "FAIL [$1]: expected $2, observed $3" >&2
    exit 1
}

# --- Case: the bridge trees have actually diverged from the pre-milestone
# base commit. An empty diff here is exactly the evidence that produced the
# 'milestone-scope' finding.
if git -C "$REPO_ROOT" rev-parse --verify --quiet "$BASE_SHA^{commit}" >/dev/null; then
    DIFF_STAT=$(git -C "$REPO_ROOT" diff --stat "$BASE_SHA"..HEAD -- packaging/gascity/bin/ packaging/gascity/test/)
    if [ -z "$DIFF_STAT" ]; then
        fail "diverged-from-base" "a non-empty diff for packaging/gascity/bin/ and packaging/gascity/test/ since $BASE_SHA" "no diff (milestone-scope finding reproduces)"
    fi
    echo "SELFTEST [diverged-from-base]: PASS (bin/ and test/ have landed changes since $BASE_SHA)"
else
    echo "SELFTEST [diverged-from-base]: SKIP (base commit $BASE_SHA not present in this checkout's history)"
fi

# --- Case: the atomic, first-wins claim actually ships in kranz-dispatch.
if ! grep -Eq 'bd[[:space:]]+update[[:space:]]+"?\$ID"?[[:space:]]+--claim' "$BIN_DIR/kranz-dispatch"; then
    fail "atomic-claim-present" "kranz-dispatch to issue 'bd update \"\$ID\" --claim'" "no such call found"
fi
echo "SELFTEST [atomic-claim-present]: PASS"

# --- Case: liveness-first recovery posture is documented in both scripts
# that own claim lifecycle (dispatch claims, run-bead owns the exit codes
# that would otherwise release/hold that claim).
for f in kranz-dispatch kranz-run-bead; do
    if ! grep -Eiq 'liveness[- ]first' "$BIN_DIR/$f"; then
        fail "liveness-first-documented" "$f to document a liveness-first recovery posture" "no 'liveness-first' mention found"
    fi
done
echo "SELFTEST [liveness-first-documented]: PASS"

# --- Case: a round-trip fixture test exists and asserts it ran against a
# live bd (not only a stub), per the mission goal.
ROUNDTRIP="$SCRIPT_DIR/kranz-dispatch-roundtrip.sh"
if [ ! -f "$ROUNDTRIP" ]; then
    fail "roundtrip-test-exists" "kranz-dispatch-roundtrip.sh to exist" "file not found"
fi
if ! grep -Eiq 'live bd' "$ROUNDTRIP"; then
    fail "roundtrip-asserts-live-bd" "the round-trip test to assert it ran against a live bd" "no 'live bd' assertion found"
fi
echo "SELFTEST [roundtrip-test-exists]: PASS"

echo "KRANZ-MILESTONE-SCOPE-SELFTEST: PASS (finding 'milestone-scope' no longer reproduces)"
