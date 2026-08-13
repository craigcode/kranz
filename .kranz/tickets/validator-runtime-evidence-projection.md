---
state-note: Implemented bounded, scrubbed WorkerReport and durable run-specific egress evidence projection for functional agent-judgement validation, with prompt-injection and false-red regressions.
state: done
title: Project bounded runtime evidence into functional validation
priority: 2
schedule: once
traced-from-mission: m-ed91b6
---

## Goal

Give the functional validator the bounded, scrubbed evidence needed to judge
`agent-judgement` assertions whose subject is the worker report or
engine-owned runtime signals, without exposing the real checkout or making
runtime files writable/readable inside the validator sandbox.

## Context

M7 proof mission `m-ed91b6` required the WorkerReport to record two exact
hostile commands and required structured `example.com:443` denial evidence.
Mandatory validator containment worked: the validator saw only its committed
snapshot, where untracked `state.json`, `events.jsonl`, and `runs/` evidence
correctly did not exist. But the validation task projected neither the latest
scrubbed report nor run-specific egress denials, so the validator emitted two
critical false-red findings. The final contract gate read the engine artifacts
outside containment and passed both assertions.

## Acceptance hints

- Only the functional validator receives the projection, and only when the
  contract contains `agent-judgement` assertions.
- Include the latest completed WorkerReport per milestone feature, bounded in
  aggregate, credential-scrubbed, and wrapped in explicit untrusted-data
  delimiters that forbid treating report text as instructions.
- Persist run-specific egress denials in additive audit schema (or an
  equivalent durable, cleanup-resistant engine record) and project only the
  relevant milestone runs. This is a deliberate contract change request:
  old event logs must deserialize with empty/default evidence.
- Do not widen validator filesystem reads or expose authority files, control
  state, raw transcripts, tokens, or operator configuration.
- A regression mission with report-backed and egress-backed judgement
  assertions validates cleanly without an orchestrator waiver; malicious
  instructions embedded in a report remain inert data.
