---
title: Scan worker commits for secrets when the feature completes
priority: 2
schedule: once
---

## Goal

A secret finding in a file a worker committed is not reported until the branch is merged or pushed, after the whole mission has been paid for. Report it when the feature completes, with the same rules and the same allowlist.

## Context

Found on 2026-11-13. The engine scans what it commits itself: a dirty-tree checkpoint is refused on an unwaived finding. The merge scans the mission diff, and `kranz exec --push` scans the range before pushing. A commit the worker makes inside its session is not scanned at that point.

In one mission two findings in an engine checkpoint blocked a feature straight away, and the worker reworded the lines. A third line of the same shape, in a file a worker had committed two features earlier, passed every milestone gate and was found only by a manual `kranz scan` over the finished branch. All three were false positives on test fixtures. The cost of the late one would have been a refused merge after the mission was complete.

This is a timing gap, not a containment gap: no unscanned content reaches the base branch or a remote.

## Scoping answers

- The scan uses the existing rules and reads the allowlist from the pinned base, as the merge does. An allowlist edit inside the mission's own range waives nothing.
- A finding is reported to the worker as a finding against that feature, by fingerprint and location. The matched text is never quoted in the event log, the prompt or the dashboard.
- Scan the feature's commit range, not the whole tree.
- Read docs/knowledge/validation/gates.md and docs/merge-gates.md first. The merge and push scans stay as they are.
- Contract files change additively only.

## Acceptance hints

- A worker commit that adds an unwaived finding fails that feature before the milestone gate runs, naming the fingerprint, rule and location.
- A fingerprint on the base's allowlist does not fail the feature.
- A test commits a detector canary from a worker-shaped commit and asserts the feature is refused. A second test asserts a clean commit passes.
- The merge and push scans still refuse the same content.
