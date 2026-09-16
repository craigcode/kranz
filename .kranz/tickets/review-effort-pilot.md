---
state: open
title: Review-effort pilot — measure evidence gaps and human work on bounded missions
priority: 3
schedule: once
blocked-by: [gate-review-packet, baseline-candidate-evidence, mission-outcome-reasons]
---

## Goal

Run a small, reproducible review pilot after the evidence follow-ups land and
publish an honest assessment of the human effort, missing evidence and defects
observed. Use the results to decide which further product work is justified.

## Context

The operator authorized scheduling this pilot on 2026-09-16 following the
[Vercel factory discussion](https://vercel.com/blog/building-a-software-factory-for-ai-sdk).
It follows the three implementation tickets above and S7; it does not replace
S7's synthetic acceptance or add a release requirement to the current ACP work.

Reuse mission reports, gate review packets, baseline/candidate receipts, outcomes
and portable exports. Existing grant latency is elapsed waiting time, not a
measurement of active review or proof of careful scrutiny. Keep the pilot record
as a reviewed artifact initially; collect evidence before adding telemetry code.

**Out of scope**

Mandatory attention tracking, employee scoring, a new analytics service, factory
orchestration, Sgian implementation, automatic policy relaxation and unsupervised
live-provider spending.

## Scoping answers

- Predeclare a small sample covering a documentation change, a bug reproduction
  and fix, and a bounded brownfield change with characterization or parity.
  Record selection criteria, task/risk class, repository/runtime identities,
  reviewer identity, review method and known differences between cases.
- Record evidence the reviewer had to collect manually, questions and repeated
  review/repair rounds, human interventions, and separately measured active
  review time where available. Keep queue/approval waiting time separate.
  Missing time or unobserved interventions remain unavailable, not zero.
- Record seeded defects missed or caught and any observed escaped defects with
  a declared observation period. A short clean pilot is not proof of safety or
  an estimate of the unseen defect population. Use descriptive results with
  explicit sample sizes; make no causal productivity claim from unlike tasks.
- Preserve the human audit/validator-input distinction and actual containment.
  Begin with deterministic fixtures and a reviewer exercise. Prepare the exact
  adapters, workloads, environments and cost ceiling for any separately approved
  live-provider portion; scheduling this ticket grants no open-ended spending.
- Deliver a concise review artifact linking retained receipts and identifying
  recurring gaps, existing mechanisms that suffice, and specific follow-up
  recommendations. New work requires review rather than automatic ticket fan-out.

## Acceptance hints

- The pilot protocol and case list exist before execution, and every reported
  measurement has a definition, source and sample count. Active review and waiting
  time cannot be substituted for each other; unavailable measurements stay absent.
- A case with deliberately stale/missing evidence makes the omission visible,
  and a baseline setup failure is not credited as a reproduced defect.
- A marked worker transcript remains outside actual fresh-validator inputs;
  the human can still inspect the permitted evidence chain.
- Another operator can reconstruct the decisions from the exported records.
  Report code delivery separately from release/cutover and label fixture versus
  live observations and any incomplete case.
- The final artifact assesses correctness, readability, architecture, security
  and performance implications; it does not claim a general efficiency gain
  without a suitable comparison. Validate links, receipt hashes and applicable
  workspace/dashboard gates if implementation changes prove necessary.
