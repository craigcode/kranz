#!/bin/sh
# kranz-dispatch-roundtrip-claim-selftest.sh — regression test for finding
# f-3-1: "A competing claim on a live claim fails" was proven only by a bare
# non-zero exit code (kranz-dispatch-roundtrip.sh:152 used to read
# `[ "$COMPETE_EXIT" -eq 0 ] && fail_case`, with nothing checking COMPETE_OUT).
# That meant an unrelated failure — a moved --actor flag, a cd failure, a
# store-permission error, a typo in the subcommand — would also yield a
# non-zero exit and print "CLAIM: PASS ... competing claim by a different
# actor fails" having proven nothing about claim contention.
#
# Fixed (ms-3-fix-1-7) by asserting the error TEXT matches 'already claimed'
# (kranz-dispatch-roundtrip.sh:170-171), in addition to the non-zero exit.
# This test proves that assertion actually gates on message content, not just
# exit code: it patches a copy of the script so the competing-claim command is
# replaced with one that fails for an UNRELATED reason (nonzero exit, error
# text carrying no "already claimed" message) and asserts the patched script
# correctly FAILs claim-competing-error-text rather than printing CLAIM: PASS.
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
    echo "KRANZ-DISPATCH-ROUNDTRIP-CLAIM-SELFTEST: SKIP (bd not on PATH)"
    exit 0
fi

SANDBOX=$(mktemp -d)
trap 'rm -rf "$SANDBOX"' EXIT INT TERM

# --- Case: a competing-claim failure for an UNRELATED reason must not print
# CLAIM: PASS -----------------------------------------------------------
#
# Locate the COMPETE_OUT assignment line by construct (its content), not by
# a pinned line number, so an unrelated edit to the file doesn't silently
# break this test's setup.
BROKEN="$SANDBOX/kranz-dispatch-roundtrip-broken.sh"

TARGET_LINE_NO=$(grep -nE '^\s*COMPETE_OUT=\$\(' "$ROUNDTRIP" | head -n1 | cut -d: -f1)
[ -n "$TARGET_LINE_NO" ] || fail "broken-setup" \
    "a COMPETE_OUT=\$(...) assignment to exist in $ROUNDTRIP" \
    "no matching line found"

# Replace only that line with a stand-in that fails for a reason having
# nothing to do with claim contention: nonzero exit, unrelated error text.
awk -v n="$TARGET_LINE_NO" \
    'NR==n {print "        COMPETE_OUT=$(printf '\''%s\\n'\'' '\''bd: error: unrecognized flag --actor'\''; false)"; next} {print}' \
    "$ROUNDTRIP" >"$BROKEN"
chmod +x "$BROKEN"

ORIG_LINE=$(sed -n "${TARGET_LINE_NO}p" "$ROUNDTRIP")
PATCHED_LINE=$(sed -n "${TARGET_LINE_NO}p" "$BROKEN")
if [ "$PATCHED_LINE" = "$ORIG_LINE" ]; then
    fail "broken-setup" "the patched copy's target line to differ from the original" \
        "line $TARGET_LINE_NO unchanged: $PATCHED_LINE"
fi
case "$PATCHED_LINE" in
    *'COMPETE_OUT='*'unrecognized flag --actor'*) : ;;
    *)
        fail "broken-setup" "the patched line to carry the unrelated-failure stand-in" \
            "$PATCHED_LINE"
        ;;
esac

OUT=$("$BROKEN" 2>&1)
STATUS=$?

case "$OUT" in
    *"FAIL [claim-competing-error-text]"*) : ;;
    *) fail "broken-fails" "a FAIL line for claim-competing-error-text" "$OUT" ;;
esac

case "$OUT" in
    *"CLAIM: PASS"*)
        fail "broken-no-claim-pass" \
            "no CLAIM: PASS marker when the competing-claim failure carried no 'already claimed' text" \
            "CLAIM: PASS printed alongside the FAIL line: $OUT"
        ;;
    *) : ;;
esac

case "$OUT" in
    *"ROUNDTRIP: PASS"*)
        fail "broken-no-roundtrip-pass" \
            "no ROUNDTRIP: PASS marker when the CLAIM case failed" \
            "ROUNDTRIP: PASS printed alongside the FAIL line: $OUT"
        ;;
    *) : ;;
esac

[ "$STATUS" -ne 0 ] || fail "broken-nonzero-exit" \
    "the patched script to exit non-zero" "exit 0"

echo "SELFTEST [broken-competing-claim-unrelated-failure]: PASS (CLAIM: PASS correctly withheld, FAIL line present, nonzero exit)"

# --- Non-vacuity: the unmodified script must still print CLAIM: PASS ------
# (proves the case above is a real regression test, not a script that never
# emits the marker at all).
OUT=$("$ROUNDTRIP" 2>&1)
STATUS=$?

case "$OUT" in
    *"CLAIM: PASS"*) : ;;
    *"ROUNDTRIP: SKIP"*)
        echo "KRANZ-DISPATCH-ROUNDTRIP-CLAIM-SELFTEST: SKIP (sandbox bd init failed for the unmodified script)"
        exit 0
        ;;
    *) fail "non-vacuity" "the unmodified script to print CLAIM: PASS" "$OUT" ;;
esac
echo "SELFTEST [non-vacuity]: PASS (unmodified script still prints CLAIM: PASS)"

echo "KRANZ-DISPATCH-ROUNDTRIP-CLAIM-SELFTEST: PASS (finding [f-3-1] no longer reproduces)"
