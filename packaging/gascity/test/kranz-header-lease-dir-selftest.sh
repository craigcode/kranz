#!/bin/sh
# kranz-header-lease-dir-selftest.sh — regression test for finding [f-3-1]
# (lease-dir default mismatch): both bridge translator headers must document
# the lease directory default as ${KRANZ_LEASE_DIR:-$GC_CITY/.gc/kranz-leases}
# — matching the city-anchored default actually shipped in code
# (kranz-dispatch:137, kranz-run-bead:77, both `${KRANZ_LEASE_DIR:-$CITY/...}`
# where the local $CITY var is itself derived from $GC_CITY) — not the
# CWD-relative `${KRANZ_LEASE_DIR:-.gc/kranz-leases}` that once appeared in
# kranz-dispatch's own header. An operator following stale header prose
# would look in, or point KRANZ_LEASE_DIR at, the wrong directory.
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

for NAME in kranz-dispatch kranz-run-bead; do
    FILE="$BIN_DIR/$NAME"
    [ -f "$FILE" ] || fail "$NAME-exists" "$FILE to exist" "not found"

    HEADER=$(extract_header "$FILE")

    # Every KRANZ_LEASE_DIR default mentioned in the header must be
    # anchored under $GC_CITY, never a bare/CWD-relative path.
    LEASE_MENTIONS=$(printf '%s' "$HEADER" | grep -ioE 'KRANZ_LEASE_DIR:-[^} ]*' || true)
    [ -n "$LEASE_MENTIONS" ] || fail "$NAME-lease-dir-documented" \
        "header to document a KRANZ_LEASE_DIR default" \
        "no KRANZ_LEASE_DIR default found in header"

    printf '%s\n' "$LEASE_MENTIONS" | while IFS= read -r MENTION; do
        [ -n "$MENTION" ] || continue
        case "$MENTION" in
            *'$GC_CITY'*) : ;;
            *)
                echo "FAIL [$NAME-lease-dir-gc-city-anchored]: expected every header KRANZ_LEASE_DIR default to be anchored under \$GC_CITY, observed: $MENTION" >&2
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

    # Reject a header default anchored to a bare $CITY (not $GC_CITY) too —
    # that's the same bug under a different spelling.
    if printf '%s' "$HEADER" | grep -oE 'KRANZ_LEASE_DIR:-\$[A-Za-z_]*' | grep -qE '^\$?KRANZ_LEASE_DIR:-\$CITY$'; then
        fail "$NAME-no-bare-city-default" \
            "header to NOT anchor the KRANZ_LEASE_DIR default on a bare \$CITY var" \
            "found KRANZ_LEASE_DIR:-\$CITY in header"
    fi

    echo "SELFTEST [$NAME]: PASS (header documents \$GC_CITY-anchored KRANZ_LEASE_DIR default)"
done

# --- Cross-check: header default must match the shipped code default. -----
for PAIR in "kranz-dispatch:137" "kranz-run-bead:77"; do
    NAME=${PAIR%%:*}
    LINE=${PAIR##*:}
    FILE="$BIN_DIR/$NAME"
    CODE_LINE=$(sed -n "${LINE}p" "$FILE")
    case "$CODE_LINE" in
        *'LEASE_DIR=${KRANZ_LEASE_DIR:-$CITY/.gc/kranz-leases}'*) : ;;
        *)
            fail "$NAME-shipped-default-unchanged" \
                "kranz-dispatch:137 / kranz-run-bead:77 to still read LEASE_DIR=\${KRANZ_LEASE_DIR:-\$CITY/.gc/kranz-leases} (local \$CITY derives from \$GC_CITY)" \
                "$CODE_LINE"
            ;;
    esac
done

echo "KRANZ-HEADER-LEASE-DIR-SELFTEST: PASS ([f-3-1] lease-dir default mismatch no longer reproduces)"
