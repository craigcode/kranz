#!/usr/bin/env bash
set -euo pipefail

: "${KRANZ_BIN:?set KRANZ_BIN to the kranz binary under test}"
: "${KRANZ_ACCEPTANCE_GATE_BIN:?build kranz-engine --example acceptance_gate and set KRANZ_ACCEPTANCE_GATE_BIN}"
test -x "$KRANZ_ACCEPTANCE_GATE_BIN"
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
  if [[ $status -eq 0 ]]; then
    rm -rf -- "$scratch"
  else
    printf 'acceptance fixture retained for inspection/resume: %s\n' "$repo" >&2
  fi
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

Use the pre-agreed interfaces: `service.store.list_items()` returns a non-empty
list of dictionaries; `service.server.make_server(port=0)` returns an HTTPServer
bound to 127.0.0.1; `python3 -m service.client --url URL --token VALUE` calls that
complete endpoint URL and prints its JSON response. README.md must document
the Python invocation, `/items`, and `--token`.
Use the existing `service.auth.accepts_bearer(bearer_value)` helper for
credential verification. Every planned feature must preserve service/auth.py
byte-for-byte relative to that feature's starting checkout; include that
constraint in its validation criteria. A validator-created repair feature may
change the helper. Milestone two starts AFTER that repair and must retain the
repaired helper. Never require auth.py to equal the mission's pinned base in
milestone two, or revert the repair. Check a planned feature's own diff for
helper changes, not the cumulative mission diff. In particular, do not use
`git diff $KRANZ_BASE_SHA -- service/auth.py` as a feature criterion: the
external acceptance audit independently verifies repair provenance.
All feature criteria must remain true after the authorized repair. Do not
encode the temporary defect as a permanent test or criterion (such as requiring
permissive authentication or forbidding wrong-token regression tests). Before
the repair, cover the store, missing credentials, and valid credentials; the
repair adds wrong-token coverage.

The immutable `acceptance_contract.py` is the pre-agreed executable contract.
Use exactly one mission contract assertion, with command
`python3 acceptance_contract.py`: the service has at least five passing unit
tests, missing credentials return 401, a wrong non-empty token returns 403,
and the exact example token returns the store's items. This assertion must
fail on the untouched base and on the seeded authentication defect. The CLI
and documentation belong to milestone two's feature criteria; the external
acceptance runner additionally verifies them with `--final` after COMPLETE.
Do not add commands requiring milestone-two files to the milestone-one gate.
Name parsed request credential variables `bearer_value`: the conservative
secret heuristic treats assignments to generic credential names as potential
literal secrets, even when the right-hand side is a runtime expression.
Every planned feature and repair specification must repeat these fixture
constraints: use `bearer_value` for parsed credentials and use only
`example-kranz-token` / `example-wrong-token` as literal credential values.
Additional invalid examples, if needed, must start with `example-`.
Set touchSet to `service/**`, `tests/**`, and `README.md`. Do not edit the
contract script, mission input, .gitignore, or secret allowlist.

The base already contains a controlled legacy defect: accepts_bearer accepts
any non-empty value. The three milestone-one planned features must preserve
that defect; do not fix it preemptively or work around it in the endpoint.
The two milestone-two planned features must preserve the REPAIRED helper.
The immutable
contract independently tests the helper as well as the actual HTTP endpoint.
After milestone-one validation reports the defect, the orchestrator must
create a fix-feature that repairs the helper and adds its regression coverage.
The final audit verifies that the helper's first change belongs to a completed
fix-feature, not a planned feature.

Use these explicit example credentials throughout; this fixture contains no
real secrets. Keep each feature specification under 150 words and each
validation criterion under 30 words. Reuse a compact standard-library test
command specified above for the contract assertion.

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
- Controlled defect: the existing authentication helper must survive unchanged
  until milestone-one validation so the fix-feature path is exercised.

## Acceptance hints

- `python3 -m unittest discover -s tests -v` exits zero and runs at least five tests.
- The final implementation rejects `Bearer example-wrong-token` with HTTP 403.
- The CLI succeeds against a live localhost fixture server and fails clearly
  for a wrong token.
- `plan.json` contains exactly two milestones and five plan-origin features.
- `events.jsonl` contains at least one `fixfeature.created` event.
- The mission reaches COMPLETE with a non-empty deliverable and `report.md`.
EOF

# Kept in the immutable base and excluded from the plan's touch set. The same
# HTTP probe must fail before the repair and pass afterward; final acceptance
# adds CLI checks only once its milestone has actually run.
cat >"$repo/acceptance_contract.py" <<'PY_CONTRACT'
import json
import pathlib
import subprocess
import sys
import threading
import unittest
import urllib.error
import urllib.request


def main():
    from service.auth import accepts_bearer

    assert not accepts_bearer("example-wrong-token"), "the seeded authentication helper must reject a wrong credential"
    assert accepts_bearer("example-kranz-token"), "the authentication helper must accept the valid example credential"
    from service.server import make_server
    from service.store import list_items

    suite = unittest.defaultTestLoader.discover("tests")
    assert suite.countTestCases() >= 5, "at least five unit tests are required"
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    assert result.wasSuccessful(), "unit tests failed"
    assert not result.skipped and not result.expectedFailures, "tests must run and pass"

    server = make_server(port=0)
    assert server.server_address[0] == "127.0.0.1", "bind only to localhost"
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    url = "http://127.0.0.1:{}/items".format(server.server_address[1])

    def request(value):
        headers = {} if value is None else {"Authorization": "Bearer " + value}
        try:
            with urllib.request.urlopen(urllib.request.Request(url, headers=headers), timeout=5) as response:
                return response.status, json.load(response)
        except urllib.error.HTTPError as error:
            return error.code, None

    try:
        assert request(None)[0] == 401, "missing credentials must return 401"
        assert request("example-wrong-token")[0] == 403, "wrong credentials must return 403"
        status, body = request("example-kranz-token")
        assert status == 200 and body == {"items": list_items()}, "authenticated response must contain the store's items"
        assert body["items"] and all(isinstance(item, dict) for item in body["items"])
        if "--final" in sys.argv:
            command = [sys.executable, "-m", "service.client", "--url", url, "--token"]
            good = subprocess.run(command + ["example-kranz-token"], capture_output=True, text=True, timeout=10)
            assert good.returncode == 0 and json.loads(good.stdout) == body, good.stderr
            bad = subprocess.run(command + ["example-wrong-token"], capture_output=True, text=True, timeout=10)
            assert bad.returncode != 0, "the CLI must reject a wrong credential"
            assert any(word in (bad.stdout + bad.stderr).lower() for word in ("403", "forbidden", "auth")), "the CLI must explain the authentication failure"
            readme = pathlib.Path("README.md").read_text().lower()
            assert all(word in readme for word in ("python", "/items", "--token")), "document the CLI invocation"
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)


if __name__ == "__main__":
    main()
PY_CONTRACT

mkdir -p "$repo/service"
touch "$repo/service/__init__.py"
cat >"$repo/service/auth.py" <<'PY_AUTH'
"""Existing authentication policy; planned features preserve this module."""


def accepts_bearer(bearer_value):
    return bool(bearer_value)
PY_AUTH

mkdir -p "$repo/.kranz"
cat >"$repo/.kranz/secret-allowlist" <<'EOF'
# Reviewed false positives observed during the release rehearsals. Each
# fingerprint waives only its exact runtime expression or synthetic value.
62bbd92fdcee8c5fb97a7061
6536daa68e3c17e1c8379f9d
# Reviewed runtime expression: auth_header.partition(...).
b1f79d2459c595bd2c724888
# Reviewed synthetic negative-test value: some-other-wrong-token.
67179b9130867b23549262cb
# Reviewed Python parameter default referencing the synthetic VALID_TOKEN.
768d6ae6a32900be95cb671b
EOF

cat >"$repo/.gitignore" <<'EOF'
__pycache__/
*.pyc
EOF

git -C "$repo" add README.md mission.md .gitignore acceptance_contract.py .kranz/secret-allowlist service
git -C "$repo" commit -m "seed flagship acceptance fixture"
base_sha="$(git -C "$repo" rev-parse HEAD)"

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
git -C "$repo" diff --exit-code "$base_sha" "$branch" -- acceptance_contract.py mission.md .gitignore .kranz/secret-allowlist

python3 - "$repo" "$base_sha" "$branch" "$mission_dir/state.json" <<'PY_REPAIR'
import json
import subprocess
import sys

repo, base, branch, state_file = sys.argv[1:]
with open(state_file, encoding="utf-8") as handle:
    state = json.load(handle)
fix_commits = {
    commit.split(maxsplit=1)[0]
    for milestone in state["mission"]["milestones"]
    for feature in milestone["features"]
    if feature["origin"] == "fix" and feature["status"] == "complete"
    for commit in feature["commits"]
}
changes = subprocess.check_output(
    ["git", "-C", repo, "log", "--reverse", "--format=%H", f"{base}..{branch}", "--", "service/auth.py"],
    text=True,
).splitlines()
assert changes and changes[0] in fix_commits, "the authentication helper must first change in a completed fix-feature"
PY_REPAIR

# Generated tests/imports are hostile children too. Use the production gate
# wrapper with fs enforced, sanitized env, and this mission's authority
# read-denies. Never fall back to running the final audit on the host.
"$KRANZ_ACCEPTANCE_GATE_BIN" "$deliverable" "$mission_dir"

printf 'acceptance smoke passed: %s\n' "$mission_id"
