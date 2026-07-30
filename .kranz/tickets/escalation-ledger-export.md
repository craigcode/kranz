---
title: Escalation ledger as a queryable, exportable corpus
priority: 2
schedule: once
---

## Goal
The escalation ledger becomes queryable and exportable: stable per-entry
ids, JSONL export, filterable by mission, task class, and date range —
usable both for operator forensics and as a corpus feed.

## Context
From docs/scoping/governance-evidence-layer.md (KRZ-322). The ledger
already exists inside the outcomes.rs fold; this adds the query/export
surface. Export-pattern precedent is trace_export.rs: derived,
deterministic, regenerable from the log with nothing to regenerate from
except the log. Doubles as the fine-tune corpus feed
(training-corpus-export consumes it). Payloads are already redacted at
write; the export must not reintroduce anything the scrubber removed.

## Acceptance hints
- Exporting twice over the same log yields identical bytes.
- Filters by mission / task class / date range, each tested.
- Export of a log containing a `secret.redacted` audit event carries the
  fingerprint only, never a value (test).
- Anti-vacuity grep on a named filter unique to this work.
