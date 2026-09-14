---
state: open
title: Review memory — scoped rules must pass replay before promotion
priority: 3
schedule: once
---

## Goal

When feedback-derived review memory is introduced into a Kranz gate, treat
each memory update as a reviewed change to reviewer behavior. Capture
accept/reject/edit feedback as evidence for scoped candidate rules, and
require a fixed replay suite plus explicit human approval before those rules
can influence subsequent gated reviews.

## Context

Capture this requirement for a concrete consumer of review memory; it does
not make a standalone learning reviewer the next implementation priority.
The work belongs to governed rule promotion and evidence under
`docs/knowledge/decisions/positioning-governance-evidence-layer.md`. Use the
existing Flight Rules lifecycle, approval, and evidence interfaces.

Inspired by [Sumanth P's review-memory demo and Karl-Johan Spiik's reply](https://www.linkedin.com/posts/sumanth077_i-built-a-self-evolving-code-review-agent-share-7501280257711230976-b-7l/)
and the [implementation](https://github.com/Sumanth077/Hands-On-AI-Engineering/tree/main/ai_agents/self_evolving_code_review_agent).
The useful shape is accept/reject/edit feedback distilled into scoped rules.
Spiik identifies the missing control: replay known diffs with findings that
should and should not appear, and reject memory updates that suppress real
findings. A falling comment-rejection rate alone cannot distinguish useful
adaptation from a reviewer that stops finding defects.

Related: `contract-readback-and-negative-controls`,
`flight-rules-finding-provenance`, `flight-rules-waiver-decisions`, and
`evidence-bundle-export`. Contract read-back examines what an acceptance
check establishes; this ticket evaluates changes to reviewer guidance.

**Scope**

- Preserve the original finding, human accept/reject/edit decision, edited
  wording, rationale, actor, source review/code revision, and applicable
  scope. Distinguish an incorrect finding from a duplicate, an existing
  mitigation, an out-of-scope issue, or an explicitly accepted risk.
  Ambiguous feedback stays evidence without automatically creating a rule.
- Produce a reviewable candidate with its rationale, scope, source feedback,
  and immutable revision. Accepting or editing one comment does not approve
  a standing rule. Reuse Flight Rules supersession and exception semantics;
  feedback must not silently weaken an engine floor or broaden a waiver.
- Keep a versioned, human-reviewed replay set of diffs with required findings
  and findings that must not appear. Identify the behavior and evidence each
  expectation concerns, allowing equivalent wording. Candidate rules cannot
  rewrite their own evaluation expectations; suite changes require separate
  review and a new version.
- Evaluate the current approved guidance and the candidate against the same
  replay inputs. Pin the effective memory/rule set, model/backend, prompts,
  retrieval configuration, and evaluation policy. Exercise the actual review
  input path so unrelated or out-of-scope memory cannot escape the check.
  Keep evaluation feedback out of active memory.
- Refuse promotion if the candidate misses a required finding or emits a
  prohibited finding. Empty suites, missing cases, execution/setup errors,
  timeouts, and unparseable results are unresolved, never passing evidence.
  Declare any repeated-run policy in advance; do not select a favorable run
  after seeing failures. Replay establishes evidence for the recorded cases,
  not a general guarantee of review correctness.
- Require explicit authorized-human approval of the exact passing candidate
  and receipt before activation. A rule, suite, or evaluation-input change
  makes the receipt stale. Failed, stale, or unapproved candidates leave the
  active version unchanged. Preserve the prior version for an audited rollback.
- Record promotion, refusal, and rollback evidence through the existing
  append-only event and evidence-bundle paths. Each affected review identifies
  the effective rule/memory version. Preserve legacy logs/configs; persisted
  schema additions require a deliberate `contractChangeRequest`.

**Out of scope**

Model retraining, autonomous prompt optimization, a new general review agent,
new worker pools, a mandatory vector database, and bulk mandatory approval of
every generated comment. Collect feedback during existing finding resolution.

## Acceptance hints

- Authorization fixture: reject a missing-authentication comment on a route already protected by middleware. A narrowly scoped candidate stops that false positive while still finding a genuinely unprotected route and a separate missing resource-ownership check.
- An overbroad candidate that suppresses authorization findings fails replay and never becomes active. A reviewer returning no findings also fails the required-finding cases, regardless of its comment-rejection metric.
- Edited-comment feedback retains both versions, rationale, and scope. Neither comment acceptance nor a successful replay alone activates its derived rule.
- An injected out-of-scope rule cannot affect the review; test the actual retrieved inputs and the version identity recorded with the verdict.
- Missing/empty/inconclusive replay evidence and changed candidate, corpus, or evaluation inputs prevent promotion. Failed promotion and rollback retain the prior version and their receipts without rewriting review history.
- Use a fixture backend to verify enforcement and persistence deterministically. Retain a separately reviewed real-model replay example to demonstrate the behavior; mocked model output alone does not establish review quality.
- Export source feedback, candidate identity, suite/evaluation identities, per-case outcomes, and operator disposition with existing redaction rules.
- Use the unique `review_memory_promotion_` test prefix (no existing matches when authored), nonzero-test guards in drafted contract commands, and all workspace gates before implementation is declared complete.
