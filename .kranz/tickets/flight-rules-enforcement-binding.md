---
title: Bind Flight Rules lifecycle and levels to authoritative gate enforcement
priority: 1
schedule: once
blocked-by: [flight-rules-resolution-pin, flight-rules-finding-provenance, flight-rules-waiver-decisions]
state: done
state-note: Implemented — approval-pinned deterministic, contextual, and manual-attestation checkers run through the existing gate ladder; lifecycle/level policy is exact, enforced MUST failures block final/merge, approved and SHOULD failures remain advisory, waivers subtract only their exact rule failure, and checker/policy drift fails closed. Dedicated flight_rules_enforcement_* tests plus the M5.5 proof matrix are green.
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

## Wrong plan (from orchestrator)
KRZ-346 is step 6 of 9 in the delivery sequence at docs/scoping/flight-rules-engineering-standards.md:362, and every one of steps 1-5 is unlanded on the base this mission branches from (main @ 90a4bdc, re-verified this turn): `git cat-file -e main:crates/engine/src/pack/standards.rs` fails, `git grep -ci standards main -- crates apps` returns exactly one hit (a doc comment at crates/engine/src/pack.rs:1), and pack.rs:68,72 still define only SCHEMA_BASE=2/SCHEMA_CONTRACT=3 with a live test at pac … (truncated)
