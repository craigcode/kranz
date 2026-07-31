#!/bin/sh
# kranz-dispatch-roundtrip-selftest.sh — regression test for finding [a2]
# marker discipline: kranz-dispatch-roundtrip.sh used to run the STATUS
# cases (kranz-dispatch-roundtrip.sh:126-131) and then unconditionally print
# "STATUS: PASS" regardless of whether any assert_status call actually
# failed — assert_status/fail_case only set FAILED=1, they never skipped the
# echo. Fixed in ms-2-fix-2-1 by adding a per-case STATUS_OK flag (cleared by
# assert_status on mismatch) and gating the echo on it, mirroring the
# RUNBEAD_OK pattern. This test proves a broken status transition can never
# again print alongside its own "STATUS: PASS" marker, independent of the
# current state of kranz-dispatch-roundtrip.sh.
#
# Requires a live `bd` on PATH, same as the script under test — SKIPs
# otherwise rather than failing.
set -u

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROUNDTRIP="$SCRIPT_DIR/kranz-dispatch-roundtrip.sh"

fail() {
    echo "FAIL [$1]: expected $2, observed $3" >&2
    exit 1
}

[ -f "$ROUNDTRIP" ] || fail "roundtrip-script-exists" "$ROUNDTRIP to exist" "not found"

if ! command -v bd >/dev/null 2>&1; then
    echo "KRANZ-DISPATCH-ROUNDTRIP-SELFTEST: SKIP (bd not on PATH)"
    exit 0
fi

SANDBOX=$(mktemp -d)
trap 'rm -rf "$SANDBOX"' EXIT INT TERM

# --- Case: a broken status transition must never print STATUS: PASS -------
#
# Patch a copy of the script so one status-transition case is guaranteed to
# fail: locate the assert_status call whose second argument is the
# open->in_progress transition label — by construct, not by pinning the
# label text, so a relabel of that assertion doesn't break this test — and
# replace ONLY that line's first argument with a value bd will never
# actually report back (bd rejects/ignores unknown status values), forcing
# assert_status's mismatch branch and STATUS_OK=0.
BROKEN="$SANDBOX/kranz-dispatch-roundtrip-broken.sh"

TARGET_LINE_NO=$(grep -nE 'assert_status[[:space:]]+"[a-z_]+"[[:space:]]+"open->in_progress' "$ROUNDTRIP" \
    | head -n1 | cut -d: -f1)
[ -n "$TARGET_LINE_NO" ] || fail "broken-setup" \
    "an assert_status call with label \"open->in_progress\" to exist in $ROUNDTRIP" \
    "no matching line found"

sed "${TARGET_LINE_NO}s/assert_status[[:space:]]*\"[a-z_]*\"/assert_status \"not_a_real_status\"/" \
    "$ROUNDTRIP" >"$BROKEN"
chmod +x "$BROKEN"

ORIG_LINE=$(sed -n "${TARGET_LINE_NO}p" "$ROUNDTRIP")
PATCHED_LINE=$(sed -n "${TARGET_LINE_NO}p" "$BROKEN")
if [ "$PATCHED_LINE" = "$ORIG_LINE" ]; then
    fail "broken-setup" "the patched copy's target line to differ from the original" \
        "line $TARGET_LINE_NO unchanged: $PATCHED_LINE"
fi
case "$PATCHED_LINE" in
    *'assert_status'*'"not_a_real_status"'*'"open->in_progress'*) : ;;
    *)
        fail "broken-setup" "the patched line to carry the forced-mismatch status and original label" \
            "$PATCHED_LINE"
        ;;
esac

OUT=$("$BROKEN" 2>&1)
STATUS=$?

case "$OUT" in
    *"FAIL [status-transition:open->in_progress"*) : ;;
    *) fail "broken-fails" "a FAIL line for status-transition:open->in_progress" "$OUT" ;;
esac

case "$OUT" in
    *"STATUS: PASS"*)
        fail "broken-no-status-pass" \
            "no STATUS: PASS marker when a status transition failed" \
            "STATUS: PASS printed alongside the FAIL line: $OUT"
        ;;
    *) : ;;
esac

case "$OUT" in
    *"ROUNDTRIP: PASS"*)
        fail "broken-no-roundtrip-pass" \
            "no ROUNDTRIP: PASS marker when a status transition failed" \
            "ROUNDTRIP: PASS printed alongside the FAIL line: $OUT"
        ;;
    *) : ;;
esac

[ "$STATUS" -ne 0 ] || fail "broken-nonzero-exit" \
    "the patched script to exit non-zero" "exit 0"

echo "SELFTEST [broken-status-transition]: PASS (STATUS: PASS correctly withheld, FAIL line present, nonzero exit)"

# --- Non-vacuity: the unmodified script must still print STATUS: PASS -----
# (proves the case above is a real regression test, not a script that never
# emits the marker at all).
OUT=$("$ROUNDTRIP" 2>&1)
STATUS=$?

case "$OUT" in
    *"STATUS: PASS"*) : ;;
    *"ROUNDTRIP: SKIP"*)
        echo "KRANZ-DISPATCH-ROUNDTRIP-SELFTEST: SKIP (sandbox bd init failed for the unmodified script)"
        exit 0
        ;;
    *) fail "non-vacuity" "the unmodified script to print STATUS: PASS" "$OUT" ;;
esac
echo "SELFTEST [non-vacuity]: PASS (unmodified script still prints STATUS: PASS)"

echo "KRANZ-DISPATCH-ROUNDTRIP-SELFTEST: PASS (finding [a2] marker discipline does not reproduce)"
