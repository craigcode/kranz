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

The immutable `acceptance_contract.py` is the pre-agreed executable contract.
Use exactly one mission contract assertion, with command
`python3 acceptance_contract.py`: the service has at least five passing unit
tests, missing credentials return 401, a wrong non-empty token returns 403,
and the exact example token returns the store's items. This assertion must
fail on the untouched base and on the seeded authentication defect. The CLI
and documentation belong to milestone two's feature criteria; the external
acceptance runner additionally verifies them with `--final` after COMPLETE.
Do not add commands requiring milestone-two files to the milestone-one gate.
Set touchSet to `service/**`, `tests/**`, and `README.md`. Do not edit the
contract script, mission input, .gitignore, or secret allowlist.

Deliberately seed one validator-detectable defect in the HTTP endpoint feature:
accept any non-empty Bearer token instead of requiring the exact token
`example-kranz-token`. Do not mention the defect in that feature's validation criteria
and do not repair it in a later planned feature. The mission-level contract,
however, MUST state that a wrong non-empty token is rejected with HTTP 403.
This controlled contradiction is intentional: validation must report it after
milestone one, the orchestrator must create a fix-feature, and that fix-feature
must repair it before the mission completes.

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

mkdir -p "$repo/.kranz"
cat >"$repo/.kranz/secret-allowlist" <<'EOF'
# Reviewed false positive: the runtime bearer-extraction call observed during
# the release rehearsal. This fingerprint waives only that exact expression;
# literal credentials and every other scanner finding remain blocked.
62bbd92fdcee8c5fb97a7061
EOF

cat >"$repo/.gitignore" <<'EOF'
__pycache__/
*.pyc
EOF

git -C "$repo" add README.md mission.md .gitignore acceptance_contract.py .kranz/secret-allowlist
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

(
  cd "$deliverable"
  python3 acceptance_contract.py --final
)

printf 'acceptance smoke passed: %s\n' "$mission_id"
