#!/bin/sh
# check-bridge-hygiene-selftest.sh — regression test for finding [a1]:
# check-bridge-hygiene.sh's default-mode `gc init`/`gc stop` scan used to
# grep for the literal adjacent string "gc init"/"gc stop", so the repo's
# own invocation idiom — `gc --city "$CITY" <verb>` (see
# bin/kranz-run-bead:34) — evaded it entirely, along with `gc -C dir stop`
# and `gc  init` (two spaces). Fixed in ms-2-fix-1-2 by matching the verb as
# a whole word with intervening flag/value tokens allowed. This test proves
# each evasion idiom is still caught (and that a genuinely unrelated line is
# still left alone), independent of the current state of packaging/gascity/.
set -u

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
HYGIENE="$SCRIPT_DIR/check-bridge-hygiene.sh"

fail() {
    echo "FAIL [$1]: expected $2, observed $3" >&2
    exit 1
}

[ -f "$HYGIENE" ] || fail "hygiene-script-exists" "$HYGIENE to exist" "not found"

SANDBOX=$(mktemp -d)
trap 'rm -rf "$SANDBOX"' EXIT

mkdir -p "$SANDBOX/test" "$SANDBOX/bin"
cp "$HYGIENE" "$SANDBOX/test/check-bridge-hygiene.sh"
chmod +x "$SANDBOX/test/check-bridge-hygiene.sh"

# Probe text is assembled from separate tokens at runtime, never written
# adjacently as "gc ... init"/"gc ... stop" literals in this file's own
# source — otherwise this selftest would itself trip the very guard it
# tests (this file is under packaging/gascity/ and, unlike
# check-bridge-hygiene.sh, carries no self-exemption).
GC_CMD=g
GC_CMD="${GC_CMD}c"
VERB_A=in
VERB_A="${VERB_A}it"
VERB_B=st
VERB_B="${VERB_B}op"

run_case() {
    # $1 = case name, $2 = probe line to write into a bin/ script
    name="$1"
    probe="$2"

    printf '#!/bin/sh\n%s\n' "$probe" >"$SANDBOX/bin/probe.sh"

    OUT=$("$SANDBOX/test/check-bridge-hygiene.sh" 2>&1)
    STATUS=$?

    if [ "$STATUS" -eq 0 ]; then
        fail "$name" "check-bridge-hygiene to reject probe with nonzero exit" \
            "exit 0 (OK) — evaded the guard: $OUT"
    fi
    case "$OUT" in
        *"forbidden - see docs/gascity.md:48"*) : ;;
        *) fail "$name" "violation message naming the forbidden verb" "$OUT" ;;
    esac
    echo "SELFTEST [$name]: PASS (caught evasion idiom)"
}

# Evasion idioms named in finding [a1] — all must still be caught.
run_case "flagged-init" "foo() { $GC_CMD --city \"\$CITY\" $VERB_A; }"
run_case "flag-stop" "bar() { $GC_CMD -C dir $VERB_B; }"
run_case "double-space-init" "baz() { $GC_CMD  $VERB_A; }"
run_case "flagged-stop" "qux() { $GC_CMD --city \"\$CITY\" $VERB_B; }"

# Sanity: an unrelated line must NOT be flagged (guards against a
# regex broad enough to false-positive on ordinary bridge code).
printf '#!/bin/sh\nbd() { %s --city "$CITY" bd "$@"; }\n' "$GC_CMD" >"$SANDBOX/bin/probe.sh"
OUT=$("$SANDBOX/test/check-bridge-hygiene.sh" 2>&1)
STATUS=$?
[ "$STATUS" -eq 0 ] || fail "no-false-positive" \
    "check-bridge-hygiene to pass on an ordinary 'gc ... bd' invocation" \
    "exit $STATUS: $OUT"
echo "SELFTEST [no-false-positive]: PASS"

echo "CHECK-BRIDGE-HYGIENE-SELFTEST: PASS (all [a1] evasion idioms caught, no false positive)"
