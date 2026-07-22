---
title: Validator command discipline — cwd is the worktree, no compound bash, dedicated tools first
priority: 2
schedule: once
---

## Goal
Stop the dominant validator failure class — aborts and grant-parks on
commands the allow-set can't match — by teaching validators (in
validator-scrutiny.md and validator-functional.md) three rules: (1) the
session cwd IS the integration worktree, so git inspection uses plain forms
(`git diff <sha>..HEAD -- <path>`, `git status`, `git log`) with no `cd`
prefix; (2) NO compound bash ever — no pipes, `;`, or `&&` between
operations; one single-segment command per call; (3) prefer the dedicated
tools for enumeration and reading (Glob for file lists, Read for content,
Grep for search) over bash find/awk/sed/head/wc entirely. Backstop: where
the permission splitter can prove a `cd` targets the session cwd itself,
allow the compound (documented as a convenience, never widened past it).

## Context
Five failures in one day from this root cause, each an operator round-trip:
awk line-numbering compound (m-9e4ef3 block 1), `cd && git diff` grant
timeout (block 2), `cd && pwd && git diff` abort (m-3cda6a ms-3), `find
.kranz -type f` grant-cap exhaustion (m-3cda6a again), `head; echo; wc`
compound (m-9e4ef3 block 3). Every one was a legitimate read-only inspection
expressed in a shape the permission splitter must deny — the model reaches
for shell idioms because nothing tells it where it runs or that dedicated
tools exist. The grant flow then depends on a human approving within the
hour, which fails silently overnight (deny-default). One prompt section per
validator removes the class at the source; the allow-set stays exactly as
strict.

## Acceptance hints
- Both validator prompts carry the three rules with plain-form examples;
  prompts.rs hash tests updated.
- A mission_test scenario: a mock validator using only plain git forms and
  dedicated tools completes validation with zero grant requests and zero
  denied tool results.
- (Backstop, if implemented) `cd <session-cwd> && git <read-only-form>` is
  permitted; the same compound to any other path is still refused.
- cargo test --workspace green.
