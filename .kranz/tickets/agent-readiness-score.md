---
title: kranz ready — agent-readiness score for a repo (0-100 + highest-leverage fix)
priority: 3
schedule: once
---

## Goal

A `kranz ready [--repo]` command that scores how ready a repo is for
autonomous missions and names the single highest-leverage fix. Checks
(each a scored dimension with a one-line remedy): test suite present
and runnable; CI config detectable (feeds M8 per-repo merge gates);
docs the planner leans on (README, CLAUDE.md/AGENTS.md) present;
.gitignore hygiene for kranz runtime files; contract-command
prerequisites present (reuse the environment-preflight machinery);
clean default branch state; calibration corpus size (0 completed
missions renders as an explicit cold-start warning, not a footnote).

## Context

M8 onboarding leg (docs/roadmap.md M8 — "fresh-repo onboarding"):
before a new repo's first mission, the operator should get a diagnosis,
not a surprise mid-draft. The environment-preflight code already probes
contract prerequisites at run time; this reuses it repo-wide and ahead
of time. Output: human-readable score card on the CLI plus a JSON shape
serve can expose later (dashboard onboarding panel rides M8, not this
ticket).

## Acceptance hints

- `kranz ready` on this repo scores high and names something honest.
- On a bare fixture repo (no tests, no CI) it scores low and the
  highest-leverage fix is the missing test runner, not a cosmetic item.
- Score dimensions and remedies covered by unit tests; cargo test
  --workspace green; fmt/clippy clean.
