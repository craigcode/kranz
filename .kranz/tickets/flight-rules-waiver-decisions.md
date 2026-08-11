---
title: Human-approved, exact, expiring Flight Rules waiver decisions
priority: 1
schedule: once
blocked-by: [flight-rules-resolution-pin, flight-rules-finding-provenance]
state: done
state-note: Implemented — standards.waiver.approved event (rule id+revision, manifest digest, approval seq, finding fingerprint, affected paths, affected-path-diff digest, reason, local-operator + cli surface, expiry), `kranz standards waive` with fail-closed refusals (waivable:false / unpinned rule / revision mismatch / absent finding / already-waived / past expiry), the coverage fold's waived join (exactly-one subtraction, log-frontier expiry, human-surface check) named through the shared matrix renderer. 16 flight_rules_waiver_* tests; full workspace gates green.
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

## Wrong plan (from orchestrator)
KRZ-344's entire binding surface is absent from the base this mission branches from (main @ 90a4bdc): `git grep -ci standards main -- crates apps` returns exactly one hit, a doc comment at crates/engine/src/pack.rs:7. There is no schema-4 `[standards]` root (pack.rs:194-208 supports only SCHEMA_BASE 2 and SCHEMA_CONTRACT 3), so no rule can declare `waivable: true` or carry a `revision`; types.rs:518 `Finding` is still subject/severity/evidence/suggestedFix/class with no rule id, revision, checke … (truncated)
