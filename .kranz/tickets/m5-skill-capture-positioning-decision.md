---
state: open
title: Decide whether repeated-pattern skill capture belongs inside Kranz's evidence boundary
priority: 3
schedule: once
---

## Goal

Resolve M5's remaining "skill capture" bullet against the positioning freeze
before any implementation is proposed. Determine whether turning repeated
mission lessons into human-approved agent skill files is an evidence/governance
feature Kranz should own, an adapter/export concern, or prompt-optimization work
that stays outside the harness.

## Constraints

- Treat `docs/knowledge/decisions/positioning-governance-evidence-layer.md` as
  the decision boundary.
- Do not add prompt routing, context management, or an in-harness primitive
  whose purpose is to make an agent write better code.
- Never write directly into a consumer's `.claude/skills`, Codex skills, or
  equivalent runtime directory without a separate explicit human approval.
- Preserve source mission/run provenance and redact secrets from any proposed
  export.

## Acceptance hints

- A short accepted decision records build / adapter-export / wontfix and why.
- If retained, the smallest contract names the repetition threshold,
  provenance, review artifact, destination ownership, invalidation, and human
  approval boundary.
- The roadmap no longer presents skill capture as unowned work.
