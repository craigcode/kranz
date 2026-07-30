#!/usr/bin/env bash
# Regression test for finding [a4]: bidirectional status map documented in
# the translator script headers.
#
# edd2df7 removed a false claim-mechanism assertion from kranz-run-bead's
# header ("asserts a claim mechanism that doesn't exist ... there is no
# lease/claim logic in the bridge"), and ms-2-fix-1-3 (5a6904a) carried that
# same correction to bin/kranz-dispatch:5. But packaging/gascity/README.md
# still advertised the same false claim at line 13 ("CLAIMS ready
# `kranz`-labeled beads"), alongside line 21's already-corrected wording —
# docs/scoping/beads-bridge-dialect.md:464-466 is explicit that neither
# script uses `bd update --claim` or `bd ready --claim`; both mutate status
# directly. This test proves none of the three prose sites (kranz-dispatch
# header, kranz-run-bead header, README.md) claims an atomic claim
# mechanism that the code doesn't implement.
#
# Usage: check-a4-claim-wording.sh
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "$0")" && pwd)"
GASCITY_DIR="$(cd -- "$SCRIPT_DIR/.." && pwd)"

fail() {
  echo "FAIL [$1]: expected $2, observed $3" >&2
  exit 1
}

DISPATCH="$GASCITY_DIR/bin/kranz-dispatch"
RUN_BEAD="$GASCITY_DIR/bin/kranz-run-bead"
README="$GASCITY_DIR/README.md"

for f in "$DISPATCH" "$RUN_BEAD" "$README"; do
  [ -f "$f" ] || fail "file-exists" "$f to exist" "not found"
done

# Phrases that assert an atomic claim mechanism the bridge does not
# implement (bd update --claim / bd ready --claim). Case-insensitive.
BAD_PATTERNS=(
  '\bclaiming each\b'
  '\bCLAIMS ready\b'
  '`Bead claimed`'
  'Bead claimed \('
)

for f in "$DISPATCH" "$RUN_BEAD" "$README"; do
  for pat in "${BAD_PATTERNS[@]}"; do
    if grep -qiE "$pat" "$f"; then
      fail "no-false-claim-wording" \
        "$f to not assert an atomic claim mechanism (pattern: $pat)" \
        "$(grep -inE "$pat" "$f")"
    fi
  done
done

# Positive check: the sites that DO discuss the in_progress transition must
# be qualified as a direct status write, not an atomic claim.
if ! grep -qiE 'in_progress|marking each' "$DISPATCH"; then
  fail "dispatch-mentions-transition" \
    "$DISPATCH header to describe the in_progress transition" "not found"
fi
if ! grep -qiE 'not an atomic claim' "$README"; then
  fail "readme-qualifies-claim" \
    "$README to qualify the in_progress write as not an atomic claim" \
    "not found"
fi

echo "CHECK-A4-CLAIM-WORDING: PASS (no script/README prose asserts an atomic claim mechanism)"
