---
state: done
state-note: 571884d: report.md restructured on the explain-diff shape — '## The plan' intent section before any statistics, features narrated with spec-intent + commits + ✓ criteria evidence inline, deterministic from the log, honest-failure shapes kept, quiz deliberately skipped (recorded as optional). Snapshot tests pin section order and inline content (report_test.rs).
title: Render report.md as an explain-diff artifact (background-first, literate diffs, quiz optional)
priority: 3
schedule: once
---

## Goal
Restructure the mission report renderer (orchestrator.rs render_mission_report)
around the explain-diff pattern: background and intent before details, prose-
narrated walkthrough of the mission's commits in reading order (not a raw
diff dump), each milestone's story with its evidence inline, and the final
gate verdicts with their actual commands. Keep the existing honest-failure
shapes (failures and abandonments narrated with the same care as successes).

## Context
Geoffrey Litt at AI Engineer World's Fair 2026 ("Understanding is the new
bottleneck"): as agents write more code, the human bottleneck shifts to
UNDERSTANDING it; his explain-diff pattern — background first, intuition
before details, literate code diffs in prose order, optional comprehension
quiz — is the best-documented structure for closing that gap. kranz's
report.md is the natural home: it is already the per-mission artifact humans
read before merging, and the 45+ committed reports are the corpus. The
renderer is a ~220-line free-function block in orchestrator.rs; this is a
rendering change only, no contract or event changes.

## Acceptance hints
- report.md opens with goal + plan summary before any diff/statistics, then
  narrates milestones/features in reading order with evidence links (events,
  commits), then contract verdicts with commands and outcomes.
- A sample mission's report shows the new structure; existing completed
  missions regenerate without errors (render is deterministic from the log).
- Optional (behind a config flag): a 3-question comprehension quiz section
  per Litt's speed-regulator idea.
- cargo test --workspace green incl. a report-structure snapshot test.
