---
state: done
state-note: docs/scoping/beads-workstore.md delivered 2026-07-24: all five questions answered from verified current evidence (Dolt re-platforming, custom statuses, leases, merge-slots, federation); recommendation stay-native + hardened bridge (D-BW-1..4), confirming the working hypothesis. D-BW decisions await operator confirmation; bridge briefs + native-adoption ideas (discussion primitive, defer_until) recorded for ticketing.
title: Investigate beads as kranz's work-item store (scoping doc + bridge-vs-substrate decision)
priority: 3
schedule: once
---

## Goal
Produce a D-X scoping doc (docs/scoping/beads-workstore.md) answering whether
kranz's core ticket/queue system should rely on the Beads project as its
work-item substrate, interoperate via a hardened bridge, or stay native —
with an explicit operator decision recorded per question. The doc must cover:
(1) whether beads' schema carries kranz's ticket fields (touch_set, scoping
answers, acceptance hints, mission linkage) natively or via extension;
(2) whether beads can model the pipeline state machine (New→Drafting→
NeedsContext/Review→Queued→Running→Parked→Done/Failed plus the
DELIVERED/LANDED merged split) without lossy status mapping; (3) what the
queue claim protocol (atomic rename claims, dead-claim recovery, repo-busy
guard, M8 fair round-robin) maps to in beads — if anything; (4) the
migration/provenance story for 85 committed tickets and the dogfood record;
(5) effort/risk of bridge-hardening vs substrate replacement, weighed
against the D5 precedent.

## Context
The citizenship plan (docs/gascity-citizenship.md) already rules the
adjacent question: D5 says "private spool is a stand-in; kranz-native queue
is the better target but unscheduled" — the bridge exists in spike form
(packaging/gascity: kranz-dispatch translates kranz-labeled beads into
ticket-shaped mission.md). The temptation to revisit: beads offers
comments/threads on work items (kranz tickets have no discussion primitive;
the thread_ts capture is aspirational), cross-rig federation, and
less custom machinery. The counterweight: the queue claim/drain layer is
mission-safety machinery (atomic claims under multi-process drains,
dead-claim recovery, repo-busy serialization, M8 catalog scheduling), and
kranz's pipeline semantics (Parked, DELIVERED/LANDED) and contract fields
(touch_set) have no beads equivalent. Working hypothesis to test, not
assume: interoperate via hardened bridge rather than rely as substrate.
Investigate beads' CURRENT schema/CLI directly (the project moves fast —
verify, don't cite from memory).

## Acceptance hints
- docs/scoping/beads-workstore.md exists with the five questions each
  answered from evidence (beads schema dump/CLI probes cited verbatim) and
  D-X decisions recorded with the operator.
- A clear recommendation: rely / bridge / stay-native, with the rejected
  shapes and why.
- If bridge: a hardened translator design (claim mapping, status mapping,
  provenance) sized as 1-2 mission briefs. If stay-native: the doc records
  what beads features (comments, federation) to adopt natively instead.
- No code changes; doc + decision only.
