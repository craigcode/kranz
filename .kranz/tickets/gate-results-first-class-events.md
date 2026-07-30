---
title: Gate results as first-class events with log-resolvable artefacts
priority: 2
schedule: once
blocked-by: [gate-plugin-interface]
---

## Goal
Every gate execution appends a typed event carrying the gate id, its
position in the ladder, the verdict, and an artefact reference resolvable
from the event log alone — no dependency on external state that may have
moved.

## Context
From docs/scoping/governance-evidence-layer.md (KRZ-312). events.rs is a
CONTRACT FILE: additive fields only (`#[serde(default)]`), and a schema
addition is a deliberate contractChangeRequest — follow that process, never
break old logs. Artefact references must be mission-relative or
content-addressed, never absolute host paths; a reference whose bytes are
gone must resolve to "unresolved", never to an error that blocks replay.
This is the last substrate gap ahead of provenance-replay.

## Acceptance hints
- Replaying a fixture log reconstructs the full gate ladder for a mission —
  ids, order, verdicts, artefact refs — with no reads outside the log.
- Old event logs (fixtures without gate events) still fold cleanly.
- An artefact ref with missing bytes reports unresolved, not failure.
- Anti-vacuity grep on a named filter unique to this work.
