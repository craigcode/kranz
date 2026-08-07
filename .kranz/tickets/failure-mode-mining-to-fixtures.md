---
state: done
state-note: harvest documented in docs/notes/failure-mode-harvest.md (8 clusters with counts, mission ids, and fixture locations + re-run recipe); the one genuine fixture gap found and shipped: control.rs rapid_back_to_back_enqueues_drain_in_issue_order (same-ms flake). Other clusters verified already fixture-covered by this week's repairs (contract lint, validator repair, allowlist blocks, auth probe, grant e2e, mock-order test fixes). Open candidates recorded for the third-occurrence rule.
title: Mine mission traces for failure-mode clusters and turn them into regression fixtures
priority: 3
schedule: once
---

## Goal
A harvest pass over the mission corpus (events.jsonl + lessons/) that
clusters recurring failure modes — contract-authoring bugs, secret-scan FP
classes, validator command friction, grant-park patterns, respawn loops —
and converts each validated cluster into a durable regression fixture: a
contract-lint pattern, a scanner rule test, a permission-profile fixture, or
a mission_test scenario. The trace corpus becomes an eval flywheel instead of
a write-only archive.

## Context
AI Engineer World's Fair 2026, twice independently (Mutagent's "learned
failure indicators" and Nearform's failure-clusters-become-evals loop):
diagnosing traces at scale requires code-checkable failure signals, and
every validated failure cluster should become a regression test. kranz's own
week proves the raw material exists: the inverted-grep contract bug, the BSD
grep -L inversion, the two-filter cargo test, the full-file checkpoint scan
FP families (already fingerprinted), the validator awk-abort. Some clusters
are already fixed as one-offs (contract lint, allowlist entries); this
ticket is the systematic pass — name the cluster, pin the fixture, prevent
the third occurrence instead of the first.

## Acceptance hints
- A documented harvest (script or mission) enumerates failure clusters with
  counts and example mission ids across the corpus.
- Each accepted cluster ships a fixture: contract-lint pattern, scrub rule
  test, permission fixture, or mission_test scenario — named after the
  cluster.
- The harvest is re-runnable (new missions extend the cluster report).
- cargo test --workspace green with the new fixtures passing.
