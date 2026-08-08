---
title: Bind Flight Rules lifecycle and levels to authoritative gate enforcement
priority: 1
schedule: once
blocked-by: [flight-rules-resolution-pin, flight-rules-finding-provenance, flight-rules-waiver-decisions]
---

## Goal
Evaluate applicable rules with their declared mechanism so approved rules and
enforced SHOULDs remain advisory while a failing enforced MUST blocks final
completion/merge unless an exact authorized waiver permits it.

## Context
KRZ-346; design D-B/D-F. Reuse the shipped GatePipeline, pack gates,
gate.result events, sandboxed gate runner, and confidence contract. Do not add
a second review loop. Checker forms are registered deterministic gate ID,
engine-owned contextual `agent-judgement`, or authorized
`manual-attestation`; rule/RFC prose can never embed a command.

Ordering remains structural: deterministic gates before model judgement.
The checker owns its authoritative verdict and optional score/threshold;
Kranz records the score but never derives or reverses the verdict from it.
Flight Rules add to the engine floor and can neither shadow, waive, nor reorder
it.

## Acceptance hints
- Matrix tests pin all status × level outcomes: draft/retired absent; approved nonblocking; enforced SHOULD advisory; enforced MUST authoritative.
- Approved and enforced active rules run their checker; only lifecycle/level/waiver policy changes whether a failing verdict blocks.
- A deterministic rule binds only to a stable gate ID and runs in the same cleared, sandbox-wrapped environment against the active/final or integration tree.
- Engine checkers come from immutable code; pack checker declaration/command comes from the approval-pinned base pack, never a mission-worktree lookup; drift refuses merge.
- The contextual checker emits one structured verdict per rule against the full diff; missing/duplicate verdicts fail closed, never pass by omission.
- An unknown, unavailable, or stage-incompatible checker for an enforced MUST fails before execution/merge naming the rule.
- An enforced failure prevents COMPLETE/merge; an exact still-valid D-I waiver changes only that rule disposition to waived.
- No standards pack preserves current gate ordering and behavior.
- Anti-vacuity: unique filter `flight_rules_enforcement_` reports a nonzero pass count.
