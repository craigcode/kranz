"""Exercise the release acceptance harness without a model or credentials."""

import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parent.parent
FAKE = r'''#!/usr/bin/env python3
import json, os, pathlib, subprocess, sys
repo = pathlib.Path(sys.argv[2])
mode = os.environ["FIXTURE_MODE"]
if mode == "failed":
    sys.exit(2)
def git(*args):
    return subprocess.check_output(["git", "-C", str(repo), *args], text=True).strip()
initial = git("rev-parse", "HEAD")
branch = "kranz/mission-m-abcdef"
worker = repo.parent / "worker"
git("worktree", "add", "-b", branch, str(worker))
(worker / "service").mkdir(exist_ok=True)
(worker / "tests").mkdir()
(worker / "service/__init__.py").write_text("")
if mode != "unrepaired-auth":
    (worker / "service/auth.py").write_text("def accepts_bearer(bearer_value): return bearer_value == 'example-kranz-token'\n")
if mode == "bad-auth":
    (worker / "service/auth.py").write_text("def accepts_bearer(bearer_value): return True\n")
(worker / "service/store.py").write_text("def list_items(): return [{'id': 1, 'name': 'fixture'}]\n")
(worker / "service/server.py").write_text("""
import json
from http.server import BaseHTTPRequestHandler, HTTPServer
from .store import list_items
class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        value = self.headers.get('Authorization')
        status = 401 if value is None else (200 if value == 'Bearer example-kranz-token' else 403)
        self.send_response(status)
        self.end_headers()
        self.wfile.write(json.dumps({'items': list_items()}).encode())
    def log_message(self, *args): pass
def make_server(port=0): return HTTPServer(('127.0.0.1', port), Handler)
""")
client = """
import argparse, sys, urllib.request, urllib.error
parser = argparse.ArgumentParser()
parser.add_argument('--url')
parser.add_argument('--token')
args = parser.parse_args()
request = urllib.request.Request(args.url, headers={'Authorization': 'Bearer ' + args.token})
try:
    with urllib.request.urlopen(request, timeout=5) as response: print(response.read().decode())
except urllib.error.HTTPError as error:
    print(str(error), file=sys.stderr)
    sys.exit(1)
"""
if mode == "bad-cli":
    client = client.replace("sys.exit(1)", "sys.exit(0)")
(worker / "service/client.py").write_text(client)
(worker / "tests/test_store.py").write_text(
    "import unittest\nfrom service.store import list_items\nclass Store(unittest.TestCase):\n"
    + ''.join("    def test_%d(self): self.assertEqual(list_items()[0]['id'], 1)\n" % i for i in range(5))
)
(worker / "README.md").write_text("python3 -m service.client --url http://127.0.0.1:8000/items --token example-kranz-token\n")
if mode == "tamper":
    (worker / "acceptance_contract.py").write_text("pass\n")
subprocess.run(["git", "-C", str(worker), "add", "service", "tests", "README.md", "acceptance_contract.py"], check=True)
subprocess.run(["git", "-C", str(worker), "commit", "-qm", "implement fixture"], check=True)
mission = repo / ".kranz/missions/m-abcdef"
mission.mkdir(parents=True)
(mission / "plan.json").write_text(json.dumps({'milestones': [{'features': [{}, {}, {}]}, {'features': [{}, {}]}]}))
(mission / "report.md").write_text("fixture report")
(mission / "events.jsonl").write_text('{"type":"fixfeature.created"}\n')
(mission / "state.json").write_text(json.dumps({'mission': {'milestones': [{'features': [{
    'origin': 'plan' if mode == 'premature-auth' else 'fix', 'status': 'complete',
    'commits': [subprocess.check_output(['git', '-C', str(worker), 'log', '-1', '--format=%H' if mode == 'bare-sha' else '--format=%H %s'], text=True).strip()],
}]}]}}))
pathlib.Path(os.environ['FIXTURE_HEADS']).write_text(json.dumps([initial, git('rev-parse', 'HEAD'), git('branch', '--show-current')]))
print('kranz exec m-abcdef COMPLETE cost=$0.00 branch=' + branch)
'''


class AcceptanceHarnessTests(unittest.TestCase):
    def run_fixture(self, mode):
        scratch = tempfile.TemporaryDirectory(prefix="kranz-harness-test-")
        self.addCleanup(scratch.cleanup)
        root = Path(scratch.name)
        fake = root / "kranz"
        fake.write_text(FAKE)
        fake.chmod(0o755)
        env = os.environ.copy()
        env.update(
            KRANZ_BIN=str(fake), RUNNER_TEMP=str(root),
            ARTIFACT_DIR=str(root / "artifacts"), FIXTURE_MODE=mode,
            FIXTURE_HEADS=str(root / "heads.json"),
        )
        env.pop("ANTHROPIC_API_KEY", None)
        result = subprocess.run(
            ["bash", str(ROOT / "scripts/acceptance-smoke.sh")],
            env=env, text=True, capture_output=True, timeout=30,
        )
        return root, result

    def test_deliverable_passes_without_moving_primary_or_requiring_api_key(self):
        root, result = self.run_fixture("pass")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        before, after, branch = json.loads((root / "heads.json").read_text())
        self.assertEqual(before, after)
        self.assertEqual(branch, "main")
        self.assertIn("Ran 5 tests", result.stderr)
        self.assertIn("acceptance smoke passed: m-abcdef", result.stdout)
        self.assertFalse(list(root.glob("kranz-acceptance.*")))

    def test_bare_commit_hashes_also_identify_the_repair(self):
        _, result = self.run_fixture("bare-sha")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_failed_mission_retains_repository_and_exit_code(self):
        root, result = self.run_fixture("failed")
        self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
        retained = list(root.glob("kranz-acceptance.*/sample-repo/.git"))
        self.assertEqual(len(retained), 1)
        self.assertIn("retained for inspection/resume", result.stderr)

    def test_cli_authentication_failure_cannot_pass_final_acceptance(self):
        _, result = self.run_fixture("bad-cli")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("the CLI must reject a wrong credential", result.stderr)

    def test_delivered_branch_cannot_replace_the_acceptance_contract(self):
        _, result = self.run_fixture("tamper")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("diff --git a/acceptance_contract.py", result.stdout)

    def test_unchanged_seeded_helper_cannot_pass_on_correct_http_alone(self):
        _, result = self.run_fixture("unrepaired-auth")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("the authentication helper must first change", result.stderr)

    def test_planned_feature_cannot_preempt_the_required_repair(self):
        _, result = self.run_fixture("premature-auth")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("the authentication helper must first change", result.stderr)

    def test_a_fix_commit_must_actually_repair_the_seeded_helper(self):
        _, result = self.run_fixture("bad-auth")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("the seeded authentication helper must reject a wrong credential", result.stderr)


if __name__ == "__main__":
    unittest.main()
