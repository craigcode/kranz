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
# It also regression-tests a second sub-finding of [a1] (ms-2-fix-1-11):
# check-bridge-hygiene.sh used to exempt its OWN file wholesale, by path,
# from the gc init/stop scan — so a bare `gc init`/`gc stop` slipped into
# the checker itself, anywhere, would never be reported. The exemption is
# now narrowed to only the lines that embed the literal grep patterns/echo
# diagnostics (see is_comment_or_readme_hit in check-bridge-hygiene.sh); a
# bare invocation elsewhere in the checker must still be caught.
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

# Self-exemption narrowing (ms-2-fix-1-11): a bare gc init/stop invocation
# added directly to check-bridge-hygiene.sh's own body — not one of its
# pattern-bearing grep/echo lines — must still be caught, not swallowed by
# the file's self-exemption.
run_self_exemption_case() {
    # $1 = case name, $2 = verb token to append as a bare invocation
    name="$1"
    verb="$2"

    cp "$HYGIENE" "$SANDBOX/test/check-bridge-hygiene.sh"
    chmod +x "$SANDBOX/test/check-bridge-hygiene.sh"
    printf '\nfoo() { %s %s; }\n' "$GC_CMD" "$verb" >>"$SANDBOX/test/check-bridge-hygiene.sh"

    # Clear out any leftover bin/ probe from earlier cases so this case's
    # result is attributable only to the self-injected line.
    rm -f "$SANDBOX/bin/probe.sh"

    OUT=$("$SANDBOX/test/check-bridge-hygiene.sh" 2>&1)
    STATUS=$?

    if [ "$STATUS" -eq 0 ]; then
        fail "$name" "check-bridge-hygiene to reject a bare invocation in its own body" \
            "exit 0 (OK) — swallowed by the self-exemption: $OUT"
    fi
    case "$OUT" in
        *"forbidden - see docs/gascity.md:48"*) : ;;
        *) fail "$name" "violation message naming the forbidden verb" "$OUT" ;;
    esac
    echo "SELFTEST [$name]: PASS (bare invocation in checker's own body still caught)"
}

run_self_exemption_case "self-exempt-init" "$VERB_A"
run_self_exemption_case "self-exempt-stop" "$VERB_B"

# Self-exemption anchoring (ms-2-fix-2-2): the exemption used to be a bare
# substring test (`*grep*` / `echo `-prefix), which is broader than the
# actual pattern-bearing lines it needs to cover. Prove the two idioms named
# in the finding still get caught now that the match is anchored to the
# real grep-assignment / echo-diagnostic shapes:
#   1. a bare invocation whose line carries a trailing comment mentioning
#      the word "grep" (the old `*grep*` test matched this anywhere on the
#      line, so a trailing `# grep ...` comment used to swallow the hit).
#   2. a helper function whose NAME contains "grep" but whose body invokes
#      the bare verb (the old `*grep*` test matched the function name too).
run_self_exemption_trailing_comment_case() {
    # $1 = case name, $2 = verb token to append as a bare invocation with a
    # trailing comment mentioning "grep"
    name="$1"
    verb="$2"

    cp "$HYGIENE" "$SANDBOX/test/check-bridge-hygiene.sh"
    chmod +x "$SANDBOX/test/check-bridge-hygiene.sh"
    printf '\nfoo() { %s %s; } # grep note\n' "$GC_CMD" "$verb" >>"$SANDBOX/test/check-bridge-hygiene.sh"

    rm -f "$SANDBOX/bin/probe.sh"

    OUT=$("$SANDBOX/test/check-bridge-hygiene.sh" 2>&1)
    STATUS=$?

    if [ "$STATUS" -eq 0 ]; then
        fail "$name" "check-bridge-hygiene to reject a bare invocation with a trailing 'grep'-mentioning comment" \
            "exit 0 (OK) — swallowed by the self-exemption: $OUT"
    fi
    case "$OUT" in
        *"forbidden - see docs/gascity.md:48"*) : ;;
        *) fail "$name" "violation message naming the forbidden verb" "$OUT" ;;
    esac
    echo "SELFTEST [$name]: PASS (trailing-comment evasion idiom still caught)"
}

run_self_exemption_helper_name_case() {
    # $1 = case name, $2 = verb token to append inside a helper function
    # whose name contains "grep"
    name="$1"
    verb="$2"

    cp "$HYGIENE" "$SANDBOX/test/check-bridge-hygiene.sh"
    chmod +x "$SANDBOX/test/check-bridge-hygiene.sh"
    printf '\nmy_grep_helper() { %s %s; }\n' "$GC_CMD" "$verb" >>"$SANDBOX/test/check-bridge-hygiene.sh"

    rm -f "$SANDBOX/bin/probe.sh"

    OUT=$("$SANDBOX/test/check-bridge-hygiene.sh" 2>&1)
    STATUS=$?

    if [ "$STATUS" -eq 0 ]; then
        fail "$name" "check-bridge-hygiene to reject a bare invocation inside a grep-named helper function" \
            "exit 0 (OK) — swallowed by the self-exemption: $OUT"
    fi
    case "$OUT" in
        *"forbidden - see docs/gascity.md:48"*) : ;;
        *) fail "$name" "violation message naming the forbidden verb" "$OUT" ;;
    esac
    echo "SELFTEST [$name]: PASS (grep-named-helper evasion idiom still caught)"
}

run_self_exemption_trailing_comment_case "self-exempt-trailing-comment-init" "$VERB_A"
run_self_exemption_trailing_comment_case "self-exempt-trailing-comment-stop" "$VERB_B"
run_self_exemption_helper_name_case "self-exempt-grep-named-helper-init" "$VERB_A"
run_self_exemption_helper_name_case "self-exempt-grep-named-helper-stop" "$VERB_B"

# Non-vacuity: restore the OLD broad substring exemption (`*grep*` /
# `echo `-prefix) on a temp copy and confirm these two new cases FAIL with
# the swallowed-by-self-exemption diagnostic — proving the new cases would
# have caught the regression the old exemption was vulnerable to. The old
# and new exemption lines are passed to perl via the environment (not
# interpolated into the perl source) so none of their shell/regex
# metacharacters need hand-escaping; \Q...\E makes the search literal.
export OLD_EXEMPTION_LINE='                    *grep*|echo\ *) return 0 ;;'
export NEW_EXEMPTION_LINE='                    *'"'"'=$(grep '"'"'*|'"'"'echo "check-bridge-hygiene:'"'"'*) return 0 ;;'

if ! grep -qF "$NEW_EXEMPTION_LINE" "$HYGIENE"; then
    fail "non-vacuity-setup" "the new anchored exemption line to be present in $HYGIENE" \
        "marker not found — cannot construct the broad-exemption regression copy"
fi

# Remove the $SANDBOX/test/check-bridge-hygiene.sh copy used by the cases
# above (it was left holding an injected probe line from the last case, and
# even an unmodified copy would confuse the scan below: BROAD's own
# self-exemption only applies to ITS OWN path, so a second, unrelated copy
# of check-bridge-hygiene.sh elsewhere under the same GASCITY_DIR would get
# its legitimate echo diagnostics flagged as if they were a real violation).
rm -f "$SANDBOX/test/check-bridge-hygiene.sh"

# The regression copy keeps the SAME basename (check-bridge-hygiene.sh) as
# the real checker, in a sibling directory — the self-exemption case in
# is_comment_or_readme_hit matches on "$SCRIPT_DIR/check-bridge-hygiene.sh",
# i.e. by basename relative to wherever the running script lives, so a
# renamed copy would never hit that case at all and the test would prove
# nothing about the exemption logic itself.
mkdir -p "$SANDBOX/broad"
BROAD="$SANDBOX/broad/check-bridge-hygiene.sh"
perl -pe 's/\Q$ENV{NEW_EXEMPTION_LINE}\E/$ENV{OLD_EXEMPTION_LINE}/' "$HYGIENE" >"$BROAD"
chmod +x "$BROAD"

if ! grep -qF "$OLD_EXEMPTION_LINE" "$BROAD"; then
    fail "non-vacuity-setup" "the broad-exemption regression copy to contain the old substring exemption" \
        "substitution did not take effect"
fi
if grep -qF "$NEW_EXEMPTION_LINE" "$BROAD"; then
    fail "non-vacuity-setup" "the broad-exemption regression copy to no longer contain the new anchored exemption" \
        "substitution left the new line in place"
fi

printf '\nfoo() { %s %s; } # grep note\n' "$GC_CMD" "$VERB_A" >>"$BROAD"
rm -f "$SANDBOX/bin/probe.sh"
OUT=$("$BROAD" 2>&1)
STATUS=$?
if [ "$STATUS" -eq 0 ]; then
    :
else
    fail "non-vacuity-trailing-comment" \
        "the OLD broad exemption to swallow the trailing-comment evasion (exit 0)" \
        "exit $STATUS: $OUT"
fi
echo "SELFTEST [non-vacuity-trailing-comment]: PASS (old broad exemption is indeed swallowed by it, confirming the new test is non-vacuous)"

perl -pe 's/\Q$ENV{NEW_EXEMPTION_LINE}\E/$ENV{OLD_EXEMPTION_LINE}/' "$HYGIENE" >"$BROAD"
chmod +x "$BROAD"
printf '\nmy_grep_helper() { %s %s; }\n' "$GC_CMD" "$VERB_B" >>"$BROAD"
rm -f "$SANDBOX/bin/probe.sh"
OUT=$("$BROAD" 2>&1)
STATUS=$?
if [ "$STATUS" -eq 0 ]; then
    :
else
    fail "non-vacuity-grep-named-helper" \
        "the OLD broad exemption to swallow the grep-named-helper evasion (exit 0)" \
        "exit $STATUS: $OUT"
fi
echo "SELFTEST [non-vacuity-grep-named-helper]: PASS (old broad exemption is indeed swallowed by it, confirming the new test is non-vacuous)"

# Discard the broad-exemption regression copy entirely — it must not linger
# under $SANDBOX and be picked up by the final unmodified-checker sanity
# scan below (that scan walks the whole GASCITY_DIR, i.e. all of $SANDBOX).
rm -rf "$SANDBOX/broad"

# Sanity: the unmodified checker (whose only "gc ... init/stop"-shaped text
# lives in its own grep patterns and echo diagnostics) must still pass
# against itself — proves the narrowed exemption doesn't over-exempt back
# to whole-file, but also doesn't under-exempt and start flagging its own
# detection code.
cp "$HYGIENE" "$SANDBOX/test/check-bridge-hygiene.sh"
chmod +x "$SANDBOX/test/check-bridge-hygiene.sh"
rm -f "$SANDBOX/bin/probe.sh"
OUT=$("$SANDBOX/test/check-bridge-hygiene.sh" 2>&1)
STATUS=$?
[ "$STATUS" -eq 0 ] || fail "self-unmodified-clean" \
    "check-bridge-hygiene to pass when run unmodified against a copy of itself" \
    "exit $STATUS: $OUT"
echo "SELFTEST [self-unmodified-clean]: PASS"

echo "CHECK-BRIDGE-HYGIENE-SELFTEST: PASS (all [a1] evasion idioms caught, no false positive)"
