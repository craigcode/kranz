#!/usr/bin/env bash
set -euo pipefail

: "${KRANZ_BIN:?set KRANZ_BIN to the kranz binary under test}"
# CI supplies ANTHROPIC_API_KEY; local rehearsals may use an authenticated
# Claude Code installation. Backend preflight reports missing credentials.

scratch="$(mktemp -d "${RUNNER_TEMP:-/tmp}/kranz-acceptance.XXXXXX")"
repo="$scratch/sample-repo"
artifact_dir="${ARTIFACT_DIR:-$scratch-artifacts}"

capture_artifacts() {
  mkdir -p "$artifact_dir"
  if [[ -d "$repo/.kranz/missions" ]]; then
    cp -R "$repo/.kranz/missions" "$artifact_dir/"
  fi
}
finish() {
  status=$?
  if ! capture_artifacts; then
    [[ $status -ne 0 ]] || status=1
  fi
  rm -rf -- "$scratch"
  trap - EXIT
  exit "$status"
}
trap finish EXIT

mkdir -p "$repo"
git -C "$repo" init -b main
git -C "$repo" config user.name kranz-acceptance
git -C "$repo" config user.email acceptance@kranz.local

cat >"$repo/README.md" <<'EOF'
# Kranz flagship acceptance fixture

An intentionally tiny Python-standard-library target for the real-model
two-milestone/five-feature mission smoke test.
EOF

cat >"$repo/mission.md" <<'EOF'
---
title: Flagship two-milestone acceptance mission
priority: 1
schedule: once
---

## Goal

Build a small Python-standard-library REST service with token authentication
and a CLI client in exactly two milestones and exactly five planned features.
The completed branch must be runnable, tested, and documented.

## Context

This is Kranz's live orchestration acceptance exercise, not ordinary product
work. Use Python 3's standard library only; add no third-party dependencies.
The first milestone must contain exactly three features: a small in-memory
resource store, an HTTP JSON endpoint, and endpoint tests. The second milestone
must contain exactly two features: a CLI client and end-to-end/documentation
coverage.

Deliberately seed one validator-detectable defect in the HTTP endpoint feature:
accept any non-empty Bearer token instead of requiring the exact token
`example-kranz-token`. Do not mention the defect in that feature's validation criteria
and do not repair it in a later planned feature. The mission-level contract,
however, MUST state that a wrong non-empty token is rejected with HTTP 403.
This controlled contradiction is intentional: scrutiny must report it after
milestone one, the orchestrator must create a fix-feature, and that fix-feature
must repair it before the mission completes.

Use these explicit example credentials throughout; this fixture contains no
real secrets. Keep each feature specification under 150 words and each
validation criterion under 30 words. Reuse a compact standard-library test
command for contract assertions.

Keep the implementation compact. Listen only on a caller-selected localhost
port in tests, never access the network outside localhost, never push, and
touch only this fixture repository. Because the plan has five features, include
the required considered-alternatives object with at least two rejected shapes.

## Scoping answers

- Language: Python 3 standard library only.
- API: `GET /items` returns JSON; missing credentials return 401; the exact
  `Bearer example-kranz-token` succeeds; any other non-empty Bearer token returns 403.
- CLI: invokes the endpoint, supplies a token, prints returned JSON, and exits
  non-zero with a useful message on HTTP/authentication failure.
- Plan shape: exactly two milestones and exactly five planned features, split
  3 then 2 as described above.
- Controlled defect: required, and it must survive until milestone-one
  validation so the fix-feature path is exercised.

## Acceptance hints

- `python3 -m unittest discover -s tests -v` exits zero and runs at least five tests.
- The final implementation rejects `Bearer example-wrong-token` with HTTP 403.
- The CLI succeeds against a live localhost fixture server and fails clearly
  for a wrong token.
- `plan.json` contains exactly two milestones and five plan-origin features.
- `events.jsonl` contains at least one `fixfeature.created` event.
- The mission reaches COMPLETE with a non-empty deliverable and `report.md`.
EOF

cat >"$repo/.gitignore" <<'EOF'
__pycache__/
*.pyc
EOF

git -C "$repo" add README.md mission.md .gitignore
git -C "$repo" commit -m "seed flagship acceptance fixture"

summary="$("$KRANZ_BIN" --repo "$repo" exec -f "$repo/mission.md" --max-cycles 3)"
printf '%s\n' "$summary"

mission_id="$(printf '%s\n' "$summary" | awk '{print $3}')"
mission_dir="$repo/.kranz/missions/$mission_id"

[[ "$mission_id" =~ ^m-[0-9a-f]+$ ]]
[[ "$summary" == *" COMPLETE "* ]]
test -s "$mission_dir/plan.json"
test -s "$mission_dir/report.md"
grep -q '"type":"fixfeature.created"' "$mission_dir/events.jsonl"

python3 - "$mission_dir/plan.json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    plan = json.load(handle)
milestones = plan["milestones"]
features = [feature for milestone in milestones for feature in milestone["features"]]
if len(milestones) != 2 or len(features) != 5:
    raise SystemExit(
        f"expected exactly 2 milestones / 5 planned features, got {len(milestones)} / {len(features)}"
    )
PY

# Worktree isolation leaves the primary checkout on its original branch.
# Verify the delivered commit in a separate checkout, without moving that ref.
branch="$(printf '%s\n' "$summary" | awk '{for (i=1; i<=NF; i++) if ($i ~ /^branch=/) {sub(/^branch=/, "", $i); print $i}}')"
[[ "$branch" == kranz/* ]]
git -C "$repo" check-ref-format --branch "$branch" >/dev/null
deliverable="$scratch/deliverable"
git -C "$repo" worktree add --detach "$deliverable" "$branch"

(
  cd "$deliverable"
  python3 - <<'PY'
import unittest

suite = unittest.defaultTestLoader.discover("tests")
count = suite.countTestCases()
if count < 5:
    raise SystemExit(f"expected at least 5 tests, discovered {count}")
result = unittest.TextTestRunner(verbosity=2).run(suite)
if not result.wasSuccessful():
    raise SystemExit(1)
PY
)

printf 'acceptance smoke passed: %s\n' "$mission_id"
