---
title: Project applicable Flight Rules through planning, work, and validation
priority: 1
schedule: once
blocked-by: [flight-rules-resolution-pin, flight-rules-finding-provenance]
---

## Goal
Feed one approval-pinned standards manifest through compact stage-specific
projections for planning, plan review, workers, and both validators, with the
exact projection identity recorded in existing prompt/session provenance.

## Context
KRZ-345; design D-D/D-G. This is a deterministic policy projection, not a
new retrieval/context engine: selection is metadata-only and the compact
normative statements are bounded separately from repo knowledge and lessons.
Unlike generic pack prompts, Flight Rules must reach the planner because the
plan itself needs to account for applicable policy.

All applicable normative statements must be present. If rule count/bytes exceed
the hard contract, approval fails naming the excess rather than truncating an
enforced rule. Full RFC rationale stays lazy; `AGENTS.md` and knowledge notes
are not enforcement sources.

## Acceptance hints
- Planning receives planning-stage rules selected from ticket/touch hints and names each source; plan review renders the proposed pinned manifest.
- If the returned plan's touch set selects additional rules, the planner gets their exact delta in a bounded revision turn before review.
- Worker, scrutiny, and functional sessions receive only rules targeting their stage/role, in stable order, with a marked untrusted-content boundary.
- Each spawned session's prompt hash covers the exact standards projection; replay identifies the manifest/projection digest.
- Approved and enforced rules are clearly labelled; prompt text never claims that an approved-only rule can block.
- Over-budget applicable policy fails approval; no rule is silently omitted.
- No pack/no applicable rules preserves current prompts byte-for-byte.
- Anti-vacuity: unique filter `flight_rules_projection_` reports a nonzero pass count.
