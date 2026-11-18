#!/usr/bin/env python3
"""Fail when an outside contribution has a commit without a sign-off.

CONTRIBUTING.md asks every outside contributor to certify the Developer
Certificate of Origin by ending each commit message with

    Signed-off-by: Real Name <email>

CI runs this script on a pull request. It reads who opened the pull request
from the environment, not from the commits, because a commit's author fields
are whatever the person who made it typed.

Usage:
    check-commit-signoff.py --base <commit> --head <commit>
    check-commit-signoff.py --self-test

Environment:
    PR_AUTHOR         login of the account that opened the pull request
    REPOSITORY_OWNER  login of the account that owns the repository

A pull request opened by the repository owner, or by Dependabot, is exempt:
the owner holds the copyright, and the project's own automation commits under
the owner's pull requests. For any other author, every commit that is in head
and not in base must carry a sign-off whose address is the commit's author or
committer address. The one exception is a merge that adds nothing of its own,
such as the one GitHub's "Update branch" button makes; a merge that carries a
change, for example a resolved conflict, needs a sign-off like any commit.

The sign-off goes in the trailer lines that end the message, where
`git commit -s` puts it. The script only reads the repository.

This check runs from the pull request's own copy of the workflow and of this
script, so a pull request that changes either one is judging itself. A
maintainer reads such a change before trusting a green result.
"""

from __future__ import annotations

import argparse
import os
import re
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

sys.dont_write_bytecode = True

EXEMPT_BOTS = {"dependabot[bot]"}
SIGN_OFF = re.compile(r"^Signed-off-by:\s*(?P<name>\S.*?)\s*<(?P<email>[^<>\s]+)>\s*$")
TRAILER = re.compile(r"^[A-Za-z][A-Za-z0-9-]*:\s*\S")


def is_exempt(pr_author: str, repository_owner: str) -> bool:
    """Whether a pull request by `pr_author` needs no sign-off."""
    author = pr_author.strip().lower()
    owner = repository_owner.strip().lower()
    if not author or not owner:
        return False
    return author == owner or author in EXEMPT_BOTS


def sign_off_addresses(message: str) -> list[str]:
    """Addresses of the sign-off lines among the trailers that end `message`.

    Trailers are the paragraphs at the end in which every line has the form
    `Token: value`. Some tools add theirs as a paragraph of its own, so every
    such closing paragraph counts, not only the last.
    """
    paragraphs = [part for part in re.split(r"\n\s*\n", message.strip()) if part.strip()]
    addresses = []
    for paragraph in reversed(paragraphs):
        lines = [line.strip() for line in paragraph.splitlines() if line.strip()]
        if not all(TRAILER.match(line) for line in lines):
            break
        for line in lines:
            match = SIGN_OFF.match(line)
            if match:
                addresses.append(match.group("email").lower())
    return addresses


def is_signed_off(message: str, author_email: str, committer_email: str) -> bool:
    """Whether `message` carries a sign-off by the author or the committer."""
    accepted = {author_email.strip().lower(), committer_email.strip().lower()} - {""}
    return any(address in accepted for address in sign_off_addresses(message))


class GitError(Exception):
    """A git command failed; the message is what git printed."""


def git(*args: str, cwd: Path | None = None) -> str:
    try:
        done = subprocess.run(
            ["git", *args], capture_output=True, text=True, cwd=cwd, check=False
        )
    except OSError as error:
        raise GitError(f"cannot run git: {error}") from error
    if done.returncode != 0:
        raise GitError(f"git {' '.join(args)} failed: {done.stderr.strip()}")
    return done.stdout


def merge_adds_nothing(commit: str, cwd: Path | None = None) -> bool:
    """Whether merge `commit` is exactly what git makes of its parents.

    `--remerge-diff` (git 2.36 or later) repeats the merge and prints how the
    recorded result differs: nothing for a clean merge, the resolution for a
    conflict or for a merge somebody edited.
    """
    return not git("show", "--format=", "--remerge-diff", commit, cwd=cwd).strip()


def unsigned_commits(base: str, head: str, cwd: Path | None = None) -> list[str]:
    """One line for each commit in head and not in base that lacks a sign-off."""
    missing = []
    for line in git("rev-list", "--parents", head, "--not", base, cwd=cwd).splitlines():
        commit, *parents = line.split()
        if len(parents) > 1 and merge_adds_nothing(commit, cwd):
            continue
        author, committer = git(
            "show", "-s", "--format=%ae%n%ce", commit, cwd=cwd
        ).splitlines()[:2]
        subject = git("show", "-s", "--format=%s", commit, cwd=cwd).strip()
        message = git("show", "-s", "--format=%B", commit, cwd=cwd)
        if not is_signed_off(message, author, committer):
            missing.append(f"{commit[:12]} {subject}")
    return missing


class SelfTest(unittest.TestCase):
    def test_owner_and_dependabot_are_exempt(self) -> None:
        self.assertTrue(is_exempt("Owner", "owner"))
        self.assertTrue(is_exempt("dependabot[bot]", "owner"))

    def test_anyone_else_and_missing_values_are_not_exempt(self) -> None:
        self.assertFalse(is_exempt("someone", "owner"))
        self.assertFalse(is_exempt("", "owner"))
        self.assertFalse(is_exempt("owner", ""))
        self.assertFalse(is_exempt("", ""))

    def test_sign_off_by_author_or_committer_is_accepted(self) -> None:
        message = "fix: a thing\n\nBody.\n\nSigned-off-by: A Person <A@Example.org>\n"
        self.assertTrue(is_signed_off(message, "a@example.org", "other@example.org"))
        self.assertTrue(is_signed_off(message, "other@example.org", "a@example.org"))

    def test_sign_off_by_somebody_else_is_refused(self) -> None:
        message = "fix: a thing\n\nSigned-off-by: A Person <a@example.org>\n"
        self.assertFalse(is_signed_off(message, "b@example.org", "c@example.org"))

    def test_missing_or_malformed_sign_off_is_refused(self) -> None:
        self.assertFalse(is_signed_off("fix: a thing\n", "a@example.org", "a@example.org"))
        self.assertFalse(
            is_signed_off(
                "fix: a thing\n\nSigned-off-by: a@example.org\n",
                "a@example.org",
                "a@example.org",
            )
        )

    def test_sign_off_followed_by_ordinary_text_is_refused(self) -> None:
        message = (
            "fix: a thing\n\nSigned-off-by: A Person <a@example.org>\n\nMore text.\n"
        )
        self.assertFalse(is_signed_off(message, "a@example.org", "a@example.org"))

    def test_sign_off_followed_by_another_trailer_paragraph_is_accepted(self) -> None:
        message = (
            "fix: a thing\n\nSigned-off-by: A Person <a@example.org>\n\n"
            "Co-authored-by: B <b@example.org>\n"
        )
        self.assertTrue(is_signed_off(message, "a@example.org", "a@example.org"))

    def test_other_trailers_beside_the_sign_off_are_allowed(self) -> None:
        message = (
            "fix: a thing\n\nReviewed-by: B <b@example.org>\n"
            "Signed-off-by: A Person <a@example.org>\n"
        )
        self.assertTrue(is_signed_off(message, "a@example.org", "a@example.org"))

    def test_a_range_of_real_commits(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)

            def run(*args: str) -> str:
                return git(
                    "-c", "user.name=A Person", "-c", "user.email=a@example.org",
                    "-c", "commit.gpgsign=false", *args, cwd=repo,
                )

            def commit(name: str, *flags: str) -> None:
                (repo / name).write_text(name, encoding="utf-8")
                run("add", name)
                run("commit", "-q", *flags, "-m", f"add {name}")

            run("init", "-q", "-b", "main")
            commit("base")
            run("checkout", "-q", "-b", "work")
            commit("signed", "-s")
            self.assertEqual(unsigned_commits("main", "work", repo), [])

            # The base branch moves on unsigned; merging it in adds nothing of
            # the contributor's own, and its commits are not in the range.
            run("checkout", "-q", "main")
            commit("upstream")
            run("checkout", "-q", "work")
            run("merge", "-q", "--no-edit", "main")
            self.assertEqual(unsigned_commits("main", "work", repo), [])

            commit("unsigned")
            missing = unsigned_commits("main", "work", repo)
            self.assertEqual(len(missing), 1, missing)
            self.assertTrue(missing[0].endswith("add unsigned"), missing)

            # A merge that carries a change of its own is checked.
            run("checkout", "-q", "-b", "carrier", "main")
            run("checkout", "-q", "-b", "side", "main~1")
            commit("side", "-s")
            run("checkout", "-q", "carrier")
            run("merge", "-q", "--no-commit", "--no-ff", "side")
            (repo / "smuggled").write_text("smuggled", encoding="utf-8")
            run("add", "smuggled")
            run("commit", "-q", "-m", "merge side")
            missing = unsigned_commits("main", "carrier", repo)
            self.assertEqual(len(missing), 1, missing)
            self.assertTrue(missing[0].endswith("merge side"), missing)

            with self.assertRaises(GitError):
                unsigned_commits("no-such-commit", "work", repo)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--base")
    parser.add_argument("--head")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()

    if args.self_test:
        suite = unittest.defaultTestLoader.loadTestsFromTestCase(SelfTest)
        return 0 if unittest.TextTestRunner(verbosity=1).run(suite).wasSuccessful() else 1
    if not args.base or not args.head:
        parser.error("--base and --head are required")

    pr_author = os.environ.get("PR_AUTHOR", "")
    owner = os.environ.get("REPOSITORY_OWNER", "")
    if is_exempt(pr_author, owner):
        print(f"OK: pull request by {pr_author}, no sign-off needed")
        return 0

    try:
        missing = unsigned_commits(args.base, args.head)
    except GitError as error:
        print(f"Cannot read the commits to check: {error}")
        return 1
    if not missing:
        print("OK: every commit is signed off")
        return 0
    print("These commits have no Signed-off-by line from their author or committer:")
    for line in missing:
        print(f"  {line}")
    print(
        "Add one with `git commit -s` (or `git rebase --signoff` for several). "
        "See CONTRIBUTING.md, Licensing of contributions."
    )
    return 1


if __name__ == "__main__":
    sys.exit(main())
