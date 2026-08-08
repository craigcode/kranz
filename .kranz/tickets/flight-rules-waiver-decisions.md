---
title: Human-approved, exact, expiring Flight Rules waiver decisions
priority: 1
schedule: once
blocked-by: [flight-rules-resolution-pin, flight-rules-finding-provenance]
---

## Goal
Provide an explicit authorized-human exception path for a rule that declares
`waivable: true`, bound narrowly enough that the waiver cannot survive a
meaningful rule, finding, scope, or diff change.

## Context
KRZ-344; design D-I. Existing validator conversion lets the orchestrator waive
ordinary judgement findings; that is insufficient authority for an enforced
organizational MUST. A model may request a waiver or propose a fix but cannot
approve one. Engine floor and non-waivable rule failures remain non-waivable.

The event is the mission-local source of truth. Standing organization
exceptions, global bypasses, and mission-branch allowlists are deliberately
out of scope.

## Acceptance hints
- Add one authenticated CLI/host action that displays evidence and records reason, approver, rule revision, finding fingerprint, paths, diff digest, sequence, and expiry.
- The digest covers the affected-path diff (whole diff only for an unscoped rule); actor evidence uses the authenticated principal or honestly names `local-operator` plus surface.
- Refuse waiver for `waivable: false`, expired rule/RFC, absent finding, mismatched revision/digest, or an unauthorized actor.
- An affected-path diff, rule revision, finding fingerprint, or expiry change invalidates the waiver and restores the block; unrelated paths receive no authority.
- A waiver subtracts exactly one matching standards failure and never disables a checker/RFC/domain/class or engine floor gate.
- Replay/report/evidence name the waiver and reason without relying on free-form orchestrator decision parsing.
- No `--ignore-standards`, auto-waive, wildcard, or permanent default exists.
- Anti-vacuity: unique filter `flight_rules_waiver_` reports a nonzero pass count.
