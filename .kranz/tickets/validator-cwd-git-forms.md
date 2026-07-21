---
title: Teach validators their cwd is the worktree (plain git forms, no cd-prefix compounds)
priority: 3
schedule: once
---

## Goal
Stop validator aborts/grant-parks on compound `cd <worktree> && git ...`
commands: tell validator prompts (validator-scrutiny.md and
validator-functional.md) that the session cwd IS the integration worktree,
so all git inspection uses plain forms (`git diff <sha>..HEAD -- <path>`,
`git status`, `git log`) with no `cd` prefix and no `pwd &&` compounds —
forms that sit inside the read-only allow-set and need no grant. Backstop:
where the permission splitter can prove a `cd` targets the session cwd
itself, allow the compound (documented as a convenience, never widened past
it).

## Context
Three failures in one day from the same root cause: validator aborted on an
awk line-numbering compound (m-9e4ef3 first block), validator grant timed
out on `cd <worktree> && git diff ... -- merged_test.rs` (m-9e4ef3 second
block), functional validator aborted on `cd <worktree> && pwd && git diff
--name-only ...` (m-3cda6a, ms-3). Each was a read-only inspection the
validator legitimately needed, expressed in a shape the allow-set can't
match — and each cost an operator round-trip. The model reaches for `cd`
because nothing tells it where it runs; one paragraph in the prompts removes
the whole class.

## Acceptance hints
- Both validator prompts state the cwd is the worktree and show the
  plain-form git examples; prompts.rs hash tests updated.
- A mission_test scenario: a mock validator using `git diff` without a cd
  prefix completes validation with zero grant requests.
- (Backstop, if implemented) `cd <session-cwd> && git <read-only-form>` is
  permitted; the same compound to any other path is still refused.
- cargo test --workspace green.
