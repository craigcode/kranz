---
title: Record whether each lesson could be a deterministic check, and list the ones that are not yet
priority: 2
schedule: once
---

## Goal

When a mission captures a lesson, the record says whether a deterministic check (an approval-lint rule, a preflight rule or a gate) could catch the same mistake. An operator can list the lessons that could be checks and have no check yet, so a lesson that keeps recurring becomes a rule and stops depending on a planner reading prose.

## Context

Lessons live in `.kranz/lessons/`, one file per mission, with an index. Most of them are about how to write a contract command: missing anti-vacuity guards, filters that match an existing test, diffs that include Kranz's own bookkeeping paths. Some of those mistakes are now caught by preflight or approval-lint rules, such as the anti-vacuity warning. Nothing links a lesson to the rule that covers it, and nothing lists the lessons that still have no rule.

Seen again on 2026-11-19: the approval lint ran a command that timed out and reported it among commands that "already pass or could not reach a verdict". Two missions blocked before anyone noticed. Nothing in the lesson flow asks "could a check have caught this?".

The gap is in the record and the follow-up, not in the checks themselves. This ticket adds no new lint rule.

## Scoping answers

- Flag as a D-X operator decision who classifies a lesson as checkable: the orchestrator when it captures the lesson, the operator afterwards, or both with the operator's answer winning.
- The classification is a small closed set, for example `checkable`, `not-checkable`, `unclassified`, plus an optional link to the ticket or rule that implements the check. Existing lessons read as `unclassified`.
- A read-only CLI listing shows lessons that are `checkable` with no linked check, oldest first.
- Do not generate lint rules automatically, and do not change how lessons are injected into prompts. `prompts.rs` and `knowledge.rs` are frozen (see `docs/knowledge/decisions/positioning-governance-evidence-layer.md`). Read that note first; if the change drifts toward how lessons shape agent behaviour, stop and check the boundary.
- `review_memory.rs` already promotes review rules through a replay suite and a human approval. Say in the plan whether lesson checks reuse that route or stay a plain ticket link, and why.
- Lesson files are tracked source. Keep the format readable by hand and backward compatible with the existing index.

## Acceptance hints

- A test: a lesson captured with a `checkable` classification and no link appears in the listing; adding a link removes it.
- A test: existing lesson files with no classification parse and list as `unclassified`.
- A test: the listing is read-only and takes no engine lock.
- Anti-vacuity: the test filter matches only the new tests.
