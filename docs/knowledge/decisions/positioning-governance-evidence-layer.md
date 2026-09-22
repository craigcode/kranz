---
title: Positioning — kranz is a governance and evidence layer
owner: operator
freshness: check-on-touch
last_verified: 2026-09-22
verified_against:
  - docs/roadmap.md
  - docs/scoping/governance-evidence-layer.md
  - docs/scoping/flight-rules-engineering-standards.md
  - crates/engine/src/backend.rs
  - crates/engine/src/outcomes.rs
  - crates/engine/src/trace_export.rs
  - crates/cli/src/ready.rs
---

Rechecked 2026-09-22 (UTC) against the evidence-stream roadmap and
[review-effort pilot protocol](../../scoping/review-effort-pilot.md).
Packets, baseline/candidate comparisons and outcome reasons remain read-only
evidence surfaces. The pilot records manual review observations; it adds no
execution or skill-capture mechanism.

## What this records

A strategic decision about what kranz is, made 2026-07-29. Work that
contradicts it should be flagged against this ADR, not reconciled quietly.

Rechecked 2026-09-20 (UTC) against the
[ACP/gate integration scope](../../scoping/acp-worker-gate-contract.md) and
[v1 contract](../../gate-evaluation-contract.md).
Its consent, external-check and evidence work uses the shipped dispatch seams;
it adds no code-generation pool, prompt optimizer or cross-feature context reuse.
The v0.3.0 integration completes S5 stage evidence and the bounded S6/S7
acceptance scope. Qualified profiles bind credentials and containment through
the existing dispatch seam. Explicit platform limits, unchanged defaults and
release gates remain; no execution-factory or prompt-optimization primitive is added.

Rechecked 2026-09-16 (UTC) against the
[review/evidence follow-ups](../../roadmap.md#scheduled-follow-up--review-efficiency-and-evidence-2026-09-16).
Review packets, comparable receipts, outcome reporting and a bounded review
pilot extend governance and evidence; they add no code-generation orchestration
or autonomous remediation.

**Kranz is a governance and evidence layer for agent work. It is not an
execution harness. One line: kranz dispatches, gates, records and proves.
It does not write code.**

## Reasoning

- **Execution is commoditising.** Headless agent CLIs (Claude Code first
  among them) ship deterministic lifecycle hooks, isolated subagents, skills
  and plugins, and gain capability weekly with zero effort from this
  project. Competing on orchestration-for-better-codegen is a race kranz
  loses on velocity — Factory, Anthropic, Cursor, and Amp all staff that
  surface (see the roadmap's external-scan pattern notes and
  docs/reviews/ampcode.md).
- **Model hosting and fine-tuning are backends, not competitors.** Hosted
  frontier, hosted fine-tune, and a local tier all sit behind one routing
  seam (KRZ-331).
- **What is not commoditised is the layer above**, and kranz already has
  most of it: an append-only event-sourced log, cost per event, a
  grant/consent model with escalation, secret-scan-at-write, and outcome
  folds that can answer *why did this change pass, and who or what decided
  it* months after the fact. That answer — provenance — is the single most
  defensible thing kranz does.
- **The wedge is contract-style validation** (KRZ-327): a real, evidenced
  defect class (vacuous/miswired contract assertions) that competing
  framings do not address. Push it while it is uncontested.

## Ownership by layer

| Layer | Owner |
|---|---|
| Writing the code | External agent CLIs (plus heterogeneous peers for decorrelation) |
| Model hosting, routing, fine-tune | Backend providers behind the routing seam |
| Dispatch, gates, escalation, cost, provenance, evidence | **Kranz** |
| Domain knowledge | Packs — private, per-consumer, behind the declared contract (KRZ-313) |

## What kranz will not build (frozen)

New in-harness execution primitives: worker pools beyond the shipped M3
machinery, prompt routing sophistication, context-management features, or
anything else whose purpose is to make an agent write better code. Also
frozen (standing non-goals, restated): terminal replacement, editor/LSP
shells, a general agentic IDE.

**Boundary gloss (2026-07-31):** the freeze covers pools whose claimed
value is code velocity or quality. Heterogeneous dispatch
(`heterogeneous-dispatch-pool.md`, KRZ-303) is NOT such a pool — it is a
retained evidence primitive, provided it keeps three properties: outputs
are candidates for judgement (never auto-merged into a winner), the
claimed value is divergence for scrutiny (never throughput), and the
cost multiplier is explicit in the consent surface. Any future use of an
N-backend fan-out for codegen velocity falls under the freeze.

## What is retained, and why it is not "execution"

- **Worktree isolation and the gated merge** (M3/M7): this is how kranz
  delivers an *audited* diff. It bounds and judges execution; it does not
  perform it.
- **The AgentBackend seam and its implementations**: dispatch requires
  adapters. Adapters translate; they do not orchestrate codegen.
- **Planning and plan approval**: the plan is a consent artifact, not an
  execution feature.
- **Flight Rules engineering standards** (M5.5): deterministic selection,
  consent pinning, gate binding, human exception authority, and policy
  evidence are governance primitives, not context-management work. The generic
  lifecycle/resolver belongs in core; actual house rules and rationale remain
  in packs. Design: `docs/scoping/flight-rules-engineering-standards.md`.
- **Local-inference tier**: a backend behind KRZ-331, kept because routing
  and cost governance need a cheap tier — not because kranz hosts models.

## The IP boundary (load-bearing)

Kranz core is domain-free — no customer names, legacy-platform vocabulary,
or customer schema identifiers anywhere in core, enforced mechanically by
the clean-room lint (KRZ-314). Domain knowledge lives in private packs
consumed through the pack contract (KRZ-313). Test for placement: would this
exist in kranz if that specific consumer did not?

## Consequences

- The ACP worker backend is promoted from stretch to critical path
  (`acp-worker-backend`): if kranz dispatches rather than executes, that
  seam is the product.
- The local-inference workstream folds into the backend-routing abstraction
  (`backend-routing-abstraction`) as implementation slices.
- The evidence spine (`outcomes-report-task-class`,
  `escalation-ledger-export`, `provenance-replay`) is the product surface to
  finish first — most of its substrate already exists. Its export writers are
  held to the same discipline as the rest of the layer:
  `trace_export::write_export_output` backs both `export-traces --out` and
  `export-corpus --out`, pinning the parent chain no-follow, refusing a
  symlinked destination, and landing bytes through a rename, so an operator's
  own export cannot be steered into overwriting a file an agent planted a link
  at (the 2026-09-01 adversarial audit).
- Roadmap tension, named: backend_cursor survives re-framed as a dispatch
  adapter under the routing seam; further M3-style parallel-execution
  investment and execution-side continuous-UX items are deprioritised
  unless they serve the governance surface. Series map and sequencing:
  `docs/scoping/governance-evidence-layer.md`.

## Revisit triggers

- A dispatch target's hook/permission surface becomes too weak to project
  gates onto (would force execution features back in-harness).
- The pack contract proves too weak for a real consumer without
  domain-shaped special cases in core — that is a contract redesign signal,
  never a licence to leak domain knowledge into core.
