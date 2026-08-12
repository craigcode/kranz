---
title: Project applicable Flight Rules through planning, work, and validation
priority: 1
schedule: once
blocked-by: [flight-rules-resolution-pin, flight-rules-finding-provenance]
state: done
state-note: Implemented — pack/projection.rs (stage projections with marked untrusted boundary, honest approved-vs-enforced labels, per-rule sources, manifest+projection digests, 64-rule/16-KiB hard budget), approval fails over-budget naming the excess, planning seed projection + bounded fixed-point revision loop (3 turns, then park), worker/scrutiny/functional prompts carry only their stage's rules with the prompt hash covering the exact projection; no pack/no applicable rules byte-identical. 17 flight_rules_projection_* tests; full workspace gates green.
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

## Wrong plan (from orchestrator)
KRZ-345's entire input is absent from the base this mission branches from. On main @ 90a4bdc, `git grep -in standards main -- crates apps` returns exactly one hit — a doc comment at crates/engine/src/pack.rs:7. There is no standards module, no StandardsManifest, no rule/revision/stage/level/lifecycle type, no normalized digest, and crates/engine/src/pack.rs:197 still accepts only SCHEMA_BASE=2 and SCHEMA_CONTRACT=3, so `schema = 4` and `[standards] root` do not exist. This ticket projects an app … (truncated)
