---
state: done
state-note: Done: freeze propagated — AGENTS.md Change-discipline boundary, what-is-kranz will-not-build section, prompts.rs/knowledge.rs module-doc pointers. docs/roadmap.md note deferred (Cursor has the file open in-flight). Doc/comment-only diff; full gates green.
title: Propagate the execution-primitive freeze into operating docs
priority: 2
schedule: once
---

## Goal
Make the positioning ADR's freeze findable at the point of temptation:
roadmap boundary section, module-doc notes on frozen surfaces, and pointers
from AGENTS.md / what-is-kranz — so the boundary cannot quietly erode.

## Context
From docs/scoping/governance-evidence-layer.md (KRZ-305). The decision
itself shipped as docs/knowledge/decisions/
positioning-governance-evidence-layer.md; this ticket is the propagation.
The ADR's retained-vs-frozen split is the source of truth: worktree/merge
machinery and the AgentBackend seam are retained (governance and dispatch),
new execution-side primitives are frozen. Mostly a docs change; no code
behavior changes.

## Acceptance hints
- Roadmap gains a boundary note referencing the ADR; the "explicitly out of
  scope" section absorbs the frozen list.
- Frozen-surface module docs carry a one-line pointer to the ADR.
- No behavioral diff: full workspace gates pass with only doc/comment
  changes in the diff.
