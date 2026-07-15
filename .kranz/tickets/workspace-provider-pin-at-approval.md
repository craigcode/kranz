---
title: Pin workspace provider identity at mission approval (M6)
priority: 3
schedule: once
blocked-by: [workspace-sandbox-visibility]
---

## Goal
When a container/remote workspace provider exists, pin the effective provider
kind + template/image identity (and version) at plan approval, and surface
readiness, preview links, and human-takeover instructions as audited mission
artifacts.

## Context
Split out of the original visibility ticket: approval-time provider pinning
is an M6 product seam (`docs/roadmap.md` M6, `docs/scoping/worker-sandboxing.md`
Tier 3), not a dashboard cosmetic. Blocked on basic local visibility so the
UI chrome exists before cloud fields are added.

Further blocked in practice on an actual workspace-provider implementation
ticket when one is filed; until then this stays P3 / not draftable as a
standalone mission that invents the provider.

## Acceptance hints
- Approval records provider id + template/image version in mission state /
  additive events.
- Dashboard/report show readiness, preview links, takeover details when
  present; secrets never logged.
- Workspace provisioning failures map to preflight or blocked with a clear
  owner (operator / provider / repo setup).
- Local worktree-only missions remain valid without a remote provider.
- Anti-vacuity grep on the named filter.
