---
title: Flight Rules finding, event, replay, and evidence provenance
priority: 1
schedule: once
blocked-by: [flight-rules-resolution-pin]
---

## Goal
Make every standards selection, finding, checker verdict, drift refusal, and
disposition joinable by stable rule/revision through the append-only log,
provenance replay, report.md, and evidence bundle.

## Context
KRZ-343; design D-H. Extend the shipped evidence spine rather than parse
orchestrator prose. `events.rs` and `types.rs` are contract files: all fields
are additive/defaulted and old plans/logs must fold byte-compatibly. Existing
`Finding.subject` remains valid for old consumers; structured rule provenance
must not be smuggled into that string.

## Acceptance hints
- Add optional rule ID, revision, source, lifecycle/level, checker ID, and digest fields; gate results admit stable `ruleIds` without changing boolean-only gates.
- `standards.resolved` records selection inputs, revisions, source digest, effective-time evaluation and approval sequence; `standards.drifted` records both digests and changed applicable rules.
- Report/provenance/evidence render `passed`, `failed`, `advisory`, `waived`, `not-evaluated`, and `not-applicable`; missing evidence is never pass.
- Removed run artefacts stay `unresolved`, preserving the existing replay contract; the pinned normalized manifest remains inspectable.
- Fixture logs from before Flight Rules fold and render unchanged.
- Same log/manifest produces byte-identical JSON, replay, and bundle output.
- Anti-vacuity: unique filter `flight_rules_provenance_` reports a nonzero pass count.
