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
# over acceptance_criteria that builds ACCEPT in the dispatch loop, landed in
# commit d3f48c3) — a FIELDS failure is a real regression of contract
# assertion [a6], not an expected condition.
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

# --- Case: atomic claim + idempotent re-claim + competing-claim-fails +
# ready-drain coverage (bd update --claim, dialect doc §3 VERIFIED on the
# installed 1.0.5 binary) — verifies the live binary's atomic claim shape
# (status plus assignee) on the fixture bead, which contract assertion a2's
# "claimed" clause requires, PLUS the behaviour this milestone actually
# ships in kranz-dispatch: a fresh claim, re-claiming a bead already held by
# the same actor being a no-op success, a DIFFERENT actor's claim attempt on
# an already-claimed bead FAILING outright (native bd 1.0.5 semantics, not
# anything the bridge builds), and a claimed bead being reflected as
# claimed/in_progress by `bd ready` rather than still offered to a second
# dispatcher's drain.
#
# No bd-native lease/TTL/heartbeat field is used here: bd 1.0.5 has none
# (docs/scoping/beads-bridge-dialect.md §3, executed probe), so the bridge
# ships its own client-side lease instead (f-3-1; see the LEASE and
# LEASE-TTL cases below — kranz-run-bead renews a heartbeat file during a
# mission, kranz-dispatch --reclaim sweeps liveness-first). The history:
# an updated_at-staleness heuristic (dbe5cf8) was reverted (5a7585c)
# because it could reap a verifiably-alive queued bead; the shipped design
# answers that failure mode by making liveness authoritative — a live
# pid's claim is never reaped — with TTL strictly as the backstop for
# lease-less claims (operator sign-off D-BW-2, 2026-07-29).

if [ -n "$FIXTURE_ID" ]; then
    # Positive control: before any claim, the fixture must actually be
    # listed by `bd ready` — otherwise the later "absent from ready"
    # assertion would pass vacuously (e.g. if the label/filter were wrong).
    PRECLAIM_READY_IDS=$(BD ready --json 2>/dev/null | jq -r '.[].id')
    PRECLAIM_LISTED=0
    for RID in $PRECLAIM_READY_IDS; do
        [ "$RID" = "$FIXTURE_ID" ] && PRECLAIM_LISTED=1
    done
    if [ "$PRECLAIM_LISTED" -ne 1 ]; then
        fail_case "claim-in-ready-before-claim" "unclaimed fixture $FIXTURE_ID listed by bd ready --json" "not listed: $PRECLAIM_READY_IDS"
    fi

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
        # A different actor's claim attempt on the same, already-claimed
        # bead must fail outright (native bd 1.0.5 semantics: "issue already
        # claimed by <assignee>"), leaving status and assignee untouched.
        # Assert the error TEXT, not merely a non-zero exit — a bare exit
        # code would also pass on an unrelated failure (a moved --actor
        # flag, a cd failure, a store-permission error) while proving
        # nothing about competing-claim semantics.
        COMPETE_OUT=$( (cd "$STORE_DIR" && bd --actor "kranz-roundtrip-competitor" update "$FIXTURE_ID" --claim --json) 2>&1)
        COMPETE_EXIT=$?
        POST_COMPETE_SHOW=$(BD show "$FIXTURE_ID" --json 2>/dev/null | unwrap)
        POST_COMPETE_STATUS=$(printf '%s' "$POST_COMPETE_SHOW" | jq -r '.status // empty')
        POST_COMPETE_ASSIGNEE=$(printf '%s' "$POST_COMPETE_SHOW" | jq -r '.assignee // empty')

        if [ "$RECLAIM_EXIT" -ne 0 ]; then
            fail_case "claim-reclaim-exit" "re-claim to succeed (exit 0)" "exit $RECLAIM_EXIT: $RECLAIM_OUT"
        elif [ "$RECLAIM_STATUS" != "in_progress" ]; then
            fail_case "claim-reclaim-status" "in_progress" "$RECLAIM_STATUS"
        elif [ "$RECLAIM_ASSIGNEE" != "$CLAIM_ASSIGNEE" ]; then
            fail_case "claim-reclaim-assignee" "same assignee '$CLAIM_ASSIGNEE'" "'$RECLAIM_ASSIGNEE'"
        elif [ "$STILL_READY" -ne 0 ]; then
            fail_case "claim-not-in-ready" "claimed bead $FIXTURE_ID absent from bd ready --json" "still listed as ready"
        elif [ "$COMPETE_EXIT" -eq 0 ]; then
            fail_case "claim-competing-fails" "a different actor's claim to fail (non-zero exit)" "exit 0: $COMPETE_OUT"
        elif ! printf '%s' "$COMPETE_OUT" | grep -qiE 'already claimed'; then
            fail_case "claim-competing-error-text" "error text matching 'already claimed'" "$COMPETE_OUT"
        elif [ "$POST_COMPETE_STATUS" != "in_progress" ]; then
            fail_case "claim-competing-status-preserved" "in_progress" "$POST_COMPETE_STATUS"
        elif [ "$POST_COMPETE_ASSIGNEE" != "$CLAIM_ASSIGNEE" ]; then
            fail_case "claim-competing-assignee-preserved" "original assignee '$CLAIM_ASSIGNEE' untouched" "'$POST_COMPETE_ASSIGNEE'"
        else
            echo "CLAIM: PASS (atomic claim, idempotent re-claim, competing claim by a different actor fails with 'already claimed')"
        fi
    fi
fi

# --- Case: the assignee seam — does bd 1.0.5 gate `--claim` on the
# assignee, or merely on status=in_progress? The CLAIM case above only
# proves a competing claim fails while the bead is in_progress; it never
# isolates status from assignee, so it cannot tell which one is doing the
# gating. This case seeds a bead into open-but-still-assigned state (claim
# it, then `bd update --status open` WITHOUT clearing the assignee) and
# attempts a competing claim as a different actor, asserting the ACTUALLY
# OBSERVED result rather than an assumed one.
#
# RESULT (executed against the installed bd 1.0.5 both in an ad hoc probe
# and reproduced by this case, 2026-07-30): the competing claim FAILS with
# "already claimed by <assignee>" even though status is open — bd gates
# `--claim` on the assignee, not on status. This confirms the
# assignee-clearing in bin/kranz-run-bead (see RUNBEAD-REOPEN-RECLAIM below)
# is a real regression fix, not mere hygiene: without it, a bead returned to
# open by kranz-run-bead would still refuse every other actor's claim.

SEAM_CREATE_OUT=$(BD create "SEAM fixture (open-but-assigned)" --type task --json 2>&1)
SEAM_ID=$(printf '%s' "$SEAM_CREATE_OUT" | unwrap | jq -r '.id // empty')
if [ -z "$SEAM_ID" ]; then
    fail_case "seam-create" "a non-empty issue id" "'$SEAM_CREATE_OUT'"
else
    BD update "$SEAM_ID" --claim >/dev/null 2>&1
    SEAM_ORIG_ASSIGNEE=$(BD show "$SEAM_ID" --json 2>/dev/null | unwrap | jq -r '.assignee // empty')
    # Reset status only — deliberately does NOT clear the assignee, so the
    # bead is open but still assigned to the original actor.
    BD update "$SEAM_ID" --status open >/dev/null 2>&1
    SEAM_PRECOMPETE_SHOW=$(BD show "$SEAM_ID" --json 2>/dev/null | unwrap)
    SEAM_PRECOMPETE_STATUS=$(printf '%s' "$SEAM_PRECOMPETE_SHOW" | jq -r '.status // empty')
    SEAM_PRECOMPETE_ASSIGNEE=$(printf '%s' "$SEAM_PRECOMPETE_SHOW" | jq -r '.assignee // empty')

    SEAM_COMPETE_OUT=$( (cd "$STORE_DIR" && bd --actor "kranz-roundtrip-seam-competitor" update "$SEAM_ID" --claim --json) 2>&1)
    SEAM_COMPETE_EXIT=$?
    SEAM_POST_SHOW=$(BD show "$SEAM_ID" --json 2>/dev/null | unwrap)
    SEAM_POST_STATUS=$(printf '%s' "$SEAM_POST_SHOW" | jq -r '.status // empty')
    SEAM_POST_ASSIGNEE=$(printf '%s' "$SEAM_POST_SHOW" | jq -r '.assignee // empty')

    if [ "$SEAM_PRECOMPETE_STATUS" != "open" ]; then
        fail_case "seam-precondition-open" "open (bead reset without clearing assignee)" "$SEAM_PRECOMPETE_STATUS"
    elif [ -z "$SEAM_PRECOMPETE_ASSIGNEE" ] || [ "$SEAM_PRECOMPETE_ASSIGNEE" != "$SEAM_ORIG_ASSIGNEE" ]; then
        fail_case "seam-precondition-assigned" "assignee still '$SEAM_ORIG_ASSIGNEE' (status-only reset, assignee untouched)" "'$SEAM_PRECOMPETE_ASSIGNEE'"
    elif [ "$SEAM_COMPETE_EXIT" -eq 0 ]; then
        fail_case "seam-competing-claim-gated-by-assignee" "a different actor's claim on an open-but-assigned bead to FAIL (assignee gates claiming, not status)" "exit 0: $SEAM_COMPETE_OUT"
    elif ! printf '%s' "$SEAM_COMPETE_OUT" | grep -qiE 'already claimed'; then
        fail_case "seam-competing-error-text" "error text matching 'already claimed'" "$SEAM_COMPETE_OUT"
    elif [ "$SEAM_POST_STATUS" != "open" ]; then
        fail_case "seam-post-status-preserved" "open" "$SEAM_POST_STATUS"
    elif [ "$SEAM_POST_ASSIGNEE" != "$SEAM_ORIG_ASSIGNEE" ]; then
        fail_case "seam-post-assignee-preserved" "original assignee '$SEAM_ORIG_ASSIGNEE' untouched" "'$SEAM_POST_ASSIGNEE'"
    else
        echo "SEAM: PASS (bd 1.0.5 gates --claim on the assignee, not status: an open-but-still-assigned bead still refuses a competing claim with 'already claimed')"
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

# --- Case: lease-aware dead-claim recovery (client-side, f-3-1) ---------
# bd 1.0.5 exposes no lease/TTL/heartbeat field (docs/scoping/beads-bridge-
# dialect.md §3, executed probe), so the bridge keeps its own lease files
# (.gc/kranz-leases/<id>.lease: <pid> <unix-ts>), renewed by kranz-run-bead
# and swept by `kranz-dispatch --reclaim`. Liveness-first, mirroring the
# kranz queue's posture: a claim whose recorded pid is ALIVE is never
# stolen, whatever its age; a dead pid releases the claim. The operator
# decision for the client-side design is D-BW-2 (accepted 2026-07-29).

LEASE_CREATE_OUT=$(BD create "LEASE fixture (dead holder)" --type task --json 2>&1)
LEASE_DEAD_ID=$(printf '%s' "$LEASE_CREATE_OUT" | unwrap | jq -r '.id // empty')
LIVE_CREATE_OUT=$(BD create "LEASE fixture (live holder)" --type task --json 2>&1)
LEASE_LIVE_ID=$(printf '%s' "$LIVE_CREATE_OUT" | unwrap | jq -r '.id // empty')
if [ -z "$LEASE_DEAD_ID" ] || [ -z "$LEASE_LIVE_ID" ]; then
    fail_case "lease-create" "two non-empty fixture ids" "'$LEASE_DEAD_ID' / '$LEASE_LIVE_ID'"
else
    LEASE_DIR="$CITY_DIR/.gc/kranz-leases"
    mkdir -p "$LEASE_DIR"
    # ms-3-fix-5-1: the sweep's candidate query is now scoped HARD to
    # $LABEL-carrying beads (defect 1), so these fixtures must actually
    # carry the "kranz" label to be swept at all.
    BD update "$LEASE_DEAD_ID" --add-label kranz >/dev/null 2>&1
    BD update "$LEASE_LIVE_ID" --add-label kranz >/dev/null 2>&1
    BD update "$LEASE_DEAD_ID" --claim >/dev/null 2>&1
    BD update "$LEASE_LIVE_ID" --claim >/dev/null 2>&1
    # Dead holder: a pid that cannot exist, recorded now.
    printf '999999999 %s\n' "$(date +%s)" > "$LEASE_DIR/$LEASE_DEAD_ID.lease"
    # Live holder: this test process itself, freshly renewed.
    printf '%s %s\n' "$$" "$(date +%s)" > "$LEASE_DIR/$LEASE_LIVE_ID.lease"

    # The reclaim runs through the bridge's ambient `gc bd` convention
    # (never `gc --city` — the harness is city-less by design), so give it
    # the established pass-through stub: every bd subcommand forwards to the
    # real store, every invocation is logged.
    LEASE_STUB_BIN="$SANDBOX/lease-stubbin"
    mkdir -p "$LEASE_STUB_BIN"
    cat > "$LEASE_STUB_BIN/gc" <<STUBEOF
#!/bin/sh
set -u
if [ "\${1:-}" = "--city" ]; then
    shift 2
fi
if [ "\${1:-}" = "bd" ]; then
    shift
    ( cd "$STORE_DIR" && bd "\$@" )
    exit \$?
fi
echo "lease-stub-gc: unsupported invocation: gc \$*" >&2
exit 1
STUBEOF
    chmod +x "$LEASE_STUB_BIN/gc"

    PATH="$LEASE_STUB_BIN:$PATH" GC_CITY="$CITY_DIR" KRANZ_LABEL="kranz" \
        KRANZ_LEASE_DIR="$LEASE_DIR" "$BIN_DIR/kranz-dispatch" --reclaim >/dev/null 2>&1

    DEAD_STATUS=$(status_of "$LEASE_DEAD_ID")
    DEAD_ASSIGNEE=$(BD show "$LEASE_DEAD_ID" --json 2>/dev/null | unwrap | jq -r '.assignee // empty')
    LIVE_STATUS=$(status_of "$LEASE_LIVE_ID")
    LIVE_ASSIGNEE=$(BD show "$LEASE_LIVE_ID" --json 2>/dev/null | unwrap | jq -r '.assignee // empty')

    if [ "$DEAD_STATUS" != "open" ] || [ -n "$DEAD_ASSIGNEE" ]; then
        fail_case "lease-dead-recovered" "dead claim released to open/unassigned" "status=$DEAD_STATUS assignee='$DEAD_ASSIGNEE'"
    elif [ "$LIVE_STATUS" != "in_progress" ] || [ -z "$LIVE_ASSIGNEE" ]; then
        fail_case "lease-live-preserved" "live claim preserved at in_progress/assigned" "status=$LIVE_STATUS assignee='$LIVE_ASSIGNEE'"
    else
        # The released bead is immediately claimable again (idempotent re-claim).
        BD update "$LEASE_DEAD_ID" --claim >/dev/null 2>&1
        RECLAIMED=$(status_of "$LEASE_DEAD_ID")
        if [ "$RECLAIMED" != "in_progress" ]; then
            fail_case "lease-reclaimable-after-recovery" "in_progress after re-claim" "$RECLAIMED"
        else
            echo "LEASE: PASS (dead-claim recovered, live-claim preserved)"
        fi
    fi

    # TTL backstop branch (f-3-1): a claim with NO lease file is recovered
    # only when idle past KRANZ_CLAIM_TTL. bd's embedded store can't be
    # backdated, so the branch is driven through the TTL knob itself:
    # huge TTL keeps a lease-less claim; TTL=0 recovers it (idle > 0).
    TTL_CREATE_OUT=$(BD create "LEASE-TTL fixture" --type task --json 2>&1)
    TTL_ID=$(printf '%s' "$TTL_CREATE_OUT" | unwrap | jq -r '.id // empty')
    if [ -z "$TTL_ID" ]; then
        fail_case "lease-ttl-create" "a non-empty fixture id" "'$TTL_CREATE_OUT'"
    else
        BD update "$TTL_ID" --add-label kranz >/dev/null 2>&1
        BD update "$TTL_ID" --claim >/dev/null 2>&1
        rm -f "$LEASE_DIR/$TTL_ID.lease"

        # Huge TTL: the lease-less claim stands (expiry only as backstop).
        PATH="$LEASE_STUB_BIN:$PATH" GC_CITY="$CITY_DIR" KRANZ_LABEL="kranz" \
            KRANZ_LEASE_DIR="$LEASE_DIR" KRANZ_CLAIM_TTL=999999 \
            "$BIN_DIR/kranz-dispatch" --reclaim >/dev/null 2>&1
        TTL_FRESH=$(status_of "$TTL_ID")

        # One full second of wall clock between the claim above and the
        # TTL=0 sweep below: the same sub-second clock-truncation skew fixed
        # for FOREIGN-BEAD/POSITIVE-CONTROL/REACHABILITY in fa35941 (NOW=
        # $(date +%s) vs a floor-truncated updated_at can read NOW - UPDATED
        # == -1, failing `-ge 0` under KRANZ_CLAIM_TTL=0) applies here too —
        # this case drives the identical --claim-then-TTL=0-sweep sequence
        # and had no such guard.
        sleep 1

        # TTL=0: any lease-less claim is stale — recovered, with the
        # bd-updated_at (Z-suffixed ISO) parse path exercised for real.
        PATH="$LEASE_STUB_BIN:$PATH" GC_CITY="$CITY_DIR" KRANZ_LABEL="kranz" \
            KRANZ_LEASE_DIR="$LEASE_DIR" KRANZ_CLAIM_TTL=0 \
            "$BIN_DIR/kranz-dispatch" --reclaim >/dev/null 2>&1
        TTL_STALE=$(status_of "$TTL_ID")

        if [ "$TTL_FRESH" != "in_progress" ]; then
            fail_case "lease-ttl-fresh-kept" "lease-less claim kept under a huge TTL" "$TTL_FRESH"
        elif [ "$TTL_STALE" != "open" ]; then
            fail_case "lease-ttl-stale-recovered" "lease-less claim recovered at TTL=0" "$TTL_STALE"
        else
            echo "LEASE-TTL: PASS (expiry only as backstop: fresh kept, stale recovered)"
        fi
    fi
fi

# --- Case: reclaim sweep scope + reachability (ms-3-fix-5-1) --------------
# Three defects fixed together, proven here:
#   FOREIGN-BEAD  (defect 1, CRITICAL): the sweep must never release or
#     un-assign an in_progress bead that does not carry $LABEL, however
#     stale. This case MUST fail against the pre-fix kranz-dispatch (which
#     swept every in_progress bead in the store with no label filter at
#     all) — see the stash/verify note in the WorkerReport.
#   POSITIVE-CONTROL: a $LABEL-carrying, lease-less, past-TTL in_progress
#     bead run through the SAME sweep invocation IS still released, so
#     FOREIGN-BEAD is not passing merely because the sweep did nothing.
#   REACHABILITY (defect 4, CRITICAL): a bare `kranz-dispatch` — no
#     `--reclaim` argument, the exact shape
#     packaging/gascity/orders/kranz-dispatch.toml runs — performs the
#     sweep before draining, with no rig-routing stub in play so the
#     freshly-released bead is left open by the "no routable rig" comment
#     path rather than being re-claimed in the same run.
#
# A dedicated, unique label keeps this section's candidate query isolated
# from every other fixture created earlier in this script (many of which
# carry no label, and a few of which carry "kranz").
#
# Setup-race note (respawn stabilisation): a naive claim-then-immediately-
# sweep sequence here was flaky. Diagnosed, not guessed — see wait_in_progress
# below: the candidate query (`bd list --status in_progress --limit 0
# ...--json`) is visible immediately after `bd update --claim` returns (40
# rapid create+claim+query round trips in an instrumented sandbox, 0
# misses), so store-side list-visibility is NOT the cause. The real cause is
# sub-second clock skew in the idle-age comparison: comparing a freshly
# captured `date +%s` (NOW, floor-truncated) against the claim's `updated_at`
# (also floor-truncated) can read UPDATED one second AHEAD of NOW purely from
# where each timestamp's fractional second falls relative to the whole-second
# boundary (measured directly: 16/40 rapid claim-then-compare round trips in
# an instrumented sandbox landed NOW - UPDATED == -1, never less). With
# KRANZ_CLAIM_TTL=0 that negative diff fails `-ge 0` and the bead is skipped
# — a flake, not a genuine defect. Fixed below two ways: (a) poll the
# candidate query itself before sweeping, so a real visibility gap would
# still surface as a deterministic, explanatory failure rather than a race;
# (b) let one full second of wall clock elapse between claim and sweep,
# which is more than the measured skew, so truncation cannot manufacture a
# negative idle age.
wait_in_progress() {
    # $1 = case name for fail_case on timeout, $2 = extra bd-list args (may
    # be empty), remaining args = ids that must all appear in the query's
    # in_progress result before returning. Bounded ~20 tries @ 0.25s (~5s).
    WIP_CASE=$1
    WIP_EXTRA=$2
    shift 2
    WIP_TRIES=0
    while [ "$WIP_TRIES" -lt 20 ]; do
        WIP_RAW=$(BD list --status in_progress --limit 0 $WIP_EXTRA --json 2>&1)
        WIP_ALL=1
        for WIP_ID in "$@"; do
            WIP_FOUND=$(printf '%s' "$WIP_RAW" | jq -r --arg id "$WIP_ID" '[.[]? | select(.id == $id)] | length' 2>/dev/null)
            [ "$WIP_FOUND" = "1" ] || WIP_ALL=0
        done
        [ "$WIP_ALL" -eq 1 ] && return 0
        WIP_TRIES=$((WIP_TRIES + 1))
        sleep 0.25
    done
    fail_case "$WIP_CASE" "ids ($*) visible via bd list --status in_progress --limit 0 $WIP_EXTRA --json within ~5s" "$WIP_RAW"
    return 1
}
RECLAIM_TEST_LABEL="kranz-reclaim-scope-test"
RECLAIM_LEASE_DIR="$SANDBOX/reclaim-scope-leases"
mkdir -p "$RECLAIM_LEASE_DIR"

# `gc` stub: forwards every `bd` subcommand (including `list`, which no
# other stub in this script needs to support) straight to the real store,
# same pass-through convention as LEASE_STUB_BIN above. `gc rig list` and
# anything else deliberately fails, so REACHABILITY's ready-drain cannot
# route a rig and therefore cannot re-claim the bead the sweep just
# released.
RECLAIM_STUB_BIN="$SANDBOX/reclaim-scope-stubbin"
mkdir -p "$RECLAIM_STUB_BIN"
cat > "$RECLAIM_STUB_BIN/gc" <<STUBEOF
#!/bin/sh
set -u
if [ "\${1:-}" = "--city" ]; then
    shift 2
fi
if [ "\${1:-}" = "bd" ]; then
    shift
    ( cd "$STORE_DIR" && bd "\$@" )
    exit \$?
fi
echo "reclaim-scope-stub-gc: unsupported invocation: gc \$*" >&2
exit 1
STUBEOF
chmod +x "$RECLAIM_STUB_BIN/gc"

FOREIGN_CREATE_OUT=$(BD create "FOREIGN fixture (no $RECLAIM_TEST_LABEL label)" --type task --json 2>&1)
FOREIGN_ID=$(printf '%s' "$FOREIGN_CREATE_OUT" | unwrap | jq -r '.id // empty')
POSCTRL_CREATE_OUT=$(BD create "POSCTRL fixture ($RECLAIM_TEST_LABEL-labeled)" --type task --json 2>&1)
POSCTRL_ID=$(printf '%s' "$POSCTRL_CREATE_OUT" | unwrap | jq -r '.id // empty')

if [ -z "$FOREIGN_ID" ] || [ -z "$POSCTRL_ID" ]; then
    fail_case "reclaim-scope-create" "two non-empty fixture ids" "'$FOREIGN_ID' / '$POSCTRL_ID'"
else
    BD update "$POSCTRL_ID" --add-label "$RECLAIM_TEST_LABEL" >/dev/null 2>&1
    BD update "$FOREIGN_ID" --claim >/dev/null 2>&1
    BD update "$POSCTRL_ID" --claim >/dev/null 2>&1

    FOREIGN_ORIG_ASSIGNEE=$(BD show "$FOREIGN_ID" --json 2>/dev/null | unwrap | jq -r '.assignee // empty')

    # Store-catchup gate: unfiltered (no --label) so FOREIGN_ID — which the
    # sweep's own label-filtered query must NOT return — is provably visible
    # to the store too. Only once both ids are confirmed in_progress can
    # FOREIGN-BEAD's survival below be attributed to the label filter rather
    # than the sweep simply not having seen it yet.
    wait_in_progress "reclaim-scope-store-catchup" "" "$FOREIGN_ID" "$POSCTRL_ID"
    sleep 1

    # TTL=0 so "past TTL" is trivially true for a lease-less claim (bd's
    # embedded store can't be backdated — same technique as the LEASE-TTL
    # case above). The sleep above (not a retry, not a softened assertion)
    # guarantees a full second of real elapsed time between the claim and
    # this single invocation, so sub-second truncation skew (see the
    # setup-race note above) cannot manufacture a negative idle age.
    GC_CITY="$CITY_DIR" KRANZ_LABEL="$RECLAIM_TEST_LABEL" KRANZ_LEASE_DIR="$RECLAIM_LEASE_DIR" \
        KRANZ_CLAIM_TTL=0 PATH="$RECLAIM_STUB_BIN:$PATH" \
        "$BIN_DIR/kranz-dispatch" --reclaim >/dev/null 2>&1

    FOREIGN_STATUS=$(status_of "$FOREIGN_ID")
    FOREIGN_ASSIGNEE=$(BD show "$FOREIGN_ID" --json 2>/dev/null | unwrap | jq -r '.assignee // empty')
    POSCTRL_STATUS=$(status_of "$POSCTRL_ID")
    POSCTRL_ASSIGNEE=$(BD show "$POSCTRL_ID" --json 2>/dev/null | unwrap | jq -r '.assignee // empty')

    if [ "$FOREIGN_STATUS" != "in_progress" ] || [ "$FOREIGN_ASSIGNEE" != "$FOREIGN_ORIG_ASSIGNEE" ]; then
        fail_case "reclaim-foreign-bead-untouched" "in_progress, assignee unchanged ('$FOREIGN_ORIG_ASSIGNEE')" "status=$FOREIGN_STATUS assignee='$FOREIGN_ASSIGNEE'"
    else
        echo "FOREIGN-BEAD: PASS (sweep never releases an in_progress bead lacking $RECLAIM_TEST_LABEL)"
    fi

    if [ "$POSCTRL_STATUS" != "open" ] || [ -n "$POSCTRL_ASSIGNEE" ]; then
        fail_case "reclaim-positive-control-released" "open with assignee cleared" "status=$POSCTRL_STATUS assignee='$POSCTRL_ASSIGNEE'"
    else
        echo "POSITIVE-CONTROL: PASS ($RECLAIM_TEST_LABEL-carrying lease-less past-TTL claim still released)"
    fi
fi

# --- Case: the client-side label guard, genuinely exercised (ms-3-fix-5-2)
# FOREIGN-BEAD above creates its foreign bead with no labels at all, so `gc
# bd list --label ...` excludes it SERVER-SIDE before the sweep's jq
# `.labels` select() ever sees it — that case would pass identically with
# the select() deleted from kranz-dispatch, proving nothing about the
# client-side guard the kranz-dispatch header documents ("...even if the
# server-side filter is dropped by the `gc bd` pass-through"). This case
# defeats the server-side filter for real via a `gc` stub whose `bd list`
# branch strips --label/--status before forwarding to the real store (same
# pass-through convention as every other stub above), so an unlabelled,
# lease-less, past-TTL in_progress bead genuinely reaches the sweep and only
# the client-side jq guard can save it.
GUARDBYPASS_LEASE_DIR="$SANDBOX/guard-bypass-leases"
mkdir -p "$GUARDBYPASS_LEASE_DIR"

GUARDBYPASS_STUB_BIN="$SANDBOX/guard-bypass-stubbin"
mkdir -p "$GUARDBYPASS_STUB_BIN"
cat > "$GUARDBYPASS_STUB_BIN/gc" <<STUBEOF
#!/bin/sh
set -u
if [ "\${1:-}" = "--city" ]; then
    shift 2
fi
if [ "\${1:-}" = "bd" ]; then
    shift
    if [ "\${1:-}" = "list" ]; then
        shift
        STRIPPED=""
        while [ "\$#" -gt 0 ]; do
            case "\$1" in
                --label|--status)
                    shift 2
                    ;;
                *)
                    STRIPPED="\$STRIPPED \$1"
                    shift
                    ;;
            esac
        done
        ( cd "$STORE_DIR" && bd list \$STRIPPED )
        exit \$?
    fi
    ( cd "$STORE_DIR" && bd "\$@" )
    exit \$?
fi
echo "guard-bypass-stub-gc: unsupported invocation: gc \$*" >&2
exit 1
STUBEOF
chmod +x "$GUARDBYPASS_STUB_BIN/gc"

GUARDBYPASS_CREATE_OUT=$(BD create "GUARD-BYPASS fixture (no label at all)" --type task --json 2>&1)
GUARDBYPASS_ID=$(printf '%s' "$GUARDBYPASS_CREATE_OUT" | unwrap | jq -r '.id // empty')
if [ -z "$GUARDBYPASS_ID" ]; then
    fail_case "guard-bypass-create" "a non-empty issue id" "'$GUARDBYPASS_CREATE_OUT'"
else
    BD update "$GUARDBYPASS_ID" --claim >/dev/null 2>&1
    GUARDBYPASS_ORIG_ASSIGNEE=$(BD show "$GUARDBYPASS_ID" --json 2>/dev/null | unwrap | jq -r '.assignee // empty')

    # Store-catchup gate (unfiltered, same convention as FOREIGN-BEAD above),
    # then the same one-second wall-clock wait so the sweep's idle-age
    # comparison cannot read negative from sub-second truncation skew.
    wait_in_progress "guard-bypass-store-catchup" "" "$GUARDBYPASS_ID"
    sleep 1

    # TTL=0 so "past TTL" is trivially true for the lease-less claim. The
    # label the sweep is asked to drain is one this bead never carries, and
    # the stub strips --label from the forwarded query, so only the
    # client-side jq `.labels` select() stands between this bead and being
    # released.
    GC_CITY="$CITY_DIR" KRANZ_LABEL="$RECLAIM_TEST_LABEL" KRANZ_LEASE_DIR="$GUARDBYPASS_LEASE_DIR" \
        KRANZ_CLAIM_TTL=0 PATH="$GUARDBYPASS_STUB_BIN:$PATH" \
        "$BIN_DIR/kranz-dispatch" --reclaim >/dev/null 2>&1

    GUARDBYPASS_STATUS=$(status_of "$GUARDBYPASS_ID")
    GUARDBYPASS_ASSIGNEE=$(BD show "$GUARDBYPASS_ID" --json 2>/dev/null | unwrap | jq -r '.assignee // empty')

    if [ "$GUARDBYPASS_STATUS" != "in_progress" ] || [ "$GUARDBYPASS_ASSIGNEE" != "$GUARDBYPASS_ORIG_ASSIGNEE" ]; then
        fail_case "guard-bypass-clientside-guard-holds" "in_progress, assignee unchanged ('$GUARDBYPASS_ORIG_ASSIGNEE') even with the server-side --label filter stripped" "status=$GUARDBYPASS_STATUS assignee='$GUARDBYPASS_ASSIGNEE'"
    else
        echo "LABEL-GUARD-BYPASS: PASS (client-side .labels guard holds even when gc bd list drops --label/--status server-side)"
    fi
fi

REACH_CREATE_OUT=$(BD create "REACHABILITY fixture ($RECLAIM_TEST_LABEL-labeled)" --type task --json 2>&1)
REACH_ID=$(printf '%s' "$REACH_CREATE_OUT" | unwrap | jq -r '.id // empty')
if [ -z "$REACH_ID" ]; then
    fail_case "reachability-create" "a non-empty issue id" "'$REACH_CREATE_OUT'"
else
    BD update "$REACH_ID" --add-label "$RECLAIM_TEST_LABEL" >/dev/null 2>&1
    BD update "$REACH_ID" --claim >/dev/null 2>&1

    REACH_CITY_DIR="$SANDBOX/reachability-city"
    REACH_SPOOL_DIR="$REACH_CITY_DIR/.gc/kranz-spool"
    mkdir -p "$REACH_SPOOL_DIR"

    # Store-catchup gate using the SAME query shape the sweep issues
    # (label-filtered), then the same one-second wait as above so the idle-
    # age comparison inside the sweep cannot read negative from sub-second
    # truncation skew.
    wait_in_progress "reachability-store-catchup" "--label $RECLAIM_TEST_LABEL" "$REACH_ID"
    sleep 1

    # No --reclaim argument: this is the exact invocation
    # packaging/gascity/orders/kranz-dispatch.toml runs (`exec =
    # "kranz-dispatch"`, bare, on its 5m cooldown). KRANZ_RIG_DIR is
    # deliberately unset and the stub's `gc rig list` unsupported, so IF the
    # sweep failed to release $REACH_ID here, the only other way it could
    # end up open is via the "no routable rig" comment path leaving it
    # untouched at in_progress — which is not what we assert below.
    GC_CITY="$REACH_CITY_DIR" KRANZ_LABEL="$RECLAIM_TEST_LABEL" \
        KRANZ_SPOOL="$REACH_SPOOL_DIR" KRANZ_LEASE_DIR="$RECLAIM_LEASE_DIR" \
        KRANZ_CLAIM_TTL=0 PATH="$RECLAIM_STUB_BIN:$PATH" "$BIN_DIR/kranz-dispatch" >/dev/null 2>&1

    REACH_STATUS=$(status_of "$REACH_ID")
    REACH_ASSIGNEE=$(BD show "$REACH_ID" --json 2>/dev/null | unwrap | jq -r '.assignee // empty')
    if [ "$REACH_STATUS" != "open" ] || [ -n "$REACH_ASSIGNEE" ]; then
        fail_case "reachability-bare-invocation-sweeps" "open with assignee cleared (bare kranz-dispatch performs the sweep before draining)" "status=$REACH_STATUS assignee='$REACH_ASSIGNEE'"
    else
        echo "REACHABILITY: PASS (bare kranz-dispatch with no --reclaim performs the sweep before draining)"
    fi
fi

# --- Case: sweep non-fatality + --reclaim's sweep-only mode (ms-3-fix-5-1,
# validation criterion 7). Both properties previously rested only on reading
# the source (no `set -e`, `--reclaim` exits right after reclaim_sweep); the
# two cases below make them into live-bd assertions.
#
#   SWEEP-NONFATAL: `gc bd list` (the sweep's candidate query) fails outright.
#     A bare kranz-dispatch must still drain a ready bead afterward. What
#     would break this: if the sweep's failure propagated (e.g. a stray
#     `set -e` interaction, or `reclaim_sweep`'s exit status somehow aborting
#     the calling shell), no spool pair would ever be written and the bead
#     would stay open/unclaimed.
#   RECLAIM-NO-DRAIN: a routable, $LABEL-carrying READY bead is present, but
#     `kranz-dispatch --reclaim` must not touch it — the spool dir stays
#     empty and the bead stays open/unassigned. This is what makes
#     "--reclaim is sweep-only" a tested claim: a --reclaim that fell through
#     to the drain would spool and claim the bead.

NONFATAL_TEST_LABEL="kranz-nonfatal-test"
NONFATAL_CITY_DIR="$SANDBOX/nonfatal-city"
NONFATAL_SPOOL_DIR="$NONFATAL_CITY_DIR/.gc/kranz-spool"
NONFATAL_STUB_BIN="$SANDBOX/nonfatal-stubbin"
mkdir -p "$NONFATAL_SPOOL_DIR" "$NONFATAL_STUB_BIN"

# `gc` stub: `bd list` (the sweep's candidate query) always fails; every
# other bd subcommand forwards to the real store, same pass-through
# convention as the other stubs in this script.
cat > "$NONFATAL_STUB_BIN/gc" <<STUBEOF
#!/bin/sh
set -u
if [ "\${1:-}" = "--city" ]; then
    shift 2
fi
if [ "\${1:-}" = "bd" ]; then
    shift
    if [ "\${1:-}" = "list" ]; then
        echo "nonfatal-stub-gc: simulated bd list failure" >&2
        exit 1
    fi
    ( cd "$STORE_DIR" && bd "\$@" )
    exit \$?
fi
echo "nonfatal-stub-gc: unsupported invocation: gc \$*" >&2
exit 1
STUBEOF
chmod +x "$NONFATAL_STUB_BIN/gc"

NONFATAL_CREATE_OUT=$(BD create "SWEEP-NONFATAL fixture ($NONFATAL_TEST_LABEL-labeled)" --type task --json 2>&1)
NONFATAL_ID=$(printf '%s' "$NONFATAL_CREATE_OUT" | unwrap | jq -r '.id // empty')
if [ -z "$NONFATAL_ID" ]; then
    fail_case "sweep-nonfatal-create" "a non-empty issue id" "'$NONFATAL_CREATE_OUT'"
else
    BD update "$NONFATAL_ID" --add-label "$NONFATAL_TEST_LABEL" >/dev/null 2>&1

    # Bare invocation (no --reclaim): the sweep runs first, `gc bd list`
    # fails inside it, then the ready-drain must still proceed.
    GC_CITY="$NONFATAL_CITY_DIR" KRANZ_RIG_DIR="$RIG_DIR" KRANZ_LABEL="$NONFATAL_TEST_LABEL" \
        KRANZ_SPOOL="$NONFATAL_SPOOL_DIR" KRANZ_ALLOW_UNVALIDATED=1 \
        PATH="$NONFATAL_STUB_BIN:$PATH" "$BIN_DIR/kranz-dispatch" >/dev/null 2>&1

    NONFATAL_STATUS=$(status_of "$NONFATAL_ID")
    NONFATAL_MFILE=$(ls "$NONFATAL_SPOOL_DIR"/*-"$NONFATAL_ID".md 2>/dev/null | head -1)
    NONFATAL_EFILE=$(ls "$NONFATAL_SPOOL_DIR"/*-"$NONFATAL_ID".env 2>/dev/null | head -1)

    if [ "$NONFATAL_STATUS" != "in_progress" ]; then
        fail_case "sweep-nonfatal-drain-status" "in_progress (drain proceeded despite the sweep's bd list failure)" "$NONFATAL_STATUS"
    elif [ -z "$NONFATAL_MFILE" ] || [ -z "$NONFATAL_EFILE" ]; then
        fail_case "sweep-nonfatal-spool-pair" "both .md and .env spool files for $NONFATAL_ID" "md='$NONFATAL_MFILE' env='$NONFATAL_EFILE'"
    else
        echo "SWEEP-NONFATAL: PASS (a failing bd list inside the sweep does not abort the dispatch drain that follows)"
    fi
fi

NODRAIN_TEST_LABEL="kranz-nodrain-test"
NODRAIN_CITY_DIR="$SANDBOX/nodrain-city"
NODRAIN_SPOOL_DIR="$NODRAIN_CITY_DIR/.gc/kranz-spool"
mkdir -p "$NODRAIN_SPOOL_DIR"

NODRAIN_CREATE_OUT=$(BD create "RECLAIM-NO-DRAIN fixture ($NODRAIN_TEST_LABEL-labeled)" --type task --json 2>&1)
NODRAIN_ID=$(printf '%s' "$NODRAIN_CREATE_OUT" | unwrap | jq -r '.id // empty')
if [ -z "$NODRAIN_ID" ]; then
    fail_case "reclaim-no-drain-create" "a non-empty issue id" "'$NODRAIN_CREATE_OUT'"
else
    BD update "$NODRAIN_ID" --add-label "$NODRAIN_TEST_LABEL" >/dev/null 2>&1

    # KRANZ_RIG_DIR points at a real git checkout, so a drain WOULD spool
    # this bead if --reclaim fell through to it. $RECLAIM_STUB_BIN (defined
    # above) forwards every bd subcommand to the real store.
    GC_CITY="$NODRAIN_CITY_DIR" KRANZ_RIG_DIR="$RIG_DIR" KRANZ_LABEL="$NODRAIN_TEST_LABEL" \
        KRANZ_SPOOL="$NODRAIN_SPOOL_DIR" KRANZ_ALLOW_UNVALIDATED=1 \
        PATH="$RECLAIM_STUB_BIN:$PATH" "$BIN_DIR/kranz-dispatch" --reclaim >/dev/null 2>&1

    NODRAIN_STATUS=$(status_of "$NODRAIN_ID")
    NODRAIN_ASSIGNEE=$(BD show "$NODRAIN_ID" --json 2>/dev/null | unwrap | jq -r '.assignee // empty')
    NODRAIN_SPOOLED=$(ls "$NODRAIN_SPOOL_DIR" 2>/dev/null)

    if [ -n "$NODRAIN_SPOOLED" ]; then
        fail_case "reclaim-no-drain-spool-empty" "spool dir to stay empty" "found: $NODRAIN_SPOOLED"
    elif [ "$NODRAIN_STATUS" != "open" ] || [ -n "$NODRAIN_ASSIGNEE" ]; then
        fail_case "reclaim-no-drain-bead-untouched" "open with no assignee (never drained)" "status=$NODRAIN_STATUS assignee='$NODRAIN_ASSIGNEE'"
    else
        echo "RECLAIM-NO-DRAIN: PASS (--reclaim runs the sweep only, never drains a routable ready bead)"
    fi
fi

# --- Case: lease PRODUCER end-to-end (f-3-1) --------------------------------
# The heartbeat producer itself, not only the consumer: a kranz-run-bead
# running a simulated mission must create the lease with its live pid
# (visible mid-run), honor the KRANZ_LEASE_DIR override, and remove the
# file on terminal exit — the complement of the consumer's --reclaim sweep.
PROD_CREATE_OUT=$(BD create "LEASE-PRODUCER fixture" --type task --json 2>&1)
PROD_ID=$(printf '%s' "$PROD_CREATE_OUT" | unwrap | jq -r '.id // empty')
if [ -z "$PROD_ID" ]; then
    fail_case "lease-producer-create" "a non-empty fixture id" "'$PROD_CREATE_OUT'"
else
    PROD_LEASE_DIR="$SANDBOX/prod-leases"
    PROD_BIN="$SANDBOX/prod-bin"
    mkdir -p "$PROD_LEASE_DIR" "$PROD_BIN"
    # A slow kranz stub: a visible mid-run window to observe the live lease.
    cat > "$PROD_BIN/kranz" <<'STUBEOF'
#!/bin/sh
sleep 2
echo "kranz-stub: simulated slow mission run"
exit 0
STUBEOF
    chmod +x "$PROD_BIN/kranz"
    MFILE="$SANDBOX/prod-mission.md"
    printf '## Goal\nproducer fixture\n' > "$MFILE"

    PATH="$PROD_BIN:$LEASE_STUB_BIN:$PATH" KRANZ_LEASE_DIR="$PROD_LEASE_DIR" \
        "$BIN_DIR/kranz-run-bead" "$CITY_DIR" "$PROD_ID" "$MFILE" "$RIG_DIR" 1 >/dev/null 2>&1 &
    PROD_PID=$!
    sleep 1

    LEASE_LIVE_PID=""
    if [ -f "$PROD_LEASE_DIR/$PROD_ID.lease" ]; then
        read -r LPID LTS < "$PROD_LEASE_DIR/$PROD_ID.lease"
        if kill -0 "$LPID" 2>/dev/null; then
            LEASE_LIVE_PID="$LPID"
        fi
    fi
    wait "$PROD_PID" || true

    if [ -z "$LEASE_LIVE_PID" ]; then
        fail_case "lease-producer-live-heartbeat" "lease file with a live pid visible mid-run (KRANZ_LEASE_DIR honored)" "$(ls -la "$PROD_LEASE_DIR" 2>/dev/null)"
    elif [ -e "$PROD_LEASE_DIR/$PROD_ID.lease" ]; then
        fail_case "lease-producer-cleanup-on-exit" "lease file removed on terminal exit" "lease still present: $(cat "$PROD_LEASE_DIR/$PROD_ID.lease" 2>/dev/null)"
    else
        echo "LEASE-PRODUCER: PASS (heartbeat created with live pid mid-run, removed on terminal exit)"
    fi
fi

# --- Case: claim-succeeded-then-show-failed window — a real bead is claimed
# via the atomic `--claim` call inside kranz-dispatch, then `gc bd show
# "$ID" --json` fails. Per the rollback shipped in kranz-dispatch's spool-write failure handling
# (`release_claim`), the bead must be returned to open/unassigned rather
# than left claimed with no spool entry, and no .md/.env pair must exist for
# it. The stub keeps its own call log (same style as the FIELDS stub below)
# so this case can assert POSITIVELY that a claim was actually taken and
# then released, rather than only asserting a final state that a no-op
# claim would also produce. -----------------------------------------------

SHOWFAIL_CREATE_OUT=$(BD create "SHOWFAIL fixture" --type task --json 2>&1)
SHOWFAIL_ID=$(printf '%s' "$SHOWFAIL_CREATE_OUT" | unwrap | jq -r '.id // empty')
if [ -z "$SHOWFAIL_ID" ]; then
    fail_case "showfail-create" "a non-empty issue id" "'$SHOWFAIL_CREATE_OUT'"
else
    SHOWFAIL_CITY_DIR="$SANDBOX/showfail-city"
    SHOWFAIL_SPOOL_DIR="$SHOWFAIL_CITY_DIR/.gc/kranz-spool"
    SHOWFAIL_STUB_BIN="$SANDBOX/showfail-stubbin"
    SHOWFAIL_CALLS_LOG="$SANDBOX/showfail-gc-calls.log"
    mkdir -p "$SHOWFAIL_SPOOL_DIR" "$SHOWFAIL_STUB_BIN"
    : > "$SHOWFAIL_CALLS_LOG"

    # `gc` stub: logs every invocation verbatim (same style as the FIELDS
    # stub below), so the case can assert positively that a claim was
    # actually taken; `ready` offers only the fixture; `update --claim` and
    # the rollback's `update --status open --assignee ""` both forward to
    # the real bd store (so the claim and any rollback are real, live-bd
    # mutations); `show` unconditionally fails, simulating the window this
    # case targets.
    cat > "$SHOWFAIL_STUB_BIN/gc" <<STUBEOF
#!/bin/sh
set -u
echo "gc \$*" >> "$SHOWFAIL_CALLS_LOG"
if [ "\${1:-}" = "--city" ]; then
    shift 2
fi
case "\${1:-}" in
    bd)
        shift
        case "\${1:-}" in
            ready)
                echo '[{"id":"$SHOWFAIL_ID"}]'
                ;;
            show)
                echo "gc-stub: simulated bd show failure" >&2
                exit 1
                ;;
            update|comment)
                ID=\$2
                ( cd "$STORE_DIR" && bd "\$@" )
                ;;
            *)
                echo "gc-stub: unsupported bd subcommand: \$1" >&2
                exit 1
                ;;
        esac
        ;;
    *)
        echo "gc-stub: unsupported invocation: gc \$*" >&2
        exit 1
        ;;
esac
STUBEOF
    chmod +x "$SHOWFAIL_STUB_BIN/gc"

    GC_CITY="$SHOWFAIL_CITY_DIR" KRANZ_RIG_DIR="$RIG_DIR" KRANZ_LABEL="kranz" \
        KRANZ_SPOOL="$SHOWFAIL_SPOOL_DIR" KRANZ_ALLOW_UNVALIDATED=1 \
        PATH="$SHOWFAIL_STUB_BIN:$PATH" "$BIN_DIR/kranz-dispatch" >/dev/null 2>&1

    SHOWFAIL_STATUS=$(status_of "$SHOWFAIL_ID")
    SHOWFAIL_ASSIGNEE=$(BD show "$SHOWFAIL_ID" --json 2>/dev/null | unwrap | jq -r '.assignee // empty')
    SHOWFAIL_MFILE=$(ls "$SHOWFAIL_SPOOL_DIR"/*-"$SHOWFAIL_ID".md 2>/dev/null | head -1)
    SHOWFAIL_EFILE=$(ls "$SHOWFAIL_SPOOL_DIR"/*-"$SHOWFAIL_ID".env 2>/dev/null | head -1)

    if ! grep -qE "update ${SHOWFAIL_ID} --claim\$" "$SHOWFAIL_CALLS_LOG" 2>/dev/null; then
        fail_case "showfail-claim-taken" "gc-calls log to record an atomic claim (update $SHOWFAIL_ID --claim) before the rollback" "$(cat "$SHOWFAIL_CALLS_LOG" 2>/dev/null)"
    elif ! grep -qE "update ${SHOWFAIL_ID} --status open --assignee" "$SHOWFAIL_CALLS_LOG" 2>/dev/null; then
        fail_case "showfail-claim-released" "gc-calls log to record the rollback release (update $SHOWFAIL_ID --status open --assignee) after the show failure" "$(cat "$SHOWFAIL_CALLS_LOG" 2>/dev/null)"
    elif [ "$SHOWFAIL_STATUS" != "open" ]; then
        fail_case "showfail-status-rolled-back" "open" "$SHOWFAIL_STATUS"
    elif [ -n "$SHOWFAIL_ASSIGNEE" ]; then
        fail_case "showfail-assignee-cleared" "assignee cleared (empty) after a gc bd show failure post-claim" "'$SHOWFAIL_ASSIGNEE'"
    elif [ -n "$SHOWFAIL_MFILE" ]; then
        fail_case "showfail-no-orphan-md" "no .md spool file for $SHOWFAIL_ID" "found: $SHOWFAIL_MFILE"
    elif [ -n "$SHOWFAIL_EFILE" ]; then
        fail_case "showfail-no-orphan-env" "no .env spool file for $SHOWFAIL_ID" "found: $SHOWFAIL_EFILE"
    else
        echo "SHOWFAIL: PASS (claim rolled back to open/unassigned and no orphan spool pair when gc bd show fails after a successful claim)"
    fi
fi

# --- Case: the .env-write failure — the ONLY path on which an orphan .md
# can ever exist. SHOWFAIL above forces `gc bd show` to fail BEFORE either
# spool file is opened, so its no-orphan-md/no-orphan-env assertions hold
# vacuously with respect to the `rm -f` rollback (bin/kranz-dispatch's
# second `rm -f "$SPOOL/$STAMP-$ID.md" "$SPOOL/$STAMP-$ID.env"`, in the
# branch where the .md write already succeeded and the .env write then
# fails) — they would still pass with both `rm -f` lines deleted. This case
# drives that branch for real: `date` is stubbed on PATH so the dispatch
# subshell's `STAMP=$(date +%s)` is pinned to a known value (`+%s` returns a
# fixed constant; every other invocation execs the real /bin/date), which
# lets this case pre-create the exact `$SPOOL/<stamp>-<id>.env` path as a
# read-only (chmod 444) placeholder file before dispatch ever runs. Opening
# that path for truncating write then fails with EACCES (verified below:
# the negative-control run shows the placeholder's content is untouched),
# while the .md file at the same stamp is a distinct path the placeholder
# never touches, so the .md write goes through normally. This is a real
# permission failure on this platform (confirmed non-root via `id -u`
# below), not a stand-in for one.

ENVFAIL_UID=$(id -u 2>/dev/null || echo "")
if [ "$ENVFAIL_UID" = "0" ]; then
    echo "ENVFAIL: SKIP (running as root — chmod 444 does not block root's own writes, so the .env-write failure cannot be constructed reliably)"
else
    ENVFAIL_STAMP="1700000000"

    ENVFAIL_CREATE_OUT=$(BD create "ENVFAIL fixture" --type task --json 2>&1)
    ENVFAIL_ID=$(printf '%s' "$ENVFAIL_CREATE_OUT" | unwrap | jq -r '.id // empty')
    if [ -z "$ENVFAIL_ID" ]; then
        fail_case "envfail-create" "a non-empty issue id" "'$ENVFAIL_CREATE_OUT'"
    else
        ENVFAIL_CITY_DIR="$SANDBOX/envfail-city"
        ENVFAIL_SPOOL_DIR="$ENVFAIL_CITY_DIR/.gc/kranz-spool"
        ENVFAIL_STUB_BIN="$SANDBOX/envfail-stubbin"
        ENVFAIL_CALLS_LOG="$SANDBOX/envfail-gc-calls.log"
        mkdir -p "$ENVFAIL_SPOOL_DIR" "$ENVFAIL_STUB_BIN"
        : > "$ENVFAIL_CALLS_LOG"

        # Pre-create the exact .env path dispatch will compute (fixed stamp,
        # known id) as a read-only placeholder, so the redirect inside
        # dispatch cannot open it for writing.
        ENVFAIL_ENV_PATH="$ENVFAIL_SPOOL_DIR/$ENVFAIL_STAMP-$ENVFAIL_ID.env"
        : > "$ENVFAIL_ENV_PATH"
        chmod 444 "$ENVFAIL_ENV_PATH"

        # `date` stub: pins `date +%s` to $ENVFAIL_STAMP; every other
        # invocation (there are none on this path, but stay general) execs
        # the real binary so nothing else on the platform is disturbed.
        cat > "$ENVFAIL_STUB_BIN/date" <<STUBEOF
#!/bin/sh
set -u
if [ "\$#" -eq 1 ] && [ "\$1" = "+%s" ]; then
    echo "$ENVFAIL_STAMP"
else
    exec /bin/date "\$@"
fi
STUBEOF
        chmod +x "$ENVFAIL_STUB_BIN/date"

        # `gc` stub: logs every invocation; `ready` offers only the fixture;
        # `show`/`update`/`comment` all forward to the real bd store, so the
        # claim and the rollback release are real, live-bd mutations.
        cat > "$ENVFAIL_STUB_BIN/gc" <<STUBEOF
#!/bin/sh
set -u
echo "gc \$*" >> "$ENVFAIL_CALLS_LOG"
if [ "\${1:-}" = "--city" ]; then
    shift 2
fi
case "\${1:-}" in
    bd)
        shift
        case "\${1:-}" in
            ready)
                echo '[{"id":"$ENVFAIL_ID"}]'
                ;;
            show|update|comment)
                ( cd "$STORE_DIR" && bd "\$@" )
                ;;
            *)
                echo "gc-stub: unsupported bd subcommand: \$1" >&2
                exit 1
                ;;
        esac
        ;;
    *)
        echo "gc-stub: unsupported invocation: gc \$*" >&2
        exit 1
        ;;
esac
STUBEOF
        chmod +x "$ENVFAIL_STUB_BIN/gc"

        GC_CITY="$ENVFAIL_CITY_DIR" KRANZ_RIG_DIR="$RIG_DIR" KRANZ_LABEL="kranz" \
            KRANZ_SPOOL="$ENVFAIL_SPOOL_DIR" KRANZ_ALLOW_UNVALIDATED=1 \
            PATH="$ENVFAIL_STUB_BIN:$PATH" "$BIN_DIR/kranz-dispatch" >/dev/null 2>&1

        ENVFAIL_STATUS=$(status_of "$ENVFAIL_ID")
        ENVFAIL_ASSIGNEE=$(BD show "$ENVFAIL_ID" --json 2>/dev/null | unwrap | jq -r '.assignee // empty')
        ENVFAIL_MFILE=$(ls "$ENVFAIL_SPOOL_DIR"/*-"$ENVFAIL_ID".md 2>/dev/null | head -1)
        ENVFAIL_EFILE=$(ls "$ENVFAIL_SPOOL_DIR"/*-"$ENVFAIL_ID".env 2>/dev/null | head -1)
        ENVFAIL_CLAIM_LINE=$(grep -nE "update ${ENVFAIL_ID} --claim\$" "$ENVFAIL_CALLS_LOG" 2>/dev/null | head -1 | cut -d: -f1)
        ENVFAIL_RELEASE_LINE=$(grep -nE "update ${ENVFAIL_ID} --status open --assignee" "$ENVFAIL_CALLS_LOG" 2>/dev/null | head -1 | cut -d: -f1)

        if [ -z "$ENVFAIL_CLAIM_LINE" ]; then
            fail_case "envfail-claim-taken" "gc-calls log to record an atomic claim (update $ENVFAIL_ID --claim) before the rollback" "$(cat "$ENVFAIL_CALLS_LOG" 2>/dev/null)"
        elif [ -z "$ENVFAIL_RELEASE_LINE" ]; then
            fail_case "envfail-claim-released" "gc-calls log to record the rollback release (update $ENVFAIL_ID --status open --assignee) after the .env write failure" "$(cat "$ENVFAIL_CALLS_LOG" 2>/dev/null)"
        elif [ "$ENVFAIL_CLAIM_LINE" -ge "$ENVFAIL_RELEASE_LINE" ]; then
            fail_case "envfail-claim-then-release-order" "the claim (line $ENVFAIL_CLAIM_LINE) to precede the release (line $ENVFAIL_RELEASE_LINE) in the call log" "$(cat "$ENVFAIL_CALLS_LOG" 2>/dev/null)"
        elif [ "$ENVFAIL_STATUS" != "open" ]; then
            fail_case "envfail-status-rolled-back" "open" "$ENVFAIL_STATUS"
        elif [ -n "$ENVFAIL_ASSIGNEE" ]; then
            fail_case "envfail-assignee-cleared" "assignee cleared (empty) after a .env spool-write failure post-claim" "'$ENVFAIL_ASSIGNEE'"
        elif [ -n "$ENVFAIL_MFILE" ]; then
            fail_case "envfail-no-orphan-md" "no .md spool file for $ENVFAIL_ID (the .md write succeeded but must be rolled back with the .env)" "found: $ENVFAIL_MFILE"
        elif [ -n "$ENVFAIL_EFILE" ]; then
            fail_case "envfail-no-orphan-env" "no .env spool file for $ENVFAIL_ID" "found: $ENVFAIL_EFILE"
        else
            echo "ENVFAIL: PASS (claim rolled back to open/unassigned and no orphan spool pair when the .env write fails after a successful .md write)"

            # --- Negative control: same scenario, but against a SANDBOX
            # COPY of kranz-dispatch with both `rm -f` rollback lines
            # deleted (never the real bin/kranz-dispatch). If the ENVFAIL
            # case above is a real test of the rollback rather than a
            # vacuous one, removing the rollback must make the orphan .md
            # survive.
            ENVFAIL_NOROLLBACK="$SANDBOX/kranz-dispatch-no-rm-f"
            grep -vF '        rm -f "$SPOOL/$STAMP-$ID.md" "$SPOOL/$STAMP-$ID.env"' \
                "$BIN_DIR/kranz-dispatch" > "$ENVFAIL_NOROLLBACK"
            chmod +x "$ENVFAIL_NOROLLBACK"

            ENVFAIL_NOROLLBACK_RMF_COUNT=$(grep -c 'rm -f' "$ENVFAIL_NOROLLBACK" 2>/dev/null || echo 0)
            ENVFAIL_ORIG_RMF_COUNT=$(grep -c 'rm -f' "$BIN_DIR/kranz-dispatch" 2>/dev/null || echo 0)
            if [ "$ENVFAIL_NOROLLBACK_RMF_COUNT" -ge "$ENVFAIL_ORIG_RMF_COUNT" ]; then
                fail_case "envfail-negctl-setup" "the sandbox copy to carry fewer 'rm -f' lines than the original ($ENVFAIL_ORIG_RMF_COUNT)" "$ENVFAIL_NOROLLBACK_RMF_COUNT still present"
            else
                NEGCTL_CREATE_OUT=$(BD create "ENVFAIL negative-control fixture" --type task --json 2>&1)
                NEGCTL_ID=$(printf '%s' "$NEGCTL_CREATE_OUT" | unwrap | jq -r '.id // empty')
                if [ -z "$NEGCTL_ID" ]; then
                    fail_case "envfail-negctl-create" "a non-empty issue id" "'$NEGCTL_CREATE_OUT'"
                else
                    NEGCTL_CITY_DIR="$SANDBOX/envfail-negctl-city"
                    NEGCTL_SPOOL_DIR="$NEGCTL_CITY_DIR/.gc/kranz-spool"
                    NEGCTL_STUB_BIN="$SANDBOX/envfail-negctl-stubbin"
                    NEGCTL_CALLS_LOG="$SANDBOX/envfail-negctl-gc-calls.log"
                    mkdir -p "$NEGCTL_SPOOL_DIR" "$NEGCTL_STUB_BIN"
                    : > "$NEGCTL_CALLS_LOG"

                    NEGCTL_ENV_PATH="$NEGCTL_SPOOL_DIR/$ENVFAIL_STAMP-$NEGCTL_ID.env"
                    : > "$NEGCTL_ENV_PATH"
                    chmod 444 "$NEGCTL_ENV_PATH"

                    cat > "$NEGCTL_STUB_BIN/date" <<STUBEOF
#!/bin/sh
set -u
if [ "\$#" -eq 1 ] && [ "\$1" = "+%s" ]; then
    echo "$ENVFAIL_STAMP"
else
    exec /bin/date "\$@"
fi
STUBEOF
                    chmod +x "$NEGCTL_STUB_BIN/date"

                    cat > "$NEGCTL_STUB_BIN/gc" <<STUBEOF
#!/bin/sh
set -u
echo "gc \$*" >> "$NEGCTL_CALLS_LOG"
if [ "\${1:-}" = "--city" ]; then
    shift 2
fi
case "\${1:-}" in
    bd)
        shift
        case "\${1:-}" in
            ready)
                echo '[{"id":"$NEGCTL_ID"}]'
                ;;
            show|update|comment)
                ( cd "$STORE_DIR" && bd "\$@" )
                ;;
            *)
                echo "gc-stub: unsupported bd subcommand: \$1" >&2
                exit 1
                ;;
        esac
        ;;
    *)
        echo "gc-stub: unsupported invocation: gc \$*" >&2
        exit 1
        ;;
esac
STUBEOF
                    chmod +x "$NEGCTL_STUB_BIN/gc"

                    GC_CITY="$NEGCTL_CITY_DIR" KRANZ_RIG_DIR="$RIG_DIR" KRANZ_LABEL="kranz" \
                        KRANZ_SPOOL="$NEGCTL_SPOOL_DIR" KRANZ_ALLOW_UNVALIDATED=1 \
                        PATH="$NEGCTL_STUB_BIN:$PATH" "$ENVFAIL_NOROLLBACK" >/dev/null 2>&1

                    NEGCTL_MFILE=$(ls "$NEGCTL_SPOOL_DIR"/*-"$NEGCTL_ID".md 2>/dev/null | head -1)
                    NEGCTL_ENV_CONTENT=$(cat "$NEGCTL_ENV_PATH" 2>/dev/null)

                    if [ -z "$NEGCTL_MFILE" ]; then
                        fail_case "envfail-negative-control" "the orphan .md for $NEGCTL_ID to SURVIVE against the sandbox copy with rm -f removed (proves the ENVFAIL case above actually depends on the rollback, not a vacuous pass)" "no .md found — the case cannot fail even with the rollback deleted"
                    elif [ -n "$NEGCTL_ENV_CONTENT" ]; then
                        fail_case "envfail-negctl-env-write-truly-failed" "the read-only .env placeholder to remain empty (proving the .env write attempt itself failed with EACCES, not merely that content differs)" "'$NEGCTL_ENV_CONTENT'"
                    else
                        echo "ENVFAIL-NEGATIVE-CONTROL: PASS (against a sandbox copy of kranz-dispatch with both rm -f rollback lines removed, the orphan .md for $NEGCTL_ID survives and the read-only .env placeholder stays untouched — confirms ENVFAIL is a real, non-vacuous test of the rollback, and that the .env write genuinely fails rather than silently succeeding)"
                    fi
                fi
            fi
        fi
    fi
fi

# --- Case: the lost race — `gc bd update --claim` itself returns non-zero
# (another dispatcher won the race). Per kranz-dispatch's atomic claim step (`gc bd
# update "$ID" --claim >/dev/null 2>&1 || continue`), this must be a SILENT
# skip: no comment, no spool files, no rollback call (none is needed — no
# claim was ever taken), and the bead untouched at open with an empty
# assignee. Until this case, that behaviour rested entirely on reading the
# `|| continue` in the source. -----------------------------------------------

LOSTRACE_CREATE_OUT=$(BD create "LOSTRACE fixture" --type task --json 2>&1)
LOSTRACE_ID=$(printf '%s' "$LOSTRACE_CREATE_OUT" | unwrap | jq -r '.id // empty')
if [ -z "$LOSTRACE_ID" ]; then
    fail_case "lostrace-create" "a non-empty issue id" "'$LOSTRACE_CREATE_OUT'"
else
    LOSTRACE_CITY_DIR="$SANDBOX/lostrace-city"
    LOSTRACE_SPOOL_DIR="$LOSTRACE_CITY_DIR/.gc/kranz-spool"
    LOSTRACE_STUB_BIN="$SANDBOX/lostrace-stubbin"
    LOSTRACE_CALLS_LOG="$SANDBOX/lostrace-gc-calls.log"
    mkdir -p "$LOSTRACE_SPOOL_DIR" "$LOSTRACE_STUB_BIN"
    : > "$LOSTRACE_CALLS_LOG"

    # `gc` stub: logs every invocation; `ready` offers only the fixture;
    # `update` (the atomic claim) always fails, simulating a lost race. No
    # other bd subcommand should ever be reached from this path.
    cat > "$LOSTRACE_STUB_BIN/gc" <<STUBEOF
#!/bin/sh
set -u
echo "gc \$*" >> "$LOSTRACE_CALLS_LOG"
if [ "\${1:-}" = "--city" ]; then
    shift 2
fi
case "\${1:-}" in
    bd)
        shift
        case "\${1:-}" in
            ready)
                echo '[{"id":"$LOSTRACE_ID"}]'
                ;;
            update)
                echo "gc-stub: simulated lost race (claim fails)" >&2
                exit 1
                ;;
            *)
                echo "gc-stub: unsupported bd subcommand: \$1" >&2
                exit 1
                ;;
        esac
        ;;
    *)
        echo "gc-stub: unsupported invocation: gc \$*" >&2
        exit 1
        ;;
esac
STUBEOF
    chmod +x "$LOSTRACE_STUB_BIN/gc"

    GC_CITY="$LOSTRACE_CITY_DIR" KRANZ_RIG_DIR="$RIG_DIR" KRANZ_LABEL="kranz" \
        KRANZ_SPOOL="$LOSTRACE_SPOOL_DIR" KRANZ_ALLOW_UNVALIDATED=1 \
        PATH="$LOSTRACE_STUB_BIN:$PATH" "$BIN_DIR/kranz-dispatch" >/dev/null 2>&1

    LOSTRACE_STATUS=$(status_of "$LOSTRACE_ID")
    LOSTRACE_ASSIGNEE=$(BD show "$LOSTRACE_ID" --json 2>/dev/null | unwrap | jq -r '.assignee // empty')
    LOSTRACE_MFILE=$(ls "$LOSTRACE_SPOOL_DIR"/*-"$LOSTRACE_ID".md 2>/dev/null | head -1)
    LOSTRACE_EFILE=$(ls "$LOSTRACE_SPOOL_DIR"/*-"$LOSTRACE_ID".env 2>/dev/null | head -1)

    if [ "$LOSTRACE_STATUS" != "open" ]; then
        fail_case "lostrace-status-untouched" "open" "$LOSTRACE_STATUS"
    elif [ -n "$LOSTRACE_ASSIGNEE" ]; then
        fail_case "lostrace-assignee-untouched" "empty assignee (bead never claimed)" "'$LOSTRACE_ASSIGNEE'"
    elif [ -n "$LOSTRACE_MFILE" ]; then
        fail_case "lostrace-no-md" "no .md spool file for $LOSTRACE_ID" "found: $LOSTRACE_MFILE"
    elif [ -n "$LOSTRACE_EFILE" ]; then
        fail_case "lostrace-no-env" "no .env spool file for $LOSTRACE_ID" "found: $LOSTRACE_EFILE"
    elif grep -qE "bd comment ${LOSTRACE_ID}" "$LOSTRACE_CALLS_LOG" 2>/dev/null; then
        fail_case "lostrace-no-comment" "no gc bd comment call for $LOSTRACE_ID (silent skip)" "$(cat "$LOSTRACE_CALLS_LOG")"
    else
        echo "LOSTRACE: PASS (a failing claim itself skips silently: no .md/.env, no comment, bead untouched at open/unassigned)"
    fi
fi

# --- Case: the scrutiny floor — refusal checks run BEFORE any claim, so a
# refused bead is never claimed. Every other dispatch invocation in this
# script sets KRANZ_ALLOW_UNVALIDATED=1, which short-circuits the branch at
# kranz-dispatch's rollback blocks entirely; this case is the only one that
# leaves it unset against a rig whose .kranz/config.json disables scrutiny,
# so it is the only one that can prove the branch actually refuses. --------

SCRUTINY_RIG_DIR="$SANDBOX/scrutiny-rig"
mkdir -p "$SCRUTINY_RIG_DIR/.kranz"
( cd "$SCRUTINY_RIG_DIR" && git init -q && git config user.email "roundtrip@kranz.local" && git config user.name "roundtrip" )
printf '{"skipScrutiny": true}\n' > "$SCRUTINY_RIG_DIR/.kranz/config.json"

SCRUTINY_CREATE_OUT=$(BD create "SCRUTINY fixture" --type task --json 2>&1)
SCRUTINY_ID=$(printf '%s' "$SCRUTINY_CREATE_OUT" | unwrap | jq -r '.id // empty')
if [ -z "$SCRUTINY_ID" ]; then
    fail_case "scrutiny-create" "a non-empty issue id" "'$SCRUTINY_CREATE_OUT'"
else
    SCRUTINY_CITY_DIR="$SANDBOX/scrutiny-city"
    SCRUTINY_SPOOL_DIR="$SCRUTINY_CITY_DIR/.gc/kranz-spool"
    SCRUTINY_STUB_BIN="$SANDBOX/scrutiny-stubbin"
    SCRUTINY_CALLS_LOG="$SANDBOX/scrutiny-gc-calls.log"
    mkdir -p "$SCRUTINY_SPOOL_DIR" "$SCRUTINY_STUB_BIN"
    : > "$SCRUTINY_CALLS_LOG"

    # `gc` stub: logs every invocation; `ready` offers only the fixture;
    # `comment` forwards to the real bd store (so the REFUSED comment is a
    # real, live-bd mutation); `update` (a claim) must never be reached from
    # this path, so it fails loudly if it ever is.
    cat > "$SCRUTINY_STUB_BIN/gc" <<STUBEOF
#!/bin/sh
set -u
echo "gc \$*" >> "$SCRUTINY_CALLS_LOG"
if [ "\${1:-}" = "--city" ]; then
    shift 2
fi
case "\${1:-}" in
    bd)
        shift
        case "\${1:-}" in
            ready)
                echo '[{"id":"$SCRUTINY_ID"}]'
                ;;
            comment)
                ( cd "$STORE_DIR" && bd "\$@" )
                ;;
            update)
                echo "gc-stub: unexpected claim attempt in scrutiny-floor case" >&2
                exit 1
                ;;
            *)
                echo "gc-stub: unsupported bd subcommand: \$1" >&2
                exit 1
                ;;
        esac
        ;;
    *)
        echo "gc-stub: unsupported invocation: gc \$*" >&2
        exit 1
        ;;
esac
STUBEOF
    chmod +x "$SCRUTINY_STUB_BIN/gc"

    GC_CITY="$SCRUTINY_CITY_DIR" KRANZ_RIG_DIR="$SCRUTINY_RIG_DIR" KRANZ_LABEL="kranz" \
        KRANZ_SPOOL="$SCRUTINY_SPOOL_DIR" \
        PATH="$SCRUTINY_STUB_BIN:$PATH" "$BIN_DIR/kranz-dispatch" >/dev/null 2>&1

    SCRUTINY_STATUS=$(status_of "$SCRUTINY_ID")
    SCRUTINY_ASSIGNEE=$(BD show "$SCRUTINY_ID" --json 2>/dev/null | unwrap | jq -r '.assignee // empty')

    if ! grep -qF "REFUSED" "$SCRUTINY_CALLS_LOG" 2>/dev/null; then
        fail_case "scrutiny-refused-comment" "a REFUSED comment logged for $SCRUTINY_ID" "$(cat "$SCRUTINY_CALLS_LOG" 2>/dev/null)"
    elif [ "$SCRUTINY_STATUS" != "open" ]; then
        fail_case "scrutiny-status-untouched" "open" "$SCRUTINY_STATUS"
    elif [ -n "$SCRUTINY_ASSIGNEE" ]; then
        fail_case "scrutiny-assignee-untouched" "empty assignee (bead never claimed)" "'$SCRUTINY_ASSIGNEE'"
    elif grep -qE "update ${SCRUTINY_ID} --claim" "$SCRUTINY_CALLS_LOG" 2>/dev/null; then
        fail_case "scrutiny-no-claim-call" "no gc bd update --claim call for $SCRUTINY_ID in the call log" "$(cat "$SCRUTINY_CALLS_LOG")"
    else
        echo "SCRUTINY: PASS (skipScrutiny rig refused before any claim: REFUSED comment logged, bead untouched at open/unassigned, no --claim call)"
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
# bead). Each fixture is claimed via `--claim` first, mirroring exactly what
# kranz-dispatch does before spooling (dispatch uses `--claim`, which also
# sets an assignee — a bare `--status in_progress` seed would not exercise
# the assignee-release regression this fix covers), so the assertion
# actually proves a transition rather than an already-true tautology.

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
    BD update "$CASE_ID" --claim >/dev/null 2>&1

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

# --- Case: claim released on reopen — a DIFFERENT actor can claim the bead
# after kranz-run-bead reopens it (direct regression test for this fix: a
# bead driven through exit 3 used to stay assigned to the original actor,
# silently starving any dispatcher running as a different actor) -----------

RELEASE_CREATE_OUT=$(BD create "RUNBEAD fixture (release-on-reopen)" --type task --json 2>&1)
RELEASE_ID=$(printf '%s' "$RELEASE_CREATE_OUT" | unwrap | jq -r '.id // empty')
if [ -z "$RELEASE_ID" ]; then
    fail_case "runbead-release-create" "a non-empty issue id" "'$RELEASE_CREATE_OUT'"
else
    BD update "$RELEASE_ID" --claim >/dev/null 2>&1
    RELEASE_MFILE="$SANDBOX/runbead-release-mission.md"
    printf '## Goal\nRUNBEAD fixture (release-on-reopen)\n' > "$RELEASE_MFILE"

    KRANZ_STUB_EXIT=3 PATH="$STUB_BIN:$PATH" \
        "$BIN_DIR/kranz-run-bead" "$CITY_DIR" "$RELEASE_ID" "$RELEASE_MFILE" "$RIG_DIR" 1 >/dev/null 2>&1

    RELEASE_STATUS=$(status_of "$RELEASE_ID")
    RELEASE_ASSIGNEE=$(BD show "$RELEASE_ID" --json 2>/dev/null | unwrap | jq -r '.assignee // empty')
    OTHER_CLAIM_OUT=$( (cd "$STORE_DIR" && bd --actor "kranz-roundtrip-different-actor" update "$RELEASE_ID" --claim --json) 2>&1)
    OTHER_CLAIM_EXIT=$?
    OTHER_CLAIM_ASSIGNEE=$(printf '%s' "$OTHER_CLAIM_OUT" | unwrap | jq -r '.assignee // empty')

    if [ "$RELEASE_STATUS" != "open" ]; then
        fail_case "runbead-release-status" "open" "$RELEASE_STATUS"
    elif [ -n "$RELEASE_ASSIGNEE" ]; then
        fail_case "runbead-release-assignee-cleared" "assignee cleared (empty) after exit 3" "'$RELEASE_ASSIGNEE'"
    elif [ "$OTHER_CLAIM_EXIT" -ne 0 ]; then
        fail_case "runbead-release-different-actor-claims" "a different actor's claim to succeed after release" "exit $OTHER_CLAIM_EXIT: $OTHER_CLAIM_OUT"
    elif [ "$OTHER_CLAIM_ASSIGNEE" != "kranz-roundtrip-different-actor" ]; then
        fail_case "runbead-release-different-actor-assignee" "assignee 'kranz-roundtrip-different-actor'" "'$OTHER_CLAIM_ASSIGNEE'"
    else
        echo "RUNBEAD-REOPEN-RECLAIM: PASS (exit 3 clears the assignee; a different actor can then claim the bead)"
    fi
fi

# --- Verdict --------------------------------------------------------------

if [ "$FAILED" -ne 0 ]; then
    echo "ROUNDTRIP: FAIL" >&2
    exit 1
fi

BD_VERSION=$(bd --version 2>&1 | sed 's/^bd version //')
echo "ROUNDTRIP: PASS (live bd $BD_VERSION)"
exit 0
