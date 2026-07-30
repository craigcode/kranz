#!/bin/sh
# check-bridge-hygiene.sh — static hygiene checks for the Gas City <-> kranz
# bridge scripts. Exits 0 when clean, nonzero (with violations printed) when
# not. The negation this script exists to encode lives entirely in its own
# exit code — kranz's contract preflight rejects a leading `!` on an
# assertion command, so callers must never write `! grep ...`; they invoke
# this script instead and read its exit status.
#
# Usage:
#   check-bridge-hygiene.sh              default mode (see below)
#   check-bridge-hygiene.sh --status-map  status-map documentation mode
set -u

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
GASCITY_DIR=$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)
BIN_DIR="$GASCITY_DIR/bin"

MODE="${1:-default}"

status_map_mode() {
    MISSING=""
    for STATUS in open in_progress blocked closed; do
        FOUND=0
        for f in "$BIN_DIR"/*; do
            [ -f "$f" ] || continue
            if grep -q "$STATUS" "$f" 2>/dev/null; then
                # Only count occurrences inside the leading comment header
                # (before the first blank-line-terminated code section is
                # too strict across scripts; instead require the mention to
                # appear within a comment line, i.e. a line starting with
                # optional whitespace then '#').
                if grep -qE "^[[:space:]]*#.*$STATUS" "$f" 2>/dev/null; then
                    FOUND=1
                fi
            fi
        done
        if [ "$FOUND" -ne 1 ]; then
            MISSING="$MISSING $STATUS"
        fi
    done

    if [ -n "$MISSING" ]; then
        echo "check-bridge-hygiene --status-map: missing header documentation for bead status(es):$MISSING" >&2
        echo "Each of open/in_progress/blocked/closed must be documented, with its kranz-side meaning, in a comment header of a script under packaging/gascity/bin/." >&2
        return 1
    fi

    echo "check-bridge-hygiene --status-map: OK (open/in_progress/blocked/closed all documented)"
    return 0
}

default_mode() {
    VIOLATIONS=0

    if [ -d "$BIN_DIR" ]; then
        HITS=$(grep -rn "set-state" "$BIN_DIR" 2>/dev/null || true)
        if [ -n "$HITS" ]; then
            echo "check-bridge-hygiene: 'set-state' found under packaging/gascity/bin/ (the bridge must only use 'bd update --status'):" >&2
            echo "$HITS" >&2
            VIOLATIONS=1
        fi
    fi

    # Exclude test/ and the README: they document (and this very check
    # enforces) the gc init/gc stop prohibition, so their prose mentions the
    # literal strings without invoking either command.
    HITS=$(grep -rn "gc init" "$GASCITY_DIR" 2>/dev/null | grep -v -e "^$SCRIPT_DIR/" -e "^$GASCITY_DIR/README.md:" || true)
    if [ -n "$HITS" ]; then
        echo "check-bridge-hygiene: 'gc init' invocation found under packaging/gascity/ (forbidden - see docs/gascity.md:48):" >&2
        echo "$HITS" >&2
        VIOLATIONS=1
    fi

    HITS=$(grep -rn "gc stop" "$GASCITY_DIR" 2>/dev/null | grep -v -e "^$SCRIPT_DIR/" -e "^$GASCITY_DIR/README.md:" || true)
    if [ -n "$HITS" ]; then
        echo "check-bridge-hygiene: 'gc stop' invocation found under packaging/gascity/ (forbidden - see docs/gascity.md:48):" >&2
        echo "$HITS" >&2
        VIOLATIONS=1
    fi

    if [ "$VIOLATIONS" -ne 0 ]; then
        return 1
    fi

    echo "check-bridge-hygiene: OK (no set-state under bin/, no gc init/gc stop under gascity/)"
    return 0
}

case "$MODE" in
    --status-map)
        status_map_mode
        exit $?
        ;;
    default)
        default_mode
        exit $?
        ;;
    *)
        echo "check-bridge-hygiene: unknown mode '$MODE' (expected no args or --status-map)" >&2
        exit 2
        ;;
esac
