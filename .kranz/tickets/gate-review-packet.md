---
state-note: Implemented and reviewed in PR #73; merged as cd7537ea076be60a63da9a27f4cb26c36d3f1b84 after all required checks passed.
state: done
title: Gate review packet — show scope, current evidence and the human decision
priority: 1
schedule: once
blocked-by: [acp-governed-mission-acceptance]
---

## Goal

Let an operator assess a mission's pending decision from a compact, traceable
review packet without reconstructing its event log. Render approved scope,
actual changes, current checks, independent findings and remaining human
obligations through the existing report and decision surfaces.

## Context

The operator authorized ticketing and scheduling the review-efficiency lessons
from [Vercel's software factory article](https://vercel.com/blog/building-a-software-factory-for-ai-sdk)
on 2026-09-16. This is the first post-ACP-acceptance follow-up in
`docs/roadmap.md`; it does not widen S5 (stage/evidence integration) or delay
S7 (governed mission acceptance), defined in the
[ACP/gate implementation sequence](../../docs/scoping/acp-worker-gate-contract.md#delivery-slices-and-effort).

Reuse S5's stage bindings and retained evidence, `report_render.rs`,
`provenance.rs`, `evidence_bundle.rs`, and the existing pending-decision APIs.
The packet is a projection of authoritative records, not a new evidence store,
consent mechanism or generated summary that can manufacture passing claims.

**Out of scope**

New agent pools, specialist prompt orchestration, automatic risk downgrades,
autonomous merge, a second exporter, and terminal/editor or cloud-factory UI.

## Scoping answers

- Present approved scope and criteria, candidate identity and diff, required checks and their current receipts, independent findings, explicit waivers, unknowns, and the exact decision awaiting the operator. Every factual status links to its retained artifact or event; unavailable evidence stays visible.
- Show what changed since the preceding review. Group by the existing stage and policy requirements; do not create a model-selected risk score that can reduce mandatory review or change blocking/advisory semantics.
- Keep the human audit view separate from the fresh validator's input. The human may inspect the wider chain; validator inputs retain S5's restricted criteria, source, check receipts and independently produced findings. Worker reasoning, transcripts and persuasive summaries must not cross by linking the human packet, its underlying API or shared snapshot metadata.
- Reuse the same projection in the existing CLI/report and dashboard decision detail. Slack and other clients link to that decision and use its existing authority checks; no second decision state or Sgian orchestration is added.
- Preserve existing logs. Any additional persisted fields need an explicit `contractChangeRequest`, defaults and old-log fixtures. S5 remains the owner of evidence collection and consumption freshness checks.

## Acceptance hints

- A fixed mission fixture exposes all five review questions: approved scope, actual change, current checks, independent judgment and remaining human work. Each answer resolves to the expected authoritative record and candidate.
- An edit after a passing check makes that evidence visibly historical. A missing artifact, zero-test receipt, error or escalation never renders as a current pass, and clicking an obsolete decision cannot authorize new work.
- A packet with a deliberate scope deviation and unresolved independent finding surfaces both without requiring a transcript search or inventing a waiver.
- Verify actual validator inputs and reachable files cannot access a marked worker transcript placed only in the human audit view. Merely omitting a field from the prompt is insufficient isolation proof.
- Old logs render explicit unavailable fields; CLI and dashboard show the same decision identity. Reuse existing report/projection tests; run all workspace gates and, when changed, all dashboard gates with the embedded bundle synced.
