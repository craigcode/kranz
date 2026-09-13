#!/usr/bin/env python3
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPTS = Path(__file__).resolve().parent
TERM = "operator-fixture[private]"


class OperatorMarkerAuditTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.repo = self.root / "repo"
        self.repo.mkdir()
        self.env = {k: v for k, v in os.environ.items()
                    if not k.startswith(("KRANZ_", "GIT_"))}
        self.git("init")
        self.git("config", "user.name", "audit-fixture")
        self.git("config", "user.email", "audit@example.invalid")
        self.git("config", "commit.gpgsign", "false")
        self.git("commit", "--allow-empty", "-m", "initial")

    def git(self, *args):
        return subprocess.run(["git", "-c", "core.hooksPath=/dev/null",
                               "-c", "core.fsmonitor=false", *args], cwd=self.repo,
                              env=self.env, check=True, capture_output=True)

    def audit(self, scope, markers="", required="1", filename=None, wrapper=False):
        env = dict(self.env, KRANZ_PUBLIC_AUDIT_MARKERS=markers,
                   KRANZ_REQUIRE_OPERATOR_MARKERS=required, KRANZ_SKIP_GITLEAKS="1")
        if filename is not None:
            env["KRANZ_PUBLIC_AUDIT_MARKERS_FILE"] = str(filename)
        command = (["bash", str(SCRIPTS / f"audit-public-{scope}.sh")] if wrapper else
                   [sys.executable, str(SCRIPTS / "audit-operator-markers.py"), scope])
        result = subprocess.run(command, cwd=self.repo, env=env, capture_output=True, text=True)
        self.assertNotIn(TERM, result.stdout + result.stderr)
        return result

    def track(self, name, data):
        path = self.repo / name
        path.write_bytes(data)
        self.git("add", name)
        self.git("commit", "-m", "fixture content")

    def test_both_public_wrappers_require_a_nonempty_vocabulary(self):
        for scope in ("tree", "history"):
            with self.subTest(scope=scope):
                self.assertEqual(self.audit(scope, wrapper=True).returncode, 2)
                self.assertEqual(self.audit(scope, "\n# comment only\n", wrapper=True).returncode, 2)
                self.assertEqual(self.audit(scope, TERM, wrapper=True).returncode, 0)

    def test_literal_markers_are_not_regular_expressions(self):
        self.track("clean.txt", b"operator-fixturep")
        self.assertEqual(self.audit("tree", TERM).returncode, 0)
        (self.repo / "clean.txt").write_text(TERM)
        self.assertEqual(self.audit("tree", TERM, wrapper=True).returncode, 1)

    def test_file_and_environment_vocabularies_are_both_applied(self):
        vocabulary = self.root / "private-vocabulary"
        vocabulary.write_bytes(b"\xef\xbb\xbf# reviewed\r\n" + TERM.encode() + b"\r\n")
        self.track("payload.bin", b"\0" + TERM.encode())
        self.assertEqual(self.audit("tree", "other-fixture", filename=vocabulary).returncode, 1)
        self.assertEqual(self.audit("tree", "other-fixture").returncode, 0)

    def test_missing_or_invalid_files_and_flags_fail_closed_without_echo(self):
        missing = self.root / TERM
        self.assertEqual(self.audit("tree", filename=missing).returncode, 2)
        missing.write_bytes(b"\xff")
        self.assertEqual(self.audit("tree", filename=missing).returncode, 2)
        self.assertEqual(self.audit("tree", TERM, required="yes").returncode, 2)

    def test_history_finds_deleted_content(self):
        self.track("private.txt", TERM.encode())
        self.git("rm", "private.txt")
        self.git("commit", "-m", "remove fixture")
        self.assertEqual(self.audit("tree", TERM).returncode, 0)
        self.assertEqual(self.audit("history", TERM, wrapper=True).returncode, 1)

    def test_history_finds_commit_messages_and_identity_headers(self):
        self.git("commit", "--allow-empty", "-m", TERM)
        self.assertEqual(self.audit("history", TERM).returncode, 1)
        self.git("-c", "user.name=identity-fixture", "commit", "--allow-empty", "-m", "identity")
        self.assertEqual(self.audit("history", "identity-fixture").returncode, 1)

    def test_history_finds_branch_names_and_annotated_tag_metadata(self):
        self.git("branch", "private-ref-fixture")
        self.assertEqual(self.audit("history", "private-ref-fixture").returncode, 1)
        self.git("tag", "-a", "fixture-tag", "-m", TERM)
        self.assertEqual(self.audit("history", TERM).returncode, 1)

    def test_tree_checks_symlink_text_without_reading_its_target(self):
        target = self.root / "outside"
        target.write_text(TERM)
        (self.repo / "link").symlink_to(target)
        self.git("add", "link")
        self.assertEqual(self.audit("tree", TERM).returncode, 0)
        (self.repo / "link").unlink()
        (self.repo / "link").symlink_to(TERM)
        self.assertEqual(self.audit("tree", TERM).returncode, 1)

    def test_missing_tracked_content_fails_closed(self):
        self.track("tracked", b"clean")
        (self.repo / "tracked").unlink()
        self.assertEqual(self.audit("tree", TERM).returncode, 2)

    def test_shallow_history_cannot_claim_an_all_history_pass(self):
        clone = self.root / "shallow"
        subprocess.run(["git", "clone", "--depth=1", self.repo.as_uri(), str(clone)],
                       check=True, env=self.env, capture_output=True)
        self.repo = clone
        self.assertEqual(self.audit("history", TERM).returncode, 2)

    def test_unconfigured_optional_scan_preserves_existing_workflow(self):
        self.assertEqual(self.audit("tree", required="0").returncode, 0)
        self.assertEqual(self.audit("history", required="0").returncode, 0)


if __name__ == "__main__":
    unittest.main()
