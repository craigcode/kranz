---
title: Provenance replay — reconstruct why a unit passed from the log alone
priority: 1
schedule: once
blocked-by: [gate-results-first-class-events]
---

## Goal
`kranz provenance <mission>` reconstructs, from the event log alone, why a
unit passed: which gates in which order, which artefacts, which
model/backend, which prompt version, which human decisions — as both a
machine-readable chain and a human summary.

## Context
From docs/scoping/governance-evidence-layer.md (KRZ-325) — the
highest-value ticket in the series; this is the audit story. The substrate
is built for it: append-only single-writer log, redact-at-write, pure-fold
reporting; gate events (gate-results-first-class-events) close the last
gap. Prompt version: check what identity prompts.rs exposes today — if
none, record a prompt content hash at session start as an additive event
field (contract-file discipline). The chain must resolve from the log even
when artefact bytes (runs/) are gone: refs report unresolved rather than
failing the replay.

## Acceptance hints
- Over a fixture log, the replay names every gate verdict in order, every
  artefact ref, the backend/model per session, the prompt identity, and
  each human approval with its event seq.
- With the runs/ directory removed, the chain still reconstructs; artefact
  refs read as unresolved, never as errors.
- Deterministic: same log → byte-identical machine output.
- Anti-vacuity grep on a named filter unique to this work.
