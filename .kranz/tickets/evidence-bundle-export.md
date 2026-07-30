---
title: Evidence bundle export — per-unit portable audit package
priority: 2
schedule: once
blocked-by: [provenance-replay]
---

## Goal
Export a per-unit portable bundle — inputs, gate results, diffs, reviewers,
escalations, cost, the provenance chain — self-contained and suitable for
handing to an auditor who has no access to the repo.

## Context
From docs/scoping/governance-evidence-layer.md (KRZ-326). Packaging of
provenance replay plus the artefact bytes it references (where still
present; absent artefacts are listed as unresolved in the manifest, never
silently omitted). Everything in the bundle has already passed
redact-at-write; the bundle must not reintroduce scrubbed values. Archive
determinism: normalize entry ordering and timestamps so the same log yields
the same bundle.

## Acceptance hints
- The bundle opens standalone: manifest + human summary + chain + artefacts,
  with no references into the source machine's paths.
- Same log → identical bundle bytes (normalized mtimes/ordering test).
- A log containing redaction audit events yields a bundle with fingerprints
  only (test).
- Missing artefact bytes appear as unresolved manifest entries.
- Anti-vacuity grep on a named filter unique to this work.
