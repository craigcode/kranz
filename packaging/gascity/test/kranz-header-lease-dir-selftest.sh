#!/bin/sh
# kranz-header-lease-dir-selftest.sh — regression test for finding [f-3-1]
# (lease-dir default mismatch): both bridge translator headers must document
# a CITY-ANCHORED KRANZ_LEASE_DIR default, never the CWD-relative
# ${KRANZ_LEASE_DIR:-.gc/kranz-leases} that once appeared in kranz-dispatch's
# header. The anchor spelling is per-script, because the two scripts derive
# their city path differently:
#   kranz-dispatch:  ${KRANZ_LEASE_DIR:-$GC_CITY/.gc/kranz-leases} — dispatch
#                     itself does `CITY=${GC_CITY:?...}` (see its own Env
#                     block), so $GC_CITY is the correct, truthful anchor.
#   kranz-run-bead:   ${KRANZ_LEASE_DIR:-$CITY/.gc/kranz-leases} — this script
#                     takes CITY as a positional argument ($1) passed by
#                     kranz-city-worker; it never reads $GC_CITY itself, so
#                     documenting $GC_CITY here would assert a mechanism the
#                     script does not have. $CITY is the truthful anchor.
set -u

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
BIN_DIR=$(CDPATH= cd -- "$SCRIPT_DIR/../bin" && pwd)

fail() {
    echo "FAIL [$1]: expected $2, observed $3" >&2
    exit 1
}

extract_header() {
    awk '/^#!/ { next } /^#/ { sub(/^#[[:space:]]?/, ""); print; next } /^[[:space:]]*$/ { next } { exit }' "$1" \
        | tr '\n' ' ' | tr -s ' '
}

# Both scripts' shipped code resolves LEASE_DIR from a *local* $CITY var
# (kranz-dispatch's is required-derived from $GC_CITY at its own Env block;
# kranz-run-bead's is the positional $1) — so the code-level construct check
# is the same regex for both; only the HEADER anchor var documented to an
# operator differs per script's derivation.
CODE_REGEX='^[[:space:]]*LEASE_DIR=\$\{KRANZ_LEASE_DIR:-\$CITY/\.gc/kranz-leases\}'

# NAME:expected-header-anchor-var
for PAIR in \
    "kranz-dispatch:GC_CITY" \
    "kranz-run-bead:CITY"
do
    NAME=$(printf '%s' "$PAIR" | cut -d: -f1)
    ANCHOR_VAR=$(printf '%s' "$PAIR" | cut -d: -f2)
    FILE="$BIN_DIR/$NAME"
    [ -f "$FILE" ] || fail "$NAME-exists" "$FILE to exist" "not found"

    HEADER=$(extract_header "$FILE")

    # Every KRANZ_LEASE_DIR default mentioned in the header must be
    # anchored under this script's specific city variable, never a
    # bare/CWD-relative path or the other script's variable.
    LEASE_MENTIONS=$(printf '%s' "$HEADER" | grep -ioE 'KRANZ_LEASE_DIR:-[^} ]*' || true)
    [ -n "$LEASE_MENTIONS" ] || fail "$NAME-lease-dir-documented" \
        "header to document a KRANZ_LEASE_DIR default" \
        "no KRANZ_LEASE_DIR default found in header"

    printf '%s\n' "$LEASE_MENTIONS" | while IFS= read -r MENTION; do
        [ -n "$MENTION" ] || continue
        case "$MENTION" in
            *'$'"$ANCHOR_VAR"*) : ;;
            *)
                echo "FAIL [$NAME-lease-dir-anchored]: expected every header KRANZ_LEASE_DIR default to be anchored under \$$ANCHOR_VAR, observed: $MENTION" >&2
                exit 1
                ;;
        esac
    done || exit 1

    # Explicitly reject the CWD-relative default this finding reported.
    if printf '%s' "$HEADER" | grep -qE 'KRANZ_LEASE_DIR:-\.gc/kranz-leases'; then
        fail "$NAME-no-cwd-relative-default" \
            "header to NOT document a bare CWD-relative KRANZ_LEASE_DIR default" \
            "found KRANZ_LEASE_DIR:-.gc/kranz-leases in header"
    fi

    echo "SELFTEST [$NAME]: PASS (header documents \$$ANCHOR_VAR-anchored KRANZ_LEASE_DIR default)"

    # --- Cross-check: header default must match the shipped code default,
    # located structurally (not by pinned line number) so an unrelated
    # line insertion elsewhere in the file cannot silently pass or fail
    # this check.
    MATCH_COUNT=$(grep -cE "$CODE_REGEX" "$FILE")
    case "$MATCH_COUNT" in
        0)
            fail "$NAME-shipped-default-unchanged" \
                "exactly one line in $NAME matching LEASE_DIR=\${KRANZ_LEASE_DIR:-\$CITY/.gc/kranz-leases}" \
                "no matching line found"
            ;;
        1) : ;;
        *)
            fail "$NAME-shipped-default-unambiguous" \
                "exactly one line in $NAME matching LEASE_DIR=\${KRANZ_LEASE_DIR:-\$CITY/.gc/kranz-leases}" \
                "$MATCH_COUNT matching lines found"
            ;;
    esac
done

echo "KRANZ-HEADER-LEASE-DIR-SELFTEST: PASS ([f-3-1] lease-dir default mismatch no longer reproduces)"
