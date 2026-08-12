---
state: done
state-note: shipped in 952cbc0
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

## Reference: Factory.ai agent-readiness taxonomy (2026-07-07)

Factory's readiness report on this repo (Level 2, 40%) is a proven
reference for `kranz ready`'s structure — mirror the category taxonomy,
tiering, and N/A handling, but ADD the kranz-specific dimension Factory
(a static scanner) cannot see.

Their 9 categories, each criterion tiered BASIC / INTERMEDIATE / ADVANCED,
inapplicable ones scored N/A (not counted):
- Style & Validation (lint, type-check, formatter, strict typing, dead
  code, naming, pre-commit hooks, complexity, duplication, tech-debt)
- Build System (build-cmd doc, deps pinned, single-command setup, monorepo
  tooling, version-drift, release automation, fast CI, agentic development)
- Testing (unit/integration exist+runnable, naming, isolation, coverage
  thresholds, flaky detection, perf tracking)
- Documentation (README, AGENTS.md + freshness, skills config, service
  arch, env template, API schema)
- Dev Environment (devcontainer, local services, DB schema, env template)
- Debugging & Observability (structured logging, tracing, metrics, health
  checks, log scrubbing, error tracking, alerting)
- Security (branch protection, secret scanning, CODEOWNERS, dep-update
  automation, gitignore, secrets mgmt, log scrubbing)
- Task Discovery (issue/PR templates, labeling, backlog health)
- Product & Experimentation (analytics, error-to-insight)

Design notes for kranz's version:
- WEIGHT / segregate: many criteria (branch protection, CODEOWNERS,
  templates, product analytics, deployment frequency, feature flags) are
  team/OSS/SaaS process, irrelevant to a solo private repo — score them but
  clearly bucket them as "going-public prep", not "code health", so the
  headline number is not dragged by inapplicable process. Factory's 40% was
  mostly this drag.
- AGENTS.md presence is a HEAVILY-weighted, unlocking signal — check it
  first (matches the took `ready` design already noted).
- ADD the dimension Factory can't measure: does the repo expose runnable
  validation commands a mission's contract can BIND to (cargo test
  --workspace, npm test, a lint/build gate)? That is kranz-specific
  agent-readiness — the difference between "ready for humans/CI" and "ready
  for kranz's own autonomous workers." Score it prominently.
