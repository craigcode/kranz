#!/bin/sh
# kranz-run-bead-events.sh — regression test for the Stage 3 City event emits.
#
# Stubs gc/bd/kranz on PATH and runs packaging/gascity/bin/kranz-run-bead with
# each exit code (0/2/3/1), asserting that the expected kranz.mission.* events
# are emitted with the right subject and type. No live city, no network.
set -u

BIN_DIR=$(CDPATH= cd -- "$(dirname -- "$0")/../bin" && pwd)
RUN_BEAD="$BIN_DIR/kranz-run-bead"

SANDBOX=$(mktemp -d "${TMPDIR:-/tmp}/kranz-run-bead-events.XXXXXX")
trap 'rm -rf "$SANDBOX"' EXIT INT TERM

EVENT_LOG="$SANDBOX/events.log"
mkdir -p "$SANDBOX/bin" "$SANDBOX/rig/.git"

# --- stubs on PATH ----------------------------------------------------------
cat > "$SANDBOX/bin/gc" <<'STUB'
#!/bin/sh
# gc stub: handles the three gc subcommands run-bead uses:
#   gc --city <city> bd <subcommand> ...
#   gc --city <city> event emit <type> [flags]
#   gc --city <city> mail send human ...
CITY=$2
shift 2
CMD=$1
shift
case "$CMD" in
    bd)
        SUB=$1
        shift
        case "$SUB" in
            show)
                # First call (before run) sees in_progress; later calls confirm terminal.
                if [ -f "$EVENT_LOG.status" ]; then
                    cat "$EVENT_LOG.status"
                else
                    echo '{"id":"bead-1","status":"in_progress"}'
                fi
                ;;
            update|close|comment)
                # Capture status changes so the post-run confirmation sees the terminal state.
                case "$*" in
                    *--status\ blocked*)
                        echo '{"id":"bead-1","status":"blocked"}' > "$EVENT_LOG.status" ;;
                    *--status\ open*)
                        echo '{"id":"bead-1","status":"open"}' > "$EVENT_LOG.status" ;;
                    *)
                        # close/comment leave status unchanged after the first write
                        [ -f "$EVENT_LOG.status" ] || echo '{"id":"bead-1","status":"in_progress"}' > "$EVENT_LOG.status"
                        ;;
                esac
                ;;
        esac
        ;;
    event)
        SUB=$1
        shift
        [ "$SUB" = "emit" ] || exit 0
        TYPE=$1
        shift
        # Flatten remaining args for the log: subject=..., message=...
        printf 'EVENT type=%s' "$TYPE" >> "$EVENT_LOG"
        for ARG in "$@"; do
            printf ' %s' "$ARG" >> "$EVENT_LOG"
        done
        printf '\n' >> "$EVENT_LOG"
        ;;
    mail)
        # Outbound escalation: no-op in this stub.
        ;;
esac
exit 0
STUB
chmod +x "$SANDBOX/bin/gc"

cat > "$SANDBOX/bin/kranz" <<'STUB'
#!/bin/sh
# kranz stub: prints a mission id and exits with $KRANZ_STUB_EXIT.
echo "m-123456 demo mission line"
exit "${KRANZ_STUB_EXIT:-0}"
STUB
chmod +x "$SANDBOX/bin/kranz"

cat > "$SANDBOX/mission.md" <<'EOF'
## Goal
Demo bead goal.
EOF

FAILED=0
fail() {
    echo "FAIL [$1]: expected $2, observed $3" >&2
    FAILED=1
}

run_bead() {
    EXIT_CODE=$1
    rm -f "$EVENT_LOG" "$EVENT_LOG.status"
    PATH="$SANDBOX/bin:$PATH" \
        EVENT_LOG="$EVENT_LOG" \
        KRANZ_STUB_EXIT="$EXIT_CODE" \
        sh "$RUN_BEAD" "$SANDBOX/city" "bead-1" "$SANDBOX/mission.md" "$SANDBOX/rig" "1" >/dev/null 2>&1
}

events_contain() {
    grep -qF "$1" "$EVENT_LOG" 2>/dev/null
}

# --- exit 0: started + complete ---------------------------------------------
run_bead 0
events_contain 'EVENT type=kranz.mission.started' || fail "exit-0-started" "kranz.mission.started event" "$(cat "$EVENT_LOG" 2>/dev/null)"
events_contain 'EVENT type=kranz.mission.complete' || fail "exit-0-complete" "kranz.mission.complete event" "$(cat "$EVENT_LOG" 2>/dev/null)"
if events_contain 'EVENT type=kranz.mission.blocked'; then
    fail "exit-0-no-blocked" "no blocked event on exit 0" "$(cat "$EVENT_LOG" 2>/dev/null)"
fi

# --- exit 2: started + blocked, no complete ---------------------------------
run_bead 2
events_contain 'EVENT type=kranz.mission.started' || fail "exit-2-started" "kranz.mission.started event" "$(cat "$EVENT_LOG" 2>/dev/null)"
events_contain 'EVENT type=kranz.mission.blocked' || fail "exit-2-blocked" "kranz.mission.blocked event" "$(cat "$EVENT_LOG" 2>/dev/null)"
if events_contain 'EVENT type=kranz.mission.complete'; then
    fail "exit-2-no-complete" "no complete event on exit 2" "$(cat "$EVENT_LOG" 2>/dev/null)"
fi

# --- exit 3: started only ----------------------------------------------------
run_bead 3
events_contain 'EVENT type=kranz.mission.started' || fail "exit-3-started" "kranz.mission.started event" "$(cat "$EVENT_LOG" 2>/dev/null)"
if events_contain 'EVENT type=kranz.mission.complete' || events_contain 'EVENT type=kranz.mission.blocked'; then
    fail "exit-3-terminal-silence" "no complete/blocked event on exit 3" "$(cat "$EVENT_LOG" 2>/dev/null)"
fi

# --- exit 1: started only ----------------------------------------------------
run_bead 1
events_contain 'EVENT type=kranz.mission.started' || fail "exit-1-started" "kranz.mission.started event" "$(cat "$EVENT_LOG" 2>/dev/null)"
if events_contain 'EVENT type=kranz.mission.complete' || events_contain 'EVENT type=kranz.mission.blocked'; then
    fail "exit-1-terminal-silence" "no complete/blocked event on exit 1" "$(cat "$EVENT_LOG" 2>/dev/null)"
fi

# --- subject is the bead id in every emitted event ----------------------------
awk '/^EVENT type=kranz\.mission\./ && !/--subject bead-1/ {exit 1}' "$EVENT_LOG" ||
    fail "subject" "every event to carry --subject bead-1" "$(cat "$EVENT_LOG" 2>/dev/null)"

if [ "$FAILED" -eq 0 ]; then
    echo "KRANZ-RUN-BEAD-EVENTS: PASS"
    exit 0
else
    echo "KRANZ-RUN-BEAD-EVENTS: FAIL" >&2
    exit 1
fi
