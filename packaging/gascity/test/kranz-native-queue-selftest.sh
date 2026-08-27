#!/bin/sh
# Stub-safe proof for the KRANZ_NATIVE_QUEUE=1 bridge. No live city, bead
# store, agent account, or mission is used: gc and kranz are deterministic
# stubs, while the production dispatch/worker/run-bead scripts run unchanged.
set -eu

BIN_DIR=$(CDPATH= cd -- "$(dirname -- "$0")/../bin" && pwd)
SANDBOX=$(mktemp -d "${TMPDIR:-/tmp}/kranz-native-queue.XXXXXX")
cleanup() {
    rm -rf "$SANDBOX"
}
trap cleanup EXIT INT TERM

CITY="$SANDBOX/city"
RIG_DIR="$SANDBOX/rig with spaces"
STUB_BIN="$SANDBOX/stubbin"
STATUS_FILE="$SANDBOX/bead.status"
GC_LOG="$SANDBOX/gc.log"
WORK_COUNT="$SANDBOX/work.count"
WORK_LOG="$SANDBOX/work.log"
EXEC_COUNT="$SANDBOX/exec.count"
FAIL_CLOSE_ONCE="$SANDBOX/fail-close-once"
mkdir -p "$CITY/.gc" "$RIG_DIR/.git" "$STUB_BIN"
printf '%s\n' open > "$STATUS_FILE"
: > "$GC_LOG"
: > "$WORK_COUNT"
: > "$WORK_LOG"
printf '%s\n' 0 > "$EXEC_COUNT"
: > "$FAIL_CLOSE_ONCE"

fail() {
    echo "FAIL [$1]: expected $2, observed $3" >&2
    exit 1
}

cat > "$STUB_BIN/gc" <<'GC_STUB'
#!/bin/sh
set -u
if [ "${1:-}" = "--city" ]; then
    shift 2
fi
printf '%s\n' "$*" >> "$GC_LOG"

write_status() {
    printf '%s\n' "$1" > "$STATUS_FILE.tmp.$$"
    mv -f "$STATUS_FILE.tmp.$$" "$STATUS_FILE"
}

case "${1:-} ${2:-}" in
    "bd list")
        STATUS=$(cat "$STATUS_FILE")
        if [ "$STATUS" = "in_progress" ]; then
            jq -n '[{id:"rig-1",labels:["kranz"],status:"in_progress",updated_at:"2020-01-01T00:00:00Z"}]'
        else
            printf '%s\n' '[]'
        fi
        ;;
    "bd ready")
        if [ "$(cat "$STATUS_FILE")" = "open" ]; then
            printf '%s\n' '[{"id":"rig-1"}]'
        else
            printf '%s\n' '[]'
        fi
        ;;
    "bd show")
        SHOW_ID=${3:-}
        if [ "$SHOW_ID" = "rig-1" ]; then
            STATUS=$(cat "$STATUS_FILE")
            jq -n --arg status "$STATUS" '{id:"rig-1",title:"Gas City bead: victim",description:"exercise native dispatch",acceptance_criteria:"mission completes",labels:["kranz"],status:$status,external_ref:"",updated_at:"2020-01-01T00:00:00Z",comments:[]}'
        else
            jq -n --arg id "$SHOW_ID" '{id:$id,title:"foreign",labels:[],status:"open",updated_at:"2020-01-01T00:00:00Z",comments:[]}'
        fi
        ;;
    "bd update")
        if [ "${3:-}" = "rig-1" ]; then
            case " $* " in
                *" --claim "*) write_status in_progress ;;
                *" --status open "*) write_status open ;;
                *" --status blocked "*) write_status blocked ;;
                *) : ;;
            esac
        fi
        ;;
    "bd close")
        [ "${3:-}" = "rig-1" ] || exit 1
        if [ -e "$FAIL_CLOSE_ONCE" ]; then
            rm -f "$FAIL_CLOSE_ONCE"
            exit 1
        fi
        write_status closed
        ;;
    "bd comment" | "event emit" | "mail send")
        ;;
    *)
        echo "unexpected gc invocation: $*" >&2
        exit 1
        ;;
esac
GC_STUB
chmod +x "$STUB_BIN/gc"

cat > "$STUB_BIN/kranz" <<'KRANZ_STUB'
#!/bin/sh
set -u
case ${1:-} in
    exec)
        shift
        MFILE=""
        SAW_ENQUEUE=0
        SOURCE=""
        EXTERNAL_REF=""
        while [ "$#" -gt 0 ]; do
            case $1 in
                -f | --file)
                    MFILE=$2
                    shift 2
                    ;;
                --enqueue)
                    SAW_ENQUEUE=1
                    shift
                    ;;
                --enqueue-source)
                    SOURCE=$2
                    shift 2
                    ;;
                --enqueue-external-ref)
                    EXTERNAL_REF=$2
                    shift 2
                    ;;
                --max-cycles)
                    shift 2
                    ;;
                *) shift ;;
            esac
        done
        [ "$SAW_ENQUEUE" -eq 1 ] || { echo "exec missing --enqueue" >&2; exit 1; }
        [ "$SOURCE" = "gascity" ] || { echo "exec missing gascity source" >&2; exit 1; }
        [ "$EXTERNAL_REF" = "rig-1" ] || { echo "exec missing rig-1 external ref" >&2; exit 1; }
        grep -q '^Gas City bead: rig-1$' "$MFILE" || {
            echo "mission brief missing Gas City bead marker" >&2
            exit 1
        }
        COUNT=$(cat "$EXEC_COUNT")
        COUNT=$((COUNT + 1))
        printf '%s\n' "$COUNT" > "$EXEC_COUNT"
        case $COUNT in
            1) MID=m-abc123 ;;
            2) MID=m-def456 ;;
            *) echo "unexpected exec count $COUNT" >&2; exit 1 ;;
        esac
        mkdir -p .kranz/queue ".kranz/missions/$MID"
        jq -n --arg missionId "$MID" --arg externalRef "$EXTERNAL_REF" --argjson createdUnixSecs "$(date +%s)" \
            '{schemaVersion:1,missionId:$missionId,producer:"gascity",externalRef:$externalRef,createdUnixSecs:$createdUnixSecs}' \
            > ".kranz/missions/$MID/enqueue-source.json"
        printf '{"missionId":"%s","priority":2,"seq":%s}\n' "$MID" "$((COUNT - 1))" > ".kranz/queue/002-0000000000000000000$((COUNT - 1))-$MID.json"
        jq -n --arg id "$MID" --arg goal "$(cat "$MFILE")" '{mission:{id:$id,goal:$goal,status:"approved"}}' > ".kranz/missions/$MID/state.json"
        echo "kranz exec $MID QUEUED cost=\$0.01 branch=kranz/mission-$MID seq=$((COUNT - 1))"
        ;;
    work)
        printf '%s\n' "$*" >> "$WORK_LOG"
        EXPECTED=""
        if [ "${2:-}" = "--once" ] && [ "${3:-}" = "--expect" ]; then
            EXPECTED=${4:-}
        fi
        ENTRY=$(find .kranz/queue -name '*.json' -type f | sort | head -1)
        [ -n "$ENTRY" ] || { echo "queue empty"; exit 0; }
        MID=$(jq -r '.missionId' < "$ENTRY")
        [ -z "$EXPECTED" ] || [ "$EXPECTED" = "$MID" ] || {
            echo "expected $EXPECTED, found $MID" >&2
            exit 1
        }
        COUNT=$(wc -l < "$WORK_COUNT" | tr -d ' ')
        printf '%s\n' work >> "$WORK_COUNT"
        jq '.mission.status = "complete"' ".kranz/missions/$MID/state.json" > ".kranz/missions/$MID/state.json.tmp"
        mv -f ".kranz/missions/$MID/state.json.tmp" ".kranz/missions/$MID/state.json"
        rm -f "$ENTRY"
        echo "running mission $MID from the queue"
        echo "ran: $MID (prior count $COUNT)"
        ;;
    queue)
        [ "${2:-}" = "--remove" ] || exit 1
        rm -f .kranz/queue/*-"${3:-}".json
        echo "removed ${3:-} from the queue"
        ;;
    abandon)
        MID=${2:-}
        jq '.mission.status = "abandoned"' ".kranz/missions/$MID/state.json" > ".kranz/missions/$MID/state.json.tmp"
        mv -f ".kranz/missions/$MID/state.json.tmp" ".kranz/missions/$MID/state.json"
        ;;
    *)
        echo "unexpected kranz invocation: $*" >&2
        exit 1
        ;;
esac
KRANZ_STUB
chmod +x "$STUB_BIN/kranz"

export GC_LOG STATUS_FILE WORK_COUNT WORK_LOG EXEC_COUNT FAIL_CLOSE_ONCE RIG_DIR
export PATH="$STUB_BIN:$PATH"

DISPATCH_OUT=$(GC_CITY="$CITY" KRANZ_RIG_DIR="$RIG_DIR" KRANZ_NATIVE_QUEUE=1 \
    "$BIN_DIR/kranz-dispatch" 2>&1)
case $DISPATCH_OUT in
    *"bead rig-1 -> native queue mission m-abc123"*) : ;;
    *) fail dispatch "a native queue handoff" "$DISPATCH_OUT" ;;
esac
[ -f "$RIG_DIR/.kranz/queue/002-00000000000000000000-m-abc123.json" ] ||
    fail enqueue "a native QueueEntry" "queue file absent"
[ "$(jq -r '.externalRef' < "$RIG_DIR/.kranz/missions/m-abc123/enqueue-source.json")" = "rig-1" ] ||
    fail source-binding "structured externalRef rig-1" "source receipt missing or wrong"
[ ! -d "$CITY/.gc/kranz-spool" ] ||
    fail no-spool "no private spool directory" "spool directory exists"
echo "NATIVE-DISPATCH: PASS (approved mission queued; no private spool)"

# An old City updated_at must not make reclaim cancel a perfectly healthy
# native entry that is merely waiting in the durable queue.
GC_CITY="$CITY" KRANZ_RIG_DIR="$RIG_DIR" KRANZ_NATIVE_QUEUE=1 KRANZ_CLAIM_TTL=1 \
    "$BIN_DIR/kranz-dispatch" --reclaim >/dev/null 2>&1
[ "$(cat "$STATUS_FILE")" = "in_progress" ] ||
    fail queued-ttl "in_progress durable ownership" "$(cat "$STATUS_FILE")"
[ -f "$RIG_DIR/.kranz/queue/002-00000000000000000000-m-abc123.json" ] ||
    fail queued-ttl "queue entry preserved" "queue entry removed"
echo "NATIVE-RECLAIM: PASS (durable queue ownership survives City TTL)"

# The first City close fails after the mission completed. The runnable queue
# entry must still be consumed exactly once, while a return-only receipt keeps
# the missing City transition recoverable.
set +e
GC_CITY="$CITY" KRANZ_RIG_DIR="$RIG_DIR" KRANZ_NATIVE_QUEUE=1 \
    "$BIN_DIR/kranz-city-worker" --once >/dev/null 2>&1
FIRST_CODE=$?
set -e
[ "$FIRST_CODE" -eq 75 ] || fail once-exit "exit 75 for unsettled City return" "$FIRST_CODE"
PENDING="$RIG_DIR/.kranz/missions/m-abc123/gascity-return-pending.json"
[ -f "$PENDING" ] || fail return-receipt "a pending City return receipt" "receipt absent"
[ "$(cat "$STATUS_FILE")" = "in_progress" ] ||
    fail first-close "in_progress after the injected close failure" "$(cat "$STATUS_FILE")"
[ "$(wc -l < "$WORK_COUNT" | tr -d ' ')" -eq 1 ] ||
    fail single-run "one kranz work invocation" "$(cat "$WORK_COUNT")"
grep -q '^work --once --expect m-abc123$' "$WORK_LOG" ||
    fail expected-front "work --once --expect m-abc123" "$(cat "$WORK_LOG")"

# The next worker pass drains the return receipt before looking for runnable
# work. It closes the bead and deletes the receipt without another work call.
GC_CITY="$CITY" KRANZ_RIG_DIR="$RIG_DIR" KRANZ_NATIVE_QUEUE=1 \
    "$BIN_DIR/kranz-city-worker" --once >/dev/null 2>&1
[ "$(cat "$STATUS_FILE")" = "closed" ] ||
    fail return-retry closed "$(cat "$STATUS_FILE")"
[ ! -e "$PENDING" ] || fail return-retry "receipt removed" "receipt remains"
[ -f "$RIG_DIR/.kranz/missions/m-abc123/enqueue-source.returned.json" ] ||
    fail return-retry "archived producer receipt" "returned receipt absent"
[ "$(wc -l < "$WORK_COUNT" | tr -d ' ')" -eq 1 ] ||
    fail no-replay "still one kranz work invocation" "$(cat "$WORK_COUNT")"

echo "NATIVE-RETURN: PASS (failed City write retried without mission replay)"

# A normal sibling kranz dispatcher is allowed to consume the shared queue.
# The durable source receipt must still let the City worker discover and
# return that terminal result; queue presence is not the ownership record.
printf '%s\n' open > "$STATUS_FILE"
: > "$FAIL_CLOSE_ONCE"
DISPATCH_OUT=$(GC_CITY="$CITY" KRANZ_RIG_DIR="$RIG_DIR" KRANZ_NATIVE_QUEUE=1 \
    "$BIN_DIR/kranz-dispatch" 2>&1)
case $DISPATCH_OUT in
    *"bead rig-1 -> native queue mission m-def456"*) : ;;
    *) fail sibling-dispatch "mission m-def456" "$DISPATCH_OUT" ;;
esac
( cd "$RIG_DIR" && kranz work --once >/dev/null )
[ ! -e "$RIG_DIR/.kranz/queue/002-00000000000000000001-m-def456.json" ] ||
    fail sibling-drain "queue entry consumed" "queue entry remains"
set +e
GC_CITY="$CITY" KRANZ_RIG_DIR="$RIG_DIR" KRANZ_NATIVE_QUEUE=1 \
    "$BIN_DIR/kranz-city-worker" --once >/dev/null 2>&1
SIBLING_CODE=$?
set -e
[ "$SIBLING_CODE" -eq 75 ] || fail sibling-return "exit 75 after injected City failure" "$SIBLING_CODE"
[ -f "$RIG_DIR/.kranz/missions/m-def456/gascity-return-pending.json" ] ||
    fail sibling-return "pending return receipt" "receipt absent"
GC_CITY="$CITY" KRANZ_RIG_DIR="$RIG_DIR" KRANZ_NATIVE_QUEUE=1 \
    "$BIN_DIR/kranz-city-worker" --once >/dev/null 2>&1
[ "$(cat "$STATUS_FILE")" = "closed" ] ||
    fail sibling-return closed "$(cat "$STATUS_FILE")"
[ -f "$RIG_DIR/.kranz/missions/m-def456/enqueue-source.returned.json" ] ||
    fail sibling-return "archived producer receipt" "returned receipt absent"
[ "$(wc -l < "$WORK_COUNT" | tr -d ' ')" -eq 2 ] ||
    fail sibling-no-replay "two total work invocations" "$(cat "$WORK_COUNT")"
echo "NATIVE-SIBLING: PASS (generic drain still returns to City without replay)"

# Planning may have held the City claim longer than its TTL before the source
# write. A fresh approved source without a queue file is still the live
# source-before-queue interval and must not be reclaimed using that older City
# timestamp.
printf '%s\n' in_progress > "$STATUS_FILE"
mkdir -p "$RIG_DIR/.kranz/missions/m-fff111"
jq -n --argjson createdUnixSecs "$(date +%s)" \
    '{schemaVersion:1,missionId:"m-fff111",producer:"gascity",externalRef:"rig-1",createdUnixSecs:$createdUnixSecs}' \
    > "$RIG_DIR/.kranz/missions/m-fff111/enqueue-source.json"
printf '%s\n' '{"mission":{"id":"m-fff111","status":"approved"}}' \
    > "$RIG_DIR/.kranz/missions/m-fff111/state.json"
GC_CITY="$CITY" KRANZ_RIG_DIR="$RIG_DIR" KRANZ_NATIVE_QUEUE=1 KRANZ_CLAIM_TTL=120 \
    "$BIN_DIR/kranz-dispatch" --reclaim >/dev/null 2>&1
[ "$(cat "$STATUS_FILE")" = "in_progress" ] ||
    fail fresh-source "in_progress bead" "$(cat "$STATUS_FILE")"
[ -f "$RIG_DIR/.kranz/missions/m-fff111/enqueue-source.json" ] ||
    fail fresh-source "active source receipt" "source receipt was reclaimed"
rm -f "$RIG_DIR/.kranz/missions/m-fff111/enqueue-source.json" \
    "$RIG_DIR/.kranz/missions/m-fff111/state.json"
rmdir "$RIG_DIR/.kranz/missions/m-fff111"
echo "NATIVE-PREQUEUE: PASS (fresh source uses receipt-local TTL)"

# A crash after the source receipt but before queue visibility leaves an
# approved mission with no queue owner. It must age into normal reclaim,
# reopen the bead, and retire the orphaned binding rather than standing
# forever merely because the source file exists.
printf '%s\n' in_progress > "$STATUS_FILE"
mkdir -p "$RIG_DIR/.kranz/missions/m-eee999"
printf '%s\n' '{"schemaVersion":1,"missionId":"m-eee999","producer":"gascity","externalRef":"rig-1","createdUnixSecs":1}' \
    > "$RIG_DIR/.kranz/missions/m-eee999/enqueue-source.json"
printf '%s\n' '{"mission":{"id":"m-eee999","status":"approved"}}' \
    > "$RIG_DIR/.kranz/missions/m-eee999/state.json"
GC_CITY="$CITY" KRANZ_RIG_DIR="$RIG_DIR" KRANZ_NATIVE_QUEUE=1 KRANZ_CLAIM_TTL=1 \
    "$BIN_DIR/kranz-dispatch" --reclaim >/dev/null 2>&1
[ "$(cat "$STATUS_FILE")" = "open" ] ||
    fail orphan-reclaim "open bead" "$(cat "$STATUS_FILE")"
[ -f "$RIG_DIR/.kranz/missions/m-eee999/enqueue-source.cancelled.json" ] ||
    fail orphan-reclaim "cancelled source receipt" "active receipt remains"
[ ! -e "$RIG_DIR/.kranz/missions/m-eee999/enqueue-source.json" ] ||
    fail orphan-reclaim "no active source receipt" "active receipt remains"
[ "$(jq -r '.mission.status' < "$RIG_DIR/.kranz/missions/m-eee999/state.json")" = "abandoned" ] ||
    fail orphan-reclaim "abandoned mission" "mission not abandoned"
echo "NATIVE-ORPHAN: PASS (pre-queue crash window reclaims after TTL)"

echo "KRANZ-NATIVE-QUEUE: PASS"
