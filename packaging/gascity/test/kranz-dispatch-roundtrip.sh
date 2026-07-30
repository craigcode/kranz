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
# docs/scoping/beads-bridge-dialect.md), which the sibling feature in this
# milestone has not landed yet at the time this script is committed — the
# FIELDS case is expected to fail until packaging/gascity/bin/kranz-dispatch
# is fixed to translate acceptance_criteria type-correctly.
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
    fi
}

if [ -n "$FIXTURE_ID" ]; then
    assert_status "in_progress" "open->in_progress"
    assert_status "blocked"     "in_progress->blocked"
    assert_status "in_progress" "blocked->in_progress"
    assert_status "open"        "in_progress->open"
    assert_status "blocked"     "open->blocked"
    assert_status "open"        "blocked->open"
    echo "STATUS: PASS (open/in_progress/blocked round-tripped both directions via bd update --status)"
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

# --- Case: lease-aware claim / liveness-first recovery ------------------
# Stubbed per this feature's spec: bd 1.0.5 exposes no lease/TTL/heartbeat
# field (docs/scoping/beads-bridge-dialect.md §3, executed probe). The next
# milestone fills this in once an operator-approved mechanism exists.

echo "LEASE: SKIP (not yet implemented)"

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

cat > "$STUB_BIN/gc" <<STUBEOF
#!/bin/sh
set -u
if [ "\${1:-}" != "bd" ]; then
    echo "gc-stub: unsupported invocation: gc \$*" >&2
    exit 1
fi
shift
case "\${1:-}" in
    ready)
        echo '[{"id":"$STRING_ID"},{"id":"$ARRAY_ID"}]'
        ;;
    show)
        ID=\$2
        if [ "\$ID" = "$STRING_ID" ]; then
            ( cd "$STORE_DIR" && bd show "\$ID" --json )
        elif [ "\$ID" = "$ARRAY_ID" ]; then
            printf '[{"id":"%s","title":"Array case","description":"desc","acceptance_criteria":["First hint","Second hint"]}]' "\$ID"
        else
            echo "gc-stub: unknown id \$ID" >&2
            exit 1
        fi
        ;;
    update|comment)
        exit 0
        ;;
    *)
        echo "gc-stub: unsupported bd subcommand: \$1" >&2
        exit 1
        ;;
esac
STUBEOF
chmod +x "$STUB_BIN/gc"

DISPATCH_LOG=$(GC_CITY="$CITY_DIR" KRANZ_RIG_DIR="$RIG_DIR" KRANZ_LABEL="kranz" \
    KRANZ_SPOOL="$SPOOL_DIR" KRANZ_ALLOW_UNVALIDATED=1 \
    PATH="$STUB_BIN:$PATH" "$BIN_DIR/kranz-dispatch" 2>&1)
DISPATCH_STATUS=$?

STRING_BRIEF=$(ls "$SPOOL_DIR"/*-"$STRING_ID".md 2>/dev/null | head -1)
ARRAY_BRIEF=$(ls "$SPOOL_DIR"/*-"$ARRAY_ID".md 2>/dev/null | head -1)

if [ $DISPATCH_STATUS -ne 0 ] || [ -z "$STRING_BRIEF" ] || [ -z "$ARRAY_BRIEF" ]; then
    fail_case "fields-dispatch-ran" "kranz-dispatch to spool a brief for both fixture ids" "exit=$DISPATCH_STATUS log: $DISPATCH_LOG"
else
    STRING_HINTS=$(awk '/^## Acceptance hints$/{f=1;next}/^## /{f=0}f' "$STRING_BRIEF")
    ARRAY_HINTS=$(awk '/^## Acceptance hints$/{f=1;next}/^## /{f=0}f' "$ARRAY_BRIEF")

    if ! printf '%s' "$STRING_HINTS" | grep -qF "$STRING_HINT_TEXT"; then
        fail_case "fields-string-verbatim" "string acceptance_criteria carried verbatim" "$STRING_HINTS"
    elif printf '%s' "$STRING_HINTS" | grep -qE '[][]|"'; then
        fail_case "fields-string-no-raw-json" "no raw JSON bracket/quote text in the string case" "$STRING_HINTS"
    else
        echo "FIELDS-STRING: PASS"
    fi

    if ! printf '%s' "$ARRAY_HINTS" | grep -qF "First hint"; then
        fail_case "fields-array-first" "'First hint' present" "$ARRAY_HINTS"
    elif ! printf '%s' "$ARRAY_HINTS" | grep -qF "Second hint"; then
        fail_case "fields-array-second" "'Second hint' present" "$ARRAY_HINTS"
    elif printf '%s' "$ARRAY_HINTS" | grep -qE '[][]|"'; then
        fail_case "fields-array-no-raw-json" "no raw JSON bracket/quote text in the array case (newline-joined entries expected)" "$ARRAY_HINTS"
    else
        echo "FIELDS-ARRAY: PASS"
    fi

    if [ "$FAILED" -eq 0 ]; then
        echo "FIELDS: PASS (acceptance_criteria string+array)"
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
