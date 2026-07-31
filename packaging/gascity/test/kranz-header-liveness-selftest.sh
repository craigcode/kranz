#!/bin/sh
# kranz-header-liveness-selftest.sh — regression test for finding [f-3-1]:
# both bridge translator headers (kranz-dispatch and kranz-run-bead) must
# document the liveness-first posture for future lease-aware claim recovery
# and the residual spooled-but-not-drained TTL-only exposure window, in
# addition to their existing status-map and exit-code-contract blocks. This
# is pure documentation content — it does not depend on bd exposing a lease
# capability — so this test just greps the shipped header comments for the
# required phrases.
set -u

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
BIN_DIR=$(CDPATH= cd -- "$SCRIPT_DIR/../bin" && pwd)

fail() {
    echo "FAIL [$1]: expected $2, observed $3" >&2
    exit 1
}

for NAME in kranz-dispatch kranz-run-bead; do
    FILE="$BIN_DIR/$NAME"
    [ -f "$FILE" ] || fail "$NAME-exists" "$FILE to exist" "not found"

    # Only inspect the leading comment header, before the first non-comment,
    # non-blank line (i.e. before the script body starts). Collapse to a
    # single space-joined line so phrases that wrap across `# ...` comment
    # lines still match as a contiguous substring.
    HEADER=$(awk '/^#!/ { next } /^#/ { sub(/^#[[:space:]]?/, ""); print; next } /^[[:space:]]*$/ { next } { exit }' "$FILE" | tr '\n' ' ' | tr -s ' ')

    case "$HEADER" in
        *"liveness first"*) : ;;
        *) fail "$NAME-liveness-first" \
            "header to document a liveness-first posture (a live claim holder is never stolen)" \
            "no 'liveness first' phrase in header" ;;
    esac

    case "$HEADER" in
        *"NOT YET IMPLEMENTED"*) : ;;
        *) fail "$NAME-liveness-not-yet-implemented" \
            "header to flag the liveness-first posture as not yet implemented (pending a bd lease mechanism)" \
            "no 'NOT YET IMPLEMENTED' phrase in header" ;;
    esac

    case "$HEADER" in
        *"Residual exposure"*|*"Residual exposure today"*) : ;;
        *) fail "$NAME-residual-exposure" \
            "header to name the residual spooled-but-not-drained exposure explicitly" \
            "no 'Residual exposure' phrase in header" ;;
    esac

    case "$HEADER" in
        *"no heartbeat"*"no TTL"*|*"no TTL"*"no heartbeat"*)
            : ;;
        *)
            fail "$NAME-residual-ttl-only" \
                "header to state the spooled-but-not-drained window has no heartbeat and no TTL backstop" \
                "missing 'no heartbeat' / 'no TTL' wording in header" ;;
    esac

    echo "SELFTEST [$NAME]: PASS (header documents liveness-first posture and residual TTL-only exposure)"
done

echo "KRANZ-HEADER-LIVENESS-SELFTEST: PASS ([f-3-1] no longer reproduces)"
