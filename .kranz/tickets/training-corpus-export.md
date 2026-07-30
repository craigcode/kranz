---
title: Provenance-tagged training-corpus export
priority: 3
schedule: once
blocked-by: [divergence-first-class-event, escalation-ledger-export]
---

## Goal
A training-corpus export drawing on validated worker traces, divergence
events, and the escalation ledger — every record provenance-tagged
(mission, backend/model, gate-chain reference) and deterministic over the
same logs.

## Context
From docs/scoping/governance-evidence-layer.md (KRZ-332). Extension of
trace_export.rs, which already exports validation-PASSED traces in
instruction-pair form with deterministic regeneration; this adds the
provenance tags and the divergence/escalation sources. Content-provenance
discipline from the local-inference flywheel ticket applies (record what
the pair was derived from). Unvalidated traces are excluded by
construction — a failed or unvalidated session never enters the corpus.

## Acceptance hints
- Same logs → byte-identical export.
- Every record carries provenance refs that resolve via provenance replay.
- A fixture with a failed session proves exclusion.
- Divergence-sourced records reference both candidates and the resolution.
- Anti-vacuity grep on a named filter unique to this work.
