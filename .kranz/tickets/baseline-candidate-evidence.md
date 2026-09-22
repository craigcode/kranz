---
state-note: Implemented and reviewed in PR #74; merged at 0c29e9f29c0aa1e020dc104f1a5731c4c48f46ed. Delivered through a PR, not a recorded Complete mission.
state: done
title: Baseline and candidate evidence — prove the intended behavior changed
priority: 2
schedule: once
blocked-by: [acp-governed-mission-acceptance]
---

## Goal

Retain comparable baseline and candidate check receipts for explicitly selected
bug fixes or behavior changes, and distinguish an intended reproduction from
an unrelated failed command. Make the pair available through existing gate
inputs, review packets and evidence export.

## Context

The operator authorized this post-ACP-acceptance follow-up on 2026-09-16 after
reviewing [Vercel's evidence-chain example](https://vercel.com/blog/building-a-software-factory-for-ai-sdk).
It also applies the earlier brownfield characterization principle: record what
the original system actually does before treating the replacement as correct.

Start after S7 (governed mission acceptance) and reuse S5 (stage/evidence
integration), defined in the
[ACP/gate implementation sequence](../../docs/scoping/acp-worker-gate-contract.md#delivery-slices-and-effort).

[Critical assertion controls](../../docs/contract-controls.md) already run
approved valid/defective overlays at one pinned revision and retain bounded,
contained receipts with expected-failure matching. Reuse `ControlSpec`,
`CheckReceipt` and the runner in `contract_controls.rs`, together with S5's
frozen inputs, `validator_snapshot.rs` and `evidence_bundle.rs`.

The new work pairs observations from actual baseline and candidate revisions,
identifies any regression-check overlay separately, records environment
comparability and exposes that pair through existing gates/reports/exports.
Do not rebuild the control runner or its failure classification. Coordinate
with the existing `contract-readback-and-negative-controls` ticket: independent
semantic read-back remains in that ticket.

**Out of scope**

Automatic test generation, mutation search, generalized claim graphs, mandatory
red-before-green for every change, runtime cutover attestation, or live provider
calls without a separately approved workload and budget.

## Scoping answers

- Extend the existing control/evidence types additively for an opt-in pair across actual baseline and candidate revisions. Bind the approved claim, check and fixture identity, expected baseline/candidate outcomes, source identities, checker identity, environment and actual observations. Approve the expected failure before treating a run as evidence; a worker's retrospective claim does not establish that its error was the intended one.
- Execute approved check bytes through existing contained gate runners in disposable snapshots. A new regression test may need to be overlaid onto the base; identify those exact test bytes and keep that overlay separate from both the original source identity and the delivered candidate.
- Distinguish assertion failure for the declared behavior from setup failure, missing credentials, unavailable services, compilation failure, zero selected tests, timeout or an unparseable result. A declared compile-time/API-shape assertion may use an exact expected diagnostic with an appropriate positive control; an arbitrary compiler error is not a successful reproduction.
- Pin check inputs and capture labels for environment differences. A change to the test, candidate, relevant baseline or environment invalidates a claimed comparable pair; historical observations remain historical.
- Preserve current check policy. Do not require every test to fail on the base: characterization, parity and already-valid invariants can legitimately pass there. New blocking requirements require explicit approved contract/policy. Persisted changes follow an additive `contractChangeRequest` and old-log tests.

## Acceptance hints

- A deterministic bug fixture fails on the base for the intended assertion and passes on the candidate using the same approved regression check. Export retains both source identities, overlay identity, environments and receipts.
- Missing dependency/auth, unrelated compilation errors and zero-test runs are inconclusive; none can satisfy the intended-failure requirement.
- A broken regression test that fails against both valid and defective controls cannot establish a useful pair. Cover a declared compile-time assertion too.
- Changed test bytes or a later candidate edit invalidate reuse. Different environments remain labelled and do not silently count as equivalent.
- Existing parity/invariant checks that pass on the base remain valid under their own approved expectations. No baseline/candidate run changes the real checkout, delivered branch or pinned check definitions.
- Missing retained evidence stays unresolved during replay/export. Use the existing gate runner and report paths; run all workspace gates for implementation.
