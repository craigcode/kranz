---
title: Tighten the release build, the Actions allowlist and push validation
priority: 3
schedule: once
---

## Goal

Remove three places where release and delivery tooling trusts more than it needs to.

## Context

Reported by the 2026-11-10 code review as low-severity items and not yet addressed. Each item below is a report, not a confirmed defect: confirm it against current code before changing anything, and drop any that no longer apply.

- The release build restores a build cache, so a release artifact can include output not built from the tagged source.
- The Actions allowlist check accepts any 40-hex commit under `actions/*`; it should accept only reviewed commits.
- `kranz exec --push` validates the remote after the mission has run, so a bad remote is discovered only after the spend.

## Scoping answers

- Fix each item on its own evidence; an item that cannot be reproduced is closed with a note, not patched speculatively.
- Contract files change additively only.
- Each fix carries a test that fails without it.

## Acceptance hints

- Release builds start from a clean cache.
- The allowlist names exact reviewed commits for first-party actions too, with a self-test case.
- `kranz exec --push` refuses an unusable remote before the mission starts.
