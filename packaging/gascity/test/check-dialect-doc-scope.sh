#!/usr/bin/env bash
# Regression test for finding a5: docs/scoping/beads-bridge-dialect.md must state,
# at the top of its §2 verified-shapes table, that the table covers upstream `bd`
# only and NOT the `gc bd` pass-through every bridge script actually calls. This
# doc has previously had that disclosure silently deleted (commit 14a49ab) and
# restored (commit 497e685) without a correction note, so this check guards
# against a repeat regression.
set -euo pipefail

DOC="$(dirname "$0")/../../../docs/scoping/beads-bridge-dialect.md"

fail() {
  echo "FAIL: $1" >&2
  exit 1
}

[ -f "$DOC" ] || fail "$DOC not found"

grep -q "verified against \*\*upstream \`bd\` 1.0.5 invoked" "$DOC" \
  || fail "§2 no longer states the table is verified against upstream bd only"

grep -q "gc bd\` pass-through's fidelity to the shapes below is" "$DOC" \
  || fail "§2 no longer flags the gc bd pass-through as unverified"

grep -q "gc bd: not in a city directory" "$DOC" \
  || fail "§4 no longer records the executed gc bd --help failure"

echo "DIALECT-DOC-SCOPE: PASS (gc bd pass-through explicitly flagged as unverified)"
