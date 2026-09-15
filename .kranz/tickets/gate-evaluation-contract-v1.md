---
state-note: S1 design, schemas and fixture checks complete; runtime integration remains S3/S4/S5.
state: done
title: Gate evaluation contract v1 — typed evidence and separate decision authority
priority: 1
schedule: once
---

## Goal

Specify the external gate wire contract and additive lifecycle needed to bind
checks and human decisions to exact evidence. This is a contract-design slice,
not a replacement mission engine.

## Context

S1 of docs/scoping/acp-worker-gate-contract.md (2026-09-14; D-A through D-H
are selected in docs/gate-evaluation-contract.md). KRZ-311/312/313 already
shipped. Keep Gate/GatePipeline, existing gate.result events and pack schema
2/3/4 compatible.

## Scope

- Resolve the D-X recommendations into an implementation decision record.
- Define versioned gate/evaluate request/response/error schemas, typed subjects
  for approval, permission, milestone, final and merge, and stable IDs/digests.
- Separate check verdict, engine disposition and authenticated human consent.
- Specify minimal evaluator inputs versus full auditor exports; define
  artifact handling, runtime path mapping and retained/redacted byte identity.
- Record contractChangeRequest proposals for backend.rs, events.rs and
  types.rs: pending permission channel and additive request/result/resolution/
  consumption joins. No silent reinterpretation of old grants or gate surfaces.

## Acceptance hints

- Valid synthetic requests round-trip; wrong stage/subject, missing binding,
  unknown version and malformed response are rejected by schema fixtures.
- A script/model cannot claim a human actor, waive an engine floor or select
  its own blocking policy. Errors/escalations do not fabricate pass verdicts.
- One-call permissions and existing mission-wide grants are unambiguous.
- Document crash/timeout/idempotency rules and which layer owns each transition.
- Old-log/config behavior is explicitly covered in the migration matrix.
- Identify exact unique test names before drafting executable contract gates.

## Out of scope

Runtime wiring, live model spend, new worker abstractions or remote transport.

## Implementation and proof

The design record is docs/gate-evaluation-contract.md. Four versioned JSON
schemas and five synthetic stage fixture families live in crates/engine/schemas,
with offline validation after installing the hash-pinned development tools in
scripts/requirements-gate-schemas.txt. scripts/check-gate-contract.py contains
fifteen uniquely prefixed `test_gate_contract_v1_` checks, including fifteen
positive request/result pairs. These prove schema and fixture bindings, not
runtime execution, containment, live adapters or human consent.

Backend/event/state changes remain explicit contractChangeRequest proposals for
S3/S4/S5; the existing Rust contracts and old-log/config behavior are unchanged.
The five-axis self-review and local gate results are recorded in
docs/reviews/2026-09-14-gate-evaluation-contract-v1.md.
