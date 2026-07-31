#!/bin/sh
# kranz-dispatch-roundtrip.sh — live-bd round-trip fixture test for the Gas
# City <-> kranz bridge.
#
# Runs entirely inside a throwaway `mktemp -d` sandbox (own bd store, own
# fixture git rig). Never touches an existing Gas City, the operator's real
# bead store, or anything outside the sandbox. Never invokes `gc init` or
# `gc stop` (docs/gascity.md:48 — launchd supervisor plus a live max-effort
# Claude session; those two calls are off-limits to any script in this repo).
#
# This test is written against the TARGET bridge behaviour (verified in
# docs/scoping/beads-bridge-dialect.md). The FIELDS case asserts behaviour
# packaging/gascity/bin/kranz-dispatch already implements (the jq type-switch
# at bin/kranz-dispatch:82, landed in commit d3f48c3) — a FIELDS failure is a
# real regression of contract assertion [a6], not an expected condition.
#
# Marker discipline: the mission's validation contract greps stdout for the
# literal "ROUNDTRIP: PASS" marker, so it must appear if and only if every
# case below actually passed. A SKIP must never also print a PASS marker.
set -u

BIN_DIR=$(CDPATH= cd -- "$(dirname -- "$0")/../bin" && pwd)
REPO_ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../../.." && pwd)

if ! command -v bd >/dev/null 2>&1; then
    echo "ROUNDTRIP: SKIP (bd not on PATH)"
    exit 0
fi

SANDBOX=$(mktemp -d "${TMPDIR:-/tmp}/kranz-dispatch-roundtrip.XXXXXX")
cleanup() {
    rm -rf "$SANDBOX"
}
trap cleanup EXIT INT TERM

FAILED=0
fail_case() {
    # $1 = case name, $2 = expected, $3 = observed
    echo "FAIL [$1]: expected $2, observed $3" >&2
    FAILED=1
}

# --- sandbox layout ---------------------------------------------------

STORE_DIR="$SANDBOX/store"
RIG_DIR="$SANDBOX/rig"
CITY_DIR="$SANDBOX/city"
SPOOL_DIR="$CITY_DIR/.gc/kranz-spool"
STUB_BIN="$SANDBOX/stubbin"
mkdir -p "$STORE_DIR" "$RIG_DIR" "$SPOOL_DIR" "$STUB_BIN"

# Fixture git rig (dispatch requires RIG_DIR/.git to exist).
( cd "$RIG_DIR" && git init -q && git config user.email "roundtrip@kranz.local" && git config user.name "roundtrip" )

# Self-contained bead store, per docs/scoping/beads-bridge-dialect.md §5
# (`bd init --non-interactive` in an arbitrary empty directory).
if ! ( cd "$STORE_DIR" && bd init --non-interactive >bd_init.log 2>&1 ); then
    echo "ROUNDTRIP: SKIP (bd init --non-interactive failed in sandbox)"
    exit 0
fi

BD() {
    ( cd "$STORE_DIR" && bd "$@" )
}

unwrap() {
    # Collapses the bd show/create/update --json outer envelope, whether it
    # is a bare object or a single-element array (dialect doc §2 recorded
    # the latter for `bd show`; be tolerant of both).
    jq 'if type == "array" then .[0] else . end'
}

# --- Case: create fixture bead -----------------------------------------

CREATE_OUT=$(BD create "Fixture bead for roundtrip" --type task --json 2>&1) || {
    fail_case "create-fixture" "bd create to succeed" "$CREATE_OUT"
}
FIXTURE_ID=$(printf '%s' "$CREATE_OUT" | unwrap | jq -r '.id // empty')
if [ -z "$FIXTURE_ID" ]; then
    fail_case "create-fixture" "a non-empty issue id" "'$CREATE_OUT'"
else
    echo "CREATE: PASS (fixture id $FIXTURE_ID)"
fi

# --- Case: atomic claim + idempotent re-claim + ready-drain coverage (bd
# update --claim, dialect doc §3 VERIFIED on the installed 1.0.5 binary) —
# verifies the live binary's atomic claim shape (status plus assignee) on
# the fixture bead, which contract assertion a2's "claimed" clause requires,
# PLUS the behaviour this milestone actually ships in kranz-dispatch: a
# fresh claim, re-claiming a bead already held by the same actor being a
# no-op success, and a claimed bead being reflected as claimed/in_progress
# by `bd ready` rather than still offered to a second dispatcher's drain.
# No lease/TTL/heartbeat/liveness logic here — that stays out of scope and
# is covered by the LEASE stub below instead.

if [ -n "$FIXTURE_ID" ]; then
    CLAIM_OUT=$(BD update "$FIXTURE_ID" --claim --json 2>&1)
    CLAIM_STATUS=$(printf '%s' "$CLAIM_OUT" | unwrap | jq -r '.status // empty')
    CLAIM_ASSIGNEE=$(printf '%s' "$CLAIM_OUT" | unwrap | jq -r '.assignee // empty')
    if [ "$CLAIM_STATUS" != "in_progress" ]; then
        fail_case "claim-status" "in_progress" "$CLAIM_STATUS"
    elif [ -z "$CLAIM_ASSIGNEE" ]; then
        fail_case "claim-assignee" "a non-empty assignee" "'$CLAIM_ASSIGNEE'"
    else
        # Re-claim the same bead (still held by the same actor): must
        # succeed (idempotent), stay in_progress, keep the same assignee.
        RECLAIM_OUT=$(BD update "$FIXTURE_ID" --claim --json 2>&1)
        RECLAIM_EXIT=$?
        RECLAIM_STATUS=$(printf '%s' "$RECLAIM_OUT" | unwrap | jq -r '.status // empty')
        RECLAIM_ASSIGNEE=$(printf '%s' "$RECLAIM_OUT" | unwrap | jq -r '.assignee // empty')
        # A claimed bead must no longer be offered by `bd ready`, so a
        # second dispatcher's drain does not re-serve it.
        READY_IDS=$(BD ready --json 2>/dev/null | jq -r '.[].id')
        STILL_READY=0
        for RID in $READY_IDS; do
            [ "$RID" = "$FIXTURE_ID" ] && STILL_READY=1
        done
        if [ "$RECLAIM_EXIT" -ne 0 ]; then
            fail_case "claim-reclaim-exit" "re-claim to succeed (exit 0)" "exit $RECLAIM_EXIT: $RECLAIM_OUT"
        elif [ "$RECLAIM_STATUS" != "in_progress" ]; then
            fail_case "claim-reclaim-status" "in_progress" "$RECLAIM_STATUS"
        elif [ "$RECLAIM_ASSIGNEE" != "$CLAIM_ASSIGNEE" ]; then
            fail_case "claim-reclaim-assignee" "same assignee '$CLAIM_ASSIGNEE'" "'$RECLAIM_ASSIGNEE'"
        elif [ "$STILL_READY" -ne 0 ]; then
            fail_case "claim-not-in-ready" "claimed bead $FIXTURE_ID absent from bd ready --json" "still listed as ready"
        else
            echo "CLAIM: PASS (atomic claim, idempotent re-claim)"
        fi
    fi
fi

# --- Case: status transitions, both directions, verified `bd update
# --status` shape; bd set-state must never be invoked by the bridge -----

status_of() {
    BD show "$1" --json 2>/dev/null | unwrap | jq -r '.status // empty'
}

assert_status() {
    # $1 = target status, $2 = step label
    BD update "$FIXTURE_ID" --status "$1" >/dev/null 2>&1
    OBSERVED=$(status_of "$FIXTURE_ID")
    if [ "$OBSERVED" != "$1" ]; then
        fail_case "status-transition:$2" "$1" "$OBSERVED"
        STATUS_OK=0
    fi
}

STATUS_OK=1
if [ -n "$FIXTURE_ID" ]; then
    assert_status "open"        "claim->open"
    assert_status "in_progress" "open->in_progress"
    assert_status "blocked"     "in_progress->blocked"
    assert_status "in_progress" "blocked->in_progress"
    assert_status "open"        "in_progress->open"
    assert_status "blocked"     "open->blocked"
    assert_status "open"        "blocked->open"
    if [ "$STATUS_OK" -eq 1 ]; then
        echo "STATUS: PASS (open/in_progress/blocked round-tripped via bd update --status; closed and closed->open covered by the CLOSE and REOPEN cases below)"
    fi
fi

# The bridge must only ever use `bd update --status`, never `bd set-state`.
SET_STATE_HITS=$(grep -rl "set-state" "$BIN_DIR" 2>/dev/null || true)
if [ -n "$SET_STATE_HITS" ]; then
    fail_case "no-set-state" "no file under packaging/gascity/bin to invoke bd set-state" "found in: $SET_STATE_HITS"
else
    echo "SET-STATE-ABSENT: PASS (bridge never invokes bd set-state)"
fi

# --- Case: close carries its reason -------------------------------------

if [ -n "$FIXTURE_ID" ]; then
    CLOSE_REASON="kranz mission COMPLETE — roundtrip fixture close"
    BD close "$FIXTURE_ID" --reason "$CLOSE_REASON" >/dev/null 2>&1
    CLOSE_OBSERVED=$(BD show "$FIXTURE_ID" --json 2>/dev/null)
    CLOSE_STATUS=$(printf '%s' "$CLOSE_OBSERVED" | unwrap | jq -r '.status // empty')
    if [ "$CLOSE_STATUS" != "closed" ]; then
        fail_case "close-status" "closed" "$CLOSE_STATUS"
    elif ! printf '%s' "$CLOSE_OBSERVED" | grep -qF "$CLOSE_REASON"; then
        fail_case "close-reason" "close reason '$CLOSE_REASON' present on the issue" "not found in bd show --json output"
    else
        echo "CLOSE: PASS (status closed, reason carried)"
    fi
fi

# --- Case: closed -> open, the fourth (reverse) status direction ------
# `bd update --status open` is tried first (live-probed 2026-07-30 against
# bd 1.0.5: it succeeds directly on a closed issue in this install); `bd
# reopen` (dialect doc Appendix) is the fallback if the live binary ever
# rejects the direct update on a closed issue.

if [ -n "$FIXTURE_ID" ]; then
    REOPEN_VERB="bd update --status open"
    BD update "$FIXTURE_ID" --status open >/dev/null 2>&1
    REOPEN_OBSERVED=$(status_of "$FIXTURE_ID")
    if [ "$REOPEN_OBSERVED" != "open" ]; then
        REOPEN_VERB="bd reopen"
        BD reopen "$FIXTURE_ID" --reason "roundtrip fixture reopen" >/dev/null 2>&1
        REOPEN_OBSERVED=$(status_of "$FIXTURE_ID")
    fi
    if [ "$REOPEN_OBSERVED" != "open" ]; then
        fail_case "reopen-closed" "open" "$REOPEN_OBSERVED"
    else
        echo "REOPEN: PASS (closed->open via $REOPEN_VERB)"
    fi
fi

# --- Case: lease-aware claim / liveness-first dead-claim recovery ------
# bd 1.0.5 exposes no lease/TTL/heartbeat field (docs/scoping/beads-bridge-
# dialect.md §3), so kranz-dispatch's recovery pass (bin/kranz-dispatch,
# ms-3-fix-1-6) builds liveness on the one timestamp bd does expose on every
# issue: `updated_at`. This case exercises the real kranz-dispatch binary
# against two real fixture beads in the sandbox store, both carrying the
# drained label: a DEAD claim (claimed once, then left untouched past
# KRANZ_CLAIM_STALE_SECONDS) and a LIVE claim (claimed immediately before
# dispatch runs, so its updated_at is fresh). The gc stub here forwards every
# bd invocation straight to the real bd binary in $STORE_DIR — no synthetic
# ids, no fabricated JSON.

LEASE_LABEL="kranz-lease-rt"
LEASE_SPOOL="$SANDBOX/lease-spool"
mkdir -p "$LEASE_SPOOL"

DEAD_CREATE_OUT=$(BD create "LEASE dead-claim fixture" --type task --labels "$LEASE_LABEL" --json 2>&1) || {
    fail_case "lease-dead-create" "bd create to succeed" "$DEAD_CREATE_OUT"
}
DEAD_ID=$(printf '%s' "$DEAD_CREATE_OUT" | unwrap | jq -r '.id // empty')

LIVE_CREATE_OUT=$(BD create "LEASE live-claim fixture" --type task --labels "$LEASE_LABEL" --json 2>&1) || {
    fail_case "lease-live-create" "bd create to succeed" "$LIVE_CREATE_OUT"
}
LIVE_ID=$(printf '%s' "$LIVE_CREATE_OUT" | unwrap | jq -r '.id // empty')

if [ -z "$DEAD_ID" ] || [ -z "$LIVE_ID" ]; then
    fail_case "lease-fixture-ids" "non-empty DEAD_ID and LIVE_ID" "DEAD_ID='$DEAD_ID' LIVE_ID='$LIVE_ID'"
else
    ( cd "$STORE_DIR" && BEADS_ACTOR=lease-dead-actor bd update "$DEAD_ID" --claim --json >/dev/null 2>&1 )

    STALE_SECONDS=2
    # Let the dead claim's updated_at age past the threshold before dispatch
    # runs its recovery pass.
    sleep $((STALE_SECONDS + 3))

    ( cd "$STORE_DIR" && BEADS_ACTOR=lease-live-actor bd update "$LIVE_ID" --claim --json >/dev/null 2>&1 )

    STUB_BIN_LEASE="$SANDBOX/stubbin-lease"
    mkdir -p "$STUB_BIN_LEASE"
    cat > "$STUB_BIN_LEASE/gc" <<STUBEOF
#!/bin/sh
set -u
if [ "\${1:-}" = "--city" ]; then
    shift 2
fi
case "\${1:-}" in
    bd)
        shift
        ( cd "$STORE_DIR" && bd "\$@" )
        ;;
    mail)
        exit 0
        ;;
    *)
        echo "gc-stub(lease): unsupported invocation: gc \$*" >&2
        exit 1
        ;;
esac
STUBEOF
    chmod +x "$STUB_BIN_LEASE/gc"

    LEASE_DISPATCH_OUT=$(GC_CITY="$CITY_DIR" KRANZ_RIG_DIR="$RIG_DIR" KRANZ_LABEL="$LEASE_LABEL" \
        KRANZ_SPOOL="$LEASE_SPOOL" KRANZ_ALLOW_UNVALIDATED=1 \
        KRANZ_CLAIM_STALE_SECONDS="$STALE_SECONDS" \
        PATH="$STUB_BIN_LEASE:$PATH" "$BIN_DIR/kranz-dispatch" 2>&1)
    LEASE_DISPATCH_STATUS=$?

    DEAD_AFTER=$(BD show "$DEAD_ID" --json 2>/dev/null | unwrap)
    DEAD_STATUS_AFTER=$(printf '%s' "$DEAD_AFTER" | jq -r '.status // empty')
    DEAD_ASSIGNEE_AFTER=$(printf '%s' "$DEAD_AFTER" | jq -r '.assignee // empty')
    LIVE_AFTER=$(BD show "$LIVE_ID" --json 2>/dev/null | unwrap)
    LIVE_STATUS_AFTER=$(printf '%s' "$LIVE_AFTER" | jq -r '.status // empty')
    LIVE_ASSIGNEE_AFTER=$(printf '%s' "$LIVE_AFTER" | jq -r '.assignee // empty')
    DEAD_BRIEF=$(ls "$LEASE_SPOOL"/*-"$DEAD_ID".md 2>/dev/null | head -1)
    LIVE_BRIEF=$(ls "$LEASE_SPOOL"/*-"$LIVE_ID".md 2>/dev/null | head -1)

    if [ "$LEASE_DISPATCH_STATUS" -ne 0 ]; then
        fail_case "lease-dispatch-ran" "kranz-dispatch to exit 0" "exit=$LEASE_DISPATCH_STATUS log: $LEASE_DISPATCH_OUT"
    elif [ "$DEAD_STATUS_AFTER" != "in_progress" ]; then
        fail_case "lease-dead-recovered-status" "in_progress (reaped, then re-claimed by dispatch)" "$DEAD_STATUS_AFTER"
    elif [ "$DEAD_ASSIGNEE_AFTER" = "lease-dead-actor" ]; then
        fail_case "lease-dead-recovered-assignee" "assignee other than the dead actor" "$DEAD_ASSIGNEE_AFTER"
    elif [ -z "$DEAD_BRIEF" ]; then
        fail_case "lease-dead-spooled" "a spooled brief for recovered dead-claim bead $DEAD_ID" "none found in $LEASE_SPOOL"
    elif [ "$LIVE_STATUS_AFTER" != "in_progress" ]; then
        fail_case "lease-live-preserved-status" "in_progress (untouched)" "$LIVE_STATUS_AFTER"
    elif [ "$LIVE_ASSIGNEE_AFTER" != "lease-live-actor" ]; then
        fail_case "lease-live-preserved-assignee" "lease-live-actor (never stolen)" "$LIVE_ASSIGNEE_AFTER"
    elif [ -n "$LIVE_BRIEF" ]; then
        fail_case "lease-live-not-spooled" "no spooled brief for the still-live claim $LIVE_ID" "found: $LIVE_BRIEF"
    else
        echo "LEASE: PASS (dead claim recovered and re-claimed, live claim preserved)"
    fi
fi

# --- Case: field translation is type-correct (acceptance_criteria) -----
# Exercises the real kranz-dispatch script's brief-generation logic. The
# STRING case is real end-to-end: a fixture bead with a string-valued
# acceptance_criteria is created in the sandbox store via the live `bd`
# binary (`bd create --acceptance "..."`, confirmed 2026-07-30 to produce
# an `acceptance_criteria` string field in `bd show --json`), and the `gc`
# stub's `show` branch for that id execs real `bd show <id> --json` against
# $STORE_DIR and passes its output through unmodified — so this case
# exercises the true `bd show --json` outer-envelope shape, not a
# stub-authored one. The ARRAY case stays synthetic: bd 1.0.5's CLI has no
# way to store an array-valued acceptance_criteria (`bd create --help` /
# `bd update --help` only expose `--acceptance string`), so a JSON-array
# payload can only be produced by fabricating the `bd show --json` response.
# Every other bridge call (`gc bd ready`, `gc bd update`, `gc bd comment`)
# still resolves to a synthetic no-op below; no real bd store is mutated by
# the ARRAY case.

STRING_HINT_TEXT="Do the thing and verify it works."
STRING_CREATE_OUT=$(BD create "FIELDS string fixture" --type task --acceptance "$STRING_HINT_TEXT" --json 2>&1) || {
    fail_case "fields-string-create" "bd create --acceptance to succeed" "$STRING_CREATE_OUT"
}
STRING_ID=$(printf '%s' "$STRING_CREATE_OUT" | unwrap | jq -r '.id // empty')
ARRAY_ID="rt-array-1"
NOACCEPT_ID="rt-noaccept-1"
EMPTYARR_ID="rt-emptyarr-1"
GC_CALLS_LOG="$SANDBOX/gc-calls.log"
: > "$GC_CALLS_LOG"

# The gc stub logs every invocation verbatim, and forwards update/comment
# (and, for the RUNBEAD cases below, close) to the real bd binary in
# $STORE_DIR whenever the target id really exists there — the synthetic
# ARRAY_ID/NOACCEPT_ID/EMPTYARR_ID ids have no real bead behind them, so
# they stay a no-op. It also accepts the `gc --city <dir> bd ...` and
# `gc --city <dir> mail send human ...` shapes kranz-run-bead's bd()
# wrapper uses.
cat > "$STUB_BIN/gc" <<STUBEOF
#!/bin/sh
set -u
echo "gc \$*" >> "$GC_CALLS_LOG"
if [ "\${1:-}" = "--city" ]; then
    shift 2
fi
case "\${1:-}" in
    bd)
        shift
        case "\${1:-}" in
            ready)
                echo '[{"id":"$STRING_ID"},{"id":"$ARRAY_ID"},{"id":"$NOACCEPT_ID"},{"id":"$EMPTYARR_ID"}]'
                ;;
            list)
                ( cd "$STORE_DIR" && bd "\$@" )
                ;;
            show)
                ID=\$2
                if [ "\$ID" = "$STRING_ID" ]; then
                    ( cd "$STORE_DIR" && bd show "\$ID" --json )
                elif [ "\$ID" = "$ARRAY_ID" ]; then
                    printf '[{"id":"%s","title":"Array case","description":"desc","acceptance_criteria":["First hint","Second hint"]}]' "\$ID"
                elif [ "\$ID" = "$NOACCEPT_ID" ]; then
                    printf '[{"id":"%s","title":"No-acceptance case","description":"desc"}]' "\$ID"
                elif [ "\$ID" = "$EMPTYARR_ID" ]; then
                    printf '[{"id":"%s","title":"Empty-array case","description":"desc","acceptance_criteria":[]}]' "\$ID"
                elif ( cd "$STORE_DIR" && bd show "\$ID" --json >/dev/null 2>&1 ); then
                    ( cd "$STORE_DIR" && bd show "\$ID" --json )
                else
                    echo "gc-stub: unknown id \$ID" >&2
                    exit 1
                fi
                ;;
            update|comment|close)
                ID=\$2
                if ( cd "$STORE_DIR" && bd show "\$ID" --json >/dev/null 2>&1 ); then
                    ( cd "$STORE_DIR" && bd "\$@" )
                else
                    exit 0
                fi
                ;;
            *)
                echo "gc-stub: unsupported bd subcommand: \$1" >&2
                exit 1
                ;;
        esac
        ;;
    mail)
        exit 0
        ;;
    *)
        echo "gc-stub: unsupported invocation: gc \$*" >&2
        exit 1
        ;;
esac
STUBEOF
chmod +x "$STUB_BIN/gc"

# Stub `kranz` for the RUNBEAD cases below: echoes one summary line and
# exits with whatever code KRANZ_STUB_EXIT names, simulating a mission run
# without ever executing a real kranz/Claude session.
cat > "$STUB_BIN/kranz" <<'STUBEOF'
#!/bin/sh
set -u
echo "kranz-stub: simulated mission run (exit ${KRANZ_STUB_EXIT:-0})"
exit "${KRANZ_STUB_EXIT:-0}"
STUBEOF
chmod +x "$STUB_BIN/kranz"

DISPATCH_LOG=$(GC_CITY="$CITY_DIR" KRANZ_RIG_DIR="$RIG_DIR" KRANZ_LABEL="kranz" \
    KRANZ_SPOOL="$SPOOL_DIR" KRANZ_ALLOW_UNVALIDATED=1 \
    PATH="$STUB_BIN:$PATH" "$BIN_DIR/kranz-dispatch" 2>&1)
DISPATCH_STATUS=$?

STRING_BRIEF=$(ls "$SPOOL_DIR"/*-"$STRING_ID".md 2>/dev/null | head -1)
ARRAY_BRIEF=$(ls "$SPOOL_DIR"/*-"$ARRAY_ID".md 2>/dev/null | head -1)
NOACCEPT_BRIEF=$(ls "$SPOOL_DIR"/*-"$NOACCEPT_ID".md 2>/dev/null | head -1)
EMPTYARR_BRIEF=$(ls "$SPOOL_DIR"/*-"$EMPTYARR_ID".md 2>/dev/null | head -1)

if [ $DISPATCH_STATUS -ne 0 ] || [ -z "$STRING_BRIEF" ] || [ -z "$ARRAY_BRIEF" ] ||
    [ -z "$NOACCEPT_BRIEF" ] || [ -z "$EMPTYARR_BRIEF" ]; then
    fail_case "fields-dispatch-ran" "kranz-dispatch to spool a brief for all four fixture ids" "exit=$DISPATCH_STATUS log: $DISPATCH_LOG"
else
    STRING_HINTS=$(awk '/^## Acceptance hints$/{f=1;next}/^## /{f=0}f' "$STRING_BRIEF")
    ARRAY_HINTS=$(awk '/^## Acceptance hints$/{f=1;next}/^## /{f=0}f' "$ARRAY_BRIEF")
    NOACCEPT_HINTS=$(awk '/^## Acceptance hints$/{f=1;next}/^## /{f=0}f' "$NOACCEPT_BRIEF")
    EMPTYARR_HINTS=$(awk '/^## Acceptance hints$/{f=1;next}/^## /{f=0}f' "$EMPTYARR_BRIEF")
    FALLBACK_TEXT="The goal above is demonstrably met."

    if ! printf '%s' "$STRING_HINTS" | grep -qF "$STRING_HINT_TEXT"; then
        fail_case "fields-string-verbatim" "string acceptance_criteria carried verbatim" "$STRING_HINTS"
    elif printf '%s' "$STRING_HINTS" | grep -qE '[][]|"'; then
        fail_case "fields-string-no-raw-json" "no raw JSON bracket/quote text in the string case" "$STRING_HINTS"
    else
        echo "FIELDS-STRING: PASS"
    fi

    ARRAY_HINT_LINES=$(printf '%s\n' "$ARRAY_HINTS" | grep -c '.')
    if [ "$ARRAY_HINT_LINES" -ne 2 ]; then
        fail_case "fields-array-line-count" "exactly two non-empty hint lines" "$ARRAY_HINT_LINES lines: $ARRAY_HINTS"
    elif ! printf '%s\n' "$ARRAY_HINTS" | grep -qx "First hint"; then
        fail_case "fields-array-first-wholeline" "'First hint' as a whole line" "$ARRAY_HINTS"
    elif ! printf '%s\n' "$ARRAY_HINTS" | grep -qx "Second hint"; then
        fail_case "fields-array-second-wholeline" "'Second hint' as a whole line" "$ARRAY_HINTS"
    elif printf '%s' "$ARRAY_HINTS" | grep -qE '[][]|"'; then
        fail_case "fields-array-no-raw-json" "no raw JSON bracket/quote text in the array case (newline-joined entries expected)" "$ARRAY_HINTS"
    else
        echo "FIELDS-ARRAY: PASS"
    fi

    if [ "$NOACCEPT_HINTS" != "$FALLBACK_TEXT" ]; then
        fail_case "fields-noaccept-fallback" "'$FALLBACK_TEXT'" "'$NOACCEPT_HINTS'"
    else
        echo "FIELDS-NOACCEPT: PASS (missing acceptance_criteria falls back)"
    fi

    if [ "$EMPTYARR_HINTS" != "$FALLBACK_TEXT" ]; then
        fail_case "fields-emptyarr-fallback" "'$FALLBACK_TEXT'" "'$EMPTYARR_HINTS'"
    else
        echo "FIELDS-EMPTYARR: PASS (empty array acceptance_criteria falls back)"
    fi

    STRING_LIVE_STATUS=$(status_of "$STRING_ID")
    if [ "$STRING_LIVE_STATUS" != "in_progress" ]; then
        fail_case "gc-stub-forward-update" "live status in_progress for real fixture $STRING_ID after dispatch" "$STRING_LIVE_STATUS"
    elif ! grep -qE "update ${ARRAY_ID} --claim\$" "$GC_CALLS_LOG" 2>/dev/null; then
        fail_case "gc-stub-log-array-update" "gc-calls.log to log an atomic claim for synthetic id $ARRAY_ID carrying exactly --claim" "$(cat "$GC_CALLS_LOG" 2>/dev/null)"
    else
        echo "GC-STUB: PASS (forwards update/comment to real bd for a live id, logs every invocation, no-ops the synthetic id)"
    fi

    if [ "$FAILED" -eq 0 ]; then
        echo "FIELDS: PASS (acceptance_criteria string+array)"
    fi
fi

# --- Case: kranz-run-bead exit-code -> live bead status translation ----
# Exercises the real packaging/gascity/bin/kranz-run-bead against a fresh
# fixture bead per case (closed is terminal, so each case needs its own
# bead). Each fixture is claimed to in_progress first, mirroring what
# kranz-dispatch does before spooling, so the assertion actually proves a
# transition rather than an already-true tautology.

run_runbead_case() {
    # $1 = exit code for the kranz stub to simulate
    # $2 = expected resulting live bead status
    # $3 = case label
    CASE_CREATE_OUT=$(BD create "RUNBEAD fixture ($3)" --type task --json 2>&1)
    CASE_ID=$(printf '%s' "$CASE_CREATE_OUT" | unwrap | jq -r '.id // empty')
    if [ -z "$CASE_ID" ]; then
        fail_case "runbead-$3-create" "a non-empty issue id" "'$CASE_CREATE_OUT'"
        return 1
    fi
    BD update "$CASE_ID" --status in_progress >/dev/null 2>&1

    MFILE="$SANDBOX/runbead-$3-mission.md"
    printf '## Goal\nRUNBEAD fixture (%s)\n' "$3" > "$MFILE"

    KRANZ_STUB_EXIT="$1" PATH="$STUB_BIN:$PATH" \
        "$BIN_DIR/kranz-run-bead" "$CITY_DIR" "$CASE_ID" "$MFILE" "$RIG_DIR" 1 >/dev/null 2>&1

    OBSERVED=$(status_of "$CASE_ID")
    if [ "$OBSERVED" != "$2" ]; then
        fail_case "runbead-$3" "$2" "$OBSERVED"
        return 1
    fi

    if [ "$3" = "exit2" ]; then
        COMMENT_CHECK=$(BD show "$CASE_ID" --json --include-comments 2>/dev/null)
        if ! printf '%s' "$COMMENT_CHECK" | grep -qF "kranz mission BLOCKED"; then
            fail_case "runbead-exit2-comment" "a BLOCKED comment recorded on $CASE_ID" "not found in: $COMMENT_CHECK"
            return 1
        fi
    fi
    return 0
}

RUNBEAD_OK=1
run_runbead_case 0 "closed"   "exit0" || RUNBEAD_OK=0
run_runbead_case 2 "blocked"  "exit2" || RUNBEAD_OK=0
run_runbead_case 3 "open"     "exit3" || RUNBEAD_OK=0
run_runbead_case 1 "open"     "exit1" || RUNBEAD_OK=0

if [ "$RUNBEAD_OK" -eq 1 ]; then
    echo "RUNBEAD: PASS (exit 0/2/3/1 map to live bead states)"
fi

# --- Verdict --------------------------------------------------------------

if [ "$FAILED" -ne 0 ]; then
    echo "ROUNDTRIP: FAIL" >&2
    exit 1
fi

BD_VERSION=$(bd --version 2>&1 | sed 's/^bd version //')
echo "ROUNDTRIP: PASS (live bd $BD_VERSION)"
exit 0
