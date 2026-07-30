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

# Bidirectional marker accepted between a bead status and its kranz-side
# counterpart: "<->" or "<-->", in either direction, on the same comment
# line. Case-insensitive on the kranz-side word so translator authors have
# some prose latitude.
status_map_file_missing() {
    # $1 = file to check. Echoes a space-separated list of what's missing
    # from that file's comment header (empty if nothing is missing).
    f="$1"
    MISSING=""

    check_pair() {
        # $1 = bead status, $2 = kranz-side word (regex, case-insensitive)
        bead_status="$1"
        kranz_word="$2"
        # A bidirectional comment line naming both, in either order, joined
        # by <-> or <-->, e.g. "# open <-> Queued" or "# Queued <--> open".
        if grep -qiE "^[[:space:]]*#.*\\b${bead_status}\\b.*<-{1,2}>.*\\b${kranz_word}\\b" "$f" 2>/dev/null ||
           grep -qiE "^[[:space:]]*#.*\\b${kranz_word}\\b.*<-{1,2}>.*\\b${bead_status}\\b" "$f" 2>/dev/null; then
            return 0
        fi
        return 1
    }

    check_pair "open" "queued"          || MISSING="$MISSING open<->Queued"
    check_pair "in_progress" "running"  || MISSING="$MISSING in_progress<->Running"
    check_pair "blocked" "blocked-report" || MISSING="$MISSING blocked<->Blocked-report"
    check_pair "closed" "done"          || MISSING="$MISSING closed<->Done"

    # Many-to-one collapse: both exit 3 and exit 1/other return the bead to
    # "open". Require a comment documenting that collapse explicitly.
    if ! grep -qiE "^[[:space:]]*#.*\\bexit[[:space:]]*3\\b.*\\bopen\\b" "$f" 2>/dev/null &&
       ! grep -qiE "^[[:space:]]*#.*\\bopen\\b.*\\bexit[[:space:]]*3\\b" "$f" 2>/dev/null; then
        MISSING="$MISSING exit3->open-collapse"
    fi
    if ! grep -qiE "^[[:space:]]*#.*\\bexit[[:space:]]*1\\b.*\\bopen\\b" "$f" 2>/dev/null &&
       ! grep -qiE "^[[:space:]]*#.*\\bopen\\b.*\\bexit[[:space:]]*1\\b" "$f" 2>/dev/null &&
       ! grep -qiE "^[[:space:]]*#.*\\bother\\b.*\\bopen\\b" "$f" 2>/dev/null &&
       ! grep -qiE "^[[:space:]]*#.*\\bopen\\b.*\\bother\\b" "$f" 2>/dev/null; then
        MISSING="$MISSING exit1-or-other->open-collapse"
    fi

    echo "$MISSING"
}

status_map_mode() {
    ANY_MISSING=0

    for NAME in kranz-dispatch kranz-run-bead; do
        f="$BIN_DIR/$NAME"
        if [ ! -f "$f" ]; then
            echo "check-bridge-hygiene --status-map: $f does not exist" >&2
            ANY_MISSING=1
            continue
        fi
        MISSING=$(status_map_file_missing "$f")
        if [ -n "$MISSING" ]; then
            echo "check-bridge-hygiene --status-map: $NAME is missing:$MISSING" >&2
            ANY_MISSING=1
        fi
    done

    if [ "$ANY_MISSING" -ne 0 ]; then
        echo "Each of kranz-dispatch and kranz-run-bead must carry, in its comment" >&2
        echo "header, a bidirectional (<-> or <-->) mapping line per bead status naming" >&2
        echo "its kranz-side meaning (open<->Queued, in_progress<->Running," >&2
        echo "blocked<->Blocked-report, closed<->Done), plus a comment documenting that" >&2
        echo "both exit 3 and exit 1/other collapse back to open." >&2
        return 1
    fi

    echo "check-bridge-hygiene --status-map: OK (kranz-dispatch and kranz-run-bead both document the bidirectional status mapping)"
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

    # Exclude the README (pure prose) and comment lines (documentation of
    # the prohibition itself, e.g. in this very script or in the round-trip
    # harness) — but NOT test/ wholesale. A real invocation on a code line
    # anywhere under packaging/gascity/, including test/, must still be
    # caught: that is exactly where the risk lives (a1 forbids it repo-wide).
    # A "comment line" is one whose content, ignoring leading whitespace,
    # starts with '#'; grep -n prefixes each hit with "path:lineno:", so the
    # comment check strips that prefix before testing the line itself.
    is_comment_or_readme_hit() {
        # $1 = one "path:lineno:content" line from grep -n
        line="$1"
        case "$line" in
            "$GASCITY_DIR/README.md":*) return 0 ;;
        esac
        CONTENT=$(printf '%s\n' "$line" | sed -e 's/^[^:]*:[0-9][0-9]*://')
        STRIPPED=$(printf '%s\n' "$CONTENT" | sed -e 's/^[[:space:]]*//')
        case "$line" in
            # This script's own detection code necessarily embeds the
            # literal search patterns/messages as grep invocations and echo
            # diagnostics — that is data, not an invocation of either
            # command. Only THOSE specific lines are exempt; any other line
            # in this file (e.g. a bare `gc init`/`gc stop` slipped in
            # elsewhere) is still caught below like any other file.
            "$SCRIPT_DIR/check-bridge-hygiene.sh":*)
                case "$STRIPPED" in
                    *grep*|echo\ *) return 0 ;;
                esac
                ;;
        esac
        case "$STRIPPED" in
            '#'*) return 0 ;;
        esac
        return 1
    }

    filter_code_hits() {
        # $1 = candidate matches (one per line), reads stdin implicitly via $1
        FILTERED=""
        OLD_IFS=$IFS
        IFS='
'
        for line in $1; do
            [ -n "$line" ] || continue
            if ! is_comment_or_readme_hit "$line"; then
                FILTERED="$FILTERED
$line"
            fi
        done
        IFS=$OLD_IFS
        printf '%s' "$FILTERED" | sed -e '/^$/d'
    }

    # Match the VERB (init/stop) as a whole word following a "gc" invocation,
    # with any run of whitespace and any number of intervening flag/value
    # tokens allowed (e.g. `gc init`, `gc  init`, `gc --city "$CITY" init`,
    # `gc -C dir stop`) — not just the adjacent literal "gc init"/"gc stop".
    # Word-boundary anchors on both "gc" and the verb keep this from firing
    # on substrings like "mygc init", "gc reinit", or "gc stopwatch".
    GC_TOKEN='(-[A-Za-z0-9_-]*|"[^"]*"|[A-Za-z0-9_./$${}"-]+)'
    GC_VERB_RE="(^|[^A-Za-z0-9_.-])gc([[:space:]]+${GC_TOKEN})*[[:space:]]+VERB([^A-Za-z0-9_-]|\$)"

    RAW=$(grep -rnE "$(printf '%s' "$GC_VERB_RE" | sed -e 's/VERB/init/')" "$GASCITY_DIR" 2>/dev/null || true)
    HITS=$(filter_code_hits "$RAW")
    if [ -n "$HITS" ]; then
        echo "check-bridge-hygiene: 'gc init' invocation found under packaging/gascity/ (forbidden - see docs/gascity.md:48):" >&2
        echo "$HITS" >&2
        VIOLATIONS=1
    fi

    RAW=$(grep -rnE "$(printf '%s' "$GC_VERB_RE" | sed -e 's/VERB/stop/')" "$GASCITY_DIR" 2>/dev/null || true)
    HITS=$(filter_code_hits "$RAW")
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
