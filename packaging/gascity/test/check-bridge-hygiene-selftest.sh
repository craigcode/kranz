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
#
# It also regression-tests a second sub-finding of [a1]: check-bridge-
# hygiene.sh used to carry a self-exemption in is_comment_or_readme_hit for
# its own file path, because its detection code spelled out the literal
# search patterns/messages it was scanning for. That exemption was narrowed
# three times (ms-2-fix-1-2, ms-2-fix-2-2, ms-2-fix-2-7) and a validator
# found a residual evasion hole every time. ms-2-fix-3-1 removed the root
# cause instead of narrowing further: the checker now assembles the command
# name and verbs from split tokens at runtime (same idiom this selftest
# uses below), so its own source never carries the literals, and the
# self-exemption was deleted outright — there is no special case left to
# evade. This file proves that behaviourally, by injecting a runtime-
# assembled `gc init`/`gc stop` onto the exact line shapes the old
# exemption used to swallow (a grep-assignment line, a diagnostic echo
# line, and a bare statement elsewhere in the body), rather than pinning
# any literal copy of the checker's implementation text — a frozen source
# literal inside a merge gate is the defect this milestone is removing.
set -u

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
HYGIENE="$SCRIPT_DIR/check-bridge-hygiene.sh"
REAL_GASCITY_DIR=$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)
REAL_BIN_DIR="$REAL_GASCITY_DIR/bin"

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
# tests (this file is under packaging/gascity/ and, like the checker
# itself now, carries no self-exemption).
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
rm -f "$SANDBOX/bin/probe.sh"

# No-self-exemption (ms-2-fix-3-1): with the self-exemption deleted, a
# runtime-assembled `gc init`/`gc stop` injected directly into the
# checker's own body must still be caught on every line shape the old
# exemption used to swallow — including the two shapes the old exemption
# matched BY DESIGN (a grep-assignment line, a diagnostic echo line), not
# just the shapes prior narrowings happened to miss. Injected as a trailing
# shell comment on an existing line (so the sandbox copy stays syntactically
# valid and the probe is never actually executed as a real `gc` invocation),
# or as a new, never-called function body for the "elsewhere" case.
run_injected_line_case() {
    # $1 = case name, $2 = mode ("grep-line" | "diag-line" | "bare"),
    # $3 = verb token
    name="$1"
    mode="$2"
    verb="$3"
    TARGET="$SANDBOX/test/check-bridge-hygiene.sh"

    cp "$HYGIENE" "$TARGET"
    chmod +x "$TARGET"

    case "$mode" in
        grep-line)
            N=$(grep -Fn '=$(grep ' "$TARGET" | head -n1 | cut -d: -f1)
            [ -n "$N" ] || fail "$name" "a grep-assignment line to exist in $TARGET" "none found"
            sed "${N}s|\$| # ${GC_CMD} ${verb}|" "$TARGET" >"$TARGET.tmp" && mv "$TARGET.tmp" "$TARGET" && chmod +x "$TARGET"
            ;;
        diag-line)
            N=$(grep -Fn 'echo "check-bridge-hygiene:' "$TARGET" | head -n1 | cut -d: -f1)
            [ -n "$N" ] || fail "$name" "a check-bridge-hygiene: diagnostic echo line to exist in $TARGET" "none found"
            sed "${N}s|\$| # ${GC_CMD} ${verb}|" "$TARGET" >"$TARGET.tmp" && mv "$TARGET.tmp" "$TARGET" && chmod +x "$TARGET"
            ;;
        bare)
            printf '\nfoo() { %s %s; }\n' "$GC_CMD" "$verb" >>"$TARGET"
            ;;
        *)
            fail "$name" "a known injection mode" "$mode"
            ;;
    esac

    rm -f "$SANDBOX/bin/probe.sh"

    OUT=$("$TARGET" 2>&1)
    STATUS=$?

    if [ "$STATUS" -eq 0 ]; then
        fail "$name" "check-bridge-hygiene to reject a $mode invocation in its own body" \
            "exit 0 (OK) — invocation was not caught: $OUT"
    fi
    case "$OUT" in
        *"forbidden - see docs/gascity.md:48"*) : ;;
        *) fail "$name" "violation message naming the forbidden verb" "$OUT" ;;
    esac
    echo "SELFTEST [$name]: PASS ($mode invocation in checker's own body still caught)"
}

run_injected_line_case "no-self-exempt-grep-line-init" "grep-line" "$VERB_A"
run_injected_line_case "no-self-exempt-grep-line-stop" "grep-line" "$VERB_B"
run_injected_line_case "no-self-exempt-diag-line-init" "diag-line" "$VERB_A"
run_injected_line_case "no-self-exempt-diag-line-stop" "diag-line" "$VERB_B"
run_injected_line_case "no-self-exempt-bare-init" "bare" "$VERB_A"
run_injected_line_case "no-self-exempt-bare-stop" "bare" "$VERB_B"

# Sanity: the unmodified checker (whose only "gc ... init/stop"-shaped text
# lives in its own runtime-assembled grep patterns and echo diagnostics —
# never spelled out literally, so no exemption is needed) must still pass
# against itself.
cp "$HYGIENE" "$SANDBOX/test/check-bridge-hygiene.sh"
chmod +x "$SANDBOX/test/check-bridge-hygiene.sh"
rm -f "$SANDBOX/bin/probe.sh"
OUT=$("$SANDBOX/test/check-bridge-hygiene.sh" 2>&1)
STATUS=$?
[ "$STATUS" -eq 0 ] || fail "self-unmodified-clean" \
    "check-bridge-hygiene to pass when run unmodified against a copy of itself" \
    "exit $STATUS: $OUT"
echo "SELFTEST [self-unmodified-clean]: PASS"

# set-state positive-direction case: the FIRST half of [a1] (the set-state
# guard in default_mode) previously had no test proving it actually fires
# — only a clean tree was ever scanned, so the guard passed vacuously.
# Assemble the token from split parts at runtime, exactly like gc/init/stop
# above, so this file's own source never carries it adjacently either.
SET_STATE_A=set
SET_STATE_B=state
SET_STATE_TOKEN="${SET_STATE_A}-${SET_STATE_B}"

cp "$HYGIENE" "$SANDBOX/test/check-bridge-hygiene.sh"
chmod +x "$SANDBOX/test/check-bridge-hygiene.sh"
printf '#!/bin/sh\n%s foo bar\n' "$SET_STATE_TOKEN" >"$SANDBOX/bin/probe.sh"

OUT=$("$SANDBOX/test/check-bridge-hygiene.sh" 2>&1)
STATUS=$?
if [ "$STATUS" -eq 0 ]; then
    fail "set-state-guard" "check-bridge-hygiene to reject a '${SET_STATE_TOKEN}' probe under bin/ with nonzero exit" \
        "exit 0 (OK) — evaded the guard: $OUT"
fi
case "$OUT" in
    *"'${SET_STATE_TOKEN}' found under packaging/gascity/bin/"*) : ;;
    *) fail "set-state-guard" "violation message naming the forbidden '${SET_STATE_TOKEN}' token" "$OUT" ;;
esac
echo "SELFTEST [set-state-guard]: PASS (set-state token under bin/ still caught)"
rm -f "$SANDBOX/bin/probe.sh"

# --status-map negative case: contract assertion [a4] previously had no test
# proving the check actually fires — only trees that already satisfy it
# were ever scanned. Copy the real translator scripts into the sandbox
# bin/, strip their bidirectional (<-> / <-->) mapping comment lines, and
# confirm --status-map rejects the result and names what's missing.
for NAME in kranz-dispatch kranz-run-bead; do
    SRC="$REAL_BIN_DIR/$NAME"
    [ -f "$SRC" ] || fail "status-map-missing-mapping" "$SRC to exist" "not found"
    grep -vE '<-{1,2}>' "$SRC" >"$SANDBOX/bin/$NAME"
    chmod +x "$SANDBOX/bin/$NAME"
done

OUT=$("$SANDBOX/test/check-bridge-hygiene.sh" --status-map 2>&1)
STATUS=$?
if [ "$STATUS" -eq 0 ]; then
    fail "status-map-missing-mapping" \
        "check-bridge-hygiene --status-map to reject scripts stripped of their mapping lines" \
        "exit 0 (OK) — evaded the check: $OUT"
fi
case "$OUT" in
    *"is missing:"*) : ;;
    *) fail "status-map-missing-mapping" "a message naming what's missing" "$OUT" ;;
esac
echo "SELFTEST [status-map-missing-mapping]: PASS (--status-map rejects scripts stripped of mapping lines)"

# Restore the sandbox bin/ to a clean state so it doesn't leak into any
# case that might be added after this point.
rm -f "$SANDBOX/bin/kranz-dispatch" "$SANDBOX/bin/kranz-run-bead" "$SANDBOX/bin/probe.sh"

echo "CHECK-BRIDGE-HYGIENE-SELFTEST: PASS (all [a1] evasion idioms caught, no false positive)"
