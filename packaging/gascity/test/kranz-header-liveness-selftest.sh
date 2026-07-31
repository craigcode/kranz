#!/bin/sh
# kranz-header-liveness-selftest.sh — regression test for finding [f-3-1]:
# both bridge translator headers (kranz-dispatch and kranz-run-bead) must
# document the liveness-first posture for lease-aware claim recovery and the
# residual spooled-but-not-drained exposure window, in addition to their
# existing status-map and exit-code-contract blocks. This is pure
# documentation content — this test greps the shipped header comments for
# the required content. It is registered in .kranz/merge-gates.json (scoped
# to packaging/gascity/bin/kranz-dispatch and
# packaging/gascity/bin/kranz-run-bead), so a regression here is caught by
# the merge gate, not just by manual re-run.
#
# Updated for ms-3-fix-1-6 (finding [a3]): liveness-first dead-claim
# recovery is now actually implemented (staleness-based, built on bd's
# `updated_at` field plus a heartbeat loop in kranz-run-bead — bd 1.0.5 has
# no dedicated lease/TTL/heartbeat field, docs/scoping/beads-bridge-
# dialect.md §3). The checks below were updated in lockstep: they now assert
# the mechanism is documented as implemented (staleness threshold + recovery
# language), not that it is flagged "not yet implemented" — and that the
# residual pre-heartbeat window is documented as heartbeat-less but still
# covered by the staleness backstop, not as covered by nothing.
#
# Matching is deliberately tolerant of prose rewording (case-insensitive,
# space-or-hyphen between words, substance checks instead of frozen
# phrases) — see ms-2-fix-4-1, which exists because an earlier selftest
# pinned an exact label literal and that brittleness had to be waived. To
# guard against the opposite failure mode (a check so loose it passes on
# any header at all), each check is also run against a copy of the header
# with the corresponding documentation word-stripped out, and must FAIL
# there.
set -u

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
BIN_DIR=$(CDPATH= cd -- "$SCRIPT_DIR/../bin" && pwd)

fail() {
    echo "FAIL [$1]: expected $2, observed $3" >&2
    exit 1
}

extract_header() {
    # Only the leading comment block, before the first non-comment,
    # non-blank line (i.e. before the script body starts). Collapsed to a
    # single space-joined line so phrases that wrap across `# ...` comment
    # lines still match as a contiguous substring.
    awk '/^#!/ { next } /^#/ { sub(/^#[[:space:]]?/, ""); print; next } /^[[:space:]]*$/ { next } { exit }' "$1" \
        | tr '\n' ' ' | tr -s ' '
}

check_liveness_first() {
    printf '%s' "$1" | grep -qiE 'liveness[ -]first'
}

check_liveness_implemented() {
    # The mechanism must be documented as shipped (a staleness/threshold
    # based recovery), not merely aspirational.
    HDR=$1
    printf '%s' "$HDR" | grep -qiE 'stale' && printf '%s' "$HDR" | grep -qiE 'recover'
}

check_residual_named() {
    printf '%s' "$1" | grep -qiE 'residual[^.]*(exposure|risk)'
}

check_ttl_heartbeat_substance() {
    # The spooled-but-not-drained (pre-heartbeat) window must be documented
    # as lacking a heartbeat, while still being covered by the
    # staleness/threshold backstop (not "covered by nothing"). Accept either
    # clause order and reworded negations (no/not/without/lacks).
    HDR=$1
    printf '%s' "$HDR" | grep -qiE '(no|not|without|lacks?)[^.]*heartbeat|heartbeat[^.]*(no|not|without|lacks?)' \
        && printf '%s' "$HDR" | grep -qiE 'stale'
}

# Remove all case-insensitive occurrences of a word from a header string,
# so a stripped fixture can't accidentally still match via leftover text
# on a line that also carries unrelated required content. Header strings
# are single already-joined lines (see extract_header), so word-level
# removal here does not risk merging text across a deleted line the way
# raw-line stripping would.
strip_word() {
    printf '%s' "$1" | perl -pe "s/\Q$2\E/ /gi"
}

for NAME in kranz-dispatch kranz-run-bead; do
    FILE="$BIN_DIR/$NAME"
    [ -f "$FILE" ] || fail "$NAME-exists" "$FILE to exist" "not found"

    HEADER=$(extract_header "$FILE")

    check_liveness_first "$HEADER" || fail "$NAME-liveness-first" \
        "header to document a liveness-first posture (a live claim holder is never stolen)" \
        "no liveness-first phrase in header"

    check_liveness_implemented "$HEADER" || fail "$NAME-liveness-implemented" \
        "header to document the liveness-first mechanism as implemented (a staleness-threshold based dead-claim recovery)" \
        "no stale+recover wording in header"

    check_residual_named "$HEADER" || fail "$NAME-residual-exposure" \
        "header to name the residual spooled-but-not-drained exposure explicitly" \
        "no 'residual ... exposure/risk' wording in header"

    check_ttl_heartbeat_substance "$HEADER" || fail "$NAME-residual-ttl-only" \
        "header to state the spooled-but-not-drained window has no heartbeat and no TTL backstop" \
        "missing heartbeat/TTL negation wording in header"

    echo "SELFTEST [$NAME]: PASS (header documents liveness-first posture and residual TTL-only exposure)"

    # --- Negative control: each check must still FAIL when the
    # corresponding documentation is genuinely absent, so a loosened
    # matcher can't quietly become vacuous. -----------------------------
    STRIPPED_HEADER=$(strip_word "$HEADER" 'liveness')
    if check_liveness_first "$STRIPPED_HEADER"; then
        fail "$NAME-negctrl-liveness-first" \
            "liveness-first check to fail once 'liveness' wording is stripped" \
            "check still passed"
    fi

    STRIPPED_HEADER=$(strip_word "$HEADER" 'stale')
    STRIPPED_HEADER=$(strip_word "$STRIPPED_HEADER" 'staleness')
    if check_liveness_implemented "$STRIPPED_HEADER"; then
        fail "$NAME-negctrl-liveness-implemented" \
            "liveness-implemented check to fail once staleness wording is stripped" \
            "check still passed"
    fi

    STRIPPED_HEADER=$(strip_word "$HEADER" 'residual')
    if check_residual_named "$STRIPPED_HEADER"; then
        fail "$NAME-negctrl-residual-exposure" \
            "residual-exposure check to fail once 'residual' wording is stripped" \
            "check still passed"
    fi

    STRIPPED_HEADER=$(strip_word "$HEADER" 'heartbeat')
    STRIPPED_HEADER=$(strip_word "$STRIPPED_HEADER" 'stale')
    STRIPPED_HEADER=$(strip_word "$STRIPPED_HEADER" 'staleness')
    if check_ttl_heartbeat_substance "$STRIPPED_HEADER"; then
        fail "$NAME-negctrl-residual-ttl-only" \
            "heartbeat/staleness substance check to fail once that wording is stripped" \
            "check still passed"
    fi

    echo "SELFTEST [$NAME]: PASS (negative control confirms each check fails when its documentation is absent)"
done

echo "KRANZ-HEADER-LIVENESS-SELFTEST: PASS ([f-3-1] no longer reproduces, checks are reword-tolerant and non-vacuous)"
