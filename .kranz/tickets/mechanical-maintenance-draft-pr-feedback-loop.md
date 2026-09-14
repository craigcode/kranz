---
state: open
title: Mechanical maintenance — draft PRs, observed dead code, reviewed routine fixes
priority: 3
schedule: once
---

## Goal

Define a governed workflow for recurring mechanical maintenance that produces
reviewable draft pull requests. Keep the human merge gate, observe suspected
dead code before proposing deletion, and turn incorrect PRs into reviewed
improvements to the routine that produced them.

## Context

Source: [LinkedIn post supplied by the operator](https://lnkd.in/p/gPwj6qzB).
The operator's takeaways are: draft PRs only, no unattended merge; dead-code
logs first and deletion proposals the next day; fix the routine when its PR is
wrong; keep mechanical maintenance loops separate from feature work. This
ticket captures those takeaways without attributing additional claims to the
post.

This belongs to Kranz's governance and evidence layer. Reuse existing
dispatch, worktree isolation, approval, validation, scheduling integrations,
and outcome evidence. Repository-specific routines live in reviewed packs or
workflow definitions, consistent with
`docs/knowledge/decisions/positioning-governance-evidence-layer.md`.

Related: `review-memory-rule-promotion-replay-gate`,
`contract-readback-and-negative-controls`, and `outcomes-report-task-class`.
Review-memory promotion governs learned review guidance; this ticket governs
maintenance routines and the changes they propose.

**Scope**

- Define a bounded mechanical-maintenance class, with explicit allowed
  transformations, paths, cadence, validation, cost limits, and routine
  revision. Formatting and reproducible generated-file drift are examples;
  new behavior, product decisions, and architectural changes remain feature
  work. A proposal that crosses its declared class or scope stops for review.
- An explicitly authorized external publisher may turn the reviewed mission
  output into a draft PR. This routine uses the engine's default local-ref
  workflow; the separate human-invoked cloud handoff grants it no push authority.
  Recurrence grants permission to run the approved routine, not permission to
  merge. Successful validation does not enable auto-merge, mark the PR ready
  for review automatically, or grant broader repository authority.
- Split dead-code work into two runs. The first records candidate identities,
  code fingerprints, reference/usage evidence, timestamps, and known coverage
  gaps in an auditable observation log; it makes no deletion. A later run,
  at least 24 hours after that observation, may propose deletion in a draft
  PR after checking the evidence again against the current base. Treat this
  as the default interpretation of "next day," not a midnight boundary.
- A candidate's changed code, new reference, observed use, or insufficient
  evidence prevents automatic inclusion in a deletion proposal. Account for
  dynamic imports, reflection, registrations, externally consumed APIs, and
  infrequent paths. A quiet observation window alone is not proof of dead
  code. Retain uncertainty and the reason for withholding a candidate.
- When a PR is wrong, retain the rejected proposal, human disposition, and
  concrete reproducer. Propose a correction to the routine's selection,
  scope, instructions, or checks, plus a regression case demonstrating the
  error and a positive case preserving useful maintenance. Correcting only
  the generated diff does not close the routine defect.
- Treat routine changes as versioned, reviewable changes. Keep fixed replay
  expectations independent of the candidate correction; require passing
  evidence and explicit human approval before activating the new revision.
  A rejected PR does not silently become a standing rule, and a routine that
  stops proposing all changes must not pass by suppressing useful findings.
  Reuse the review-memory promotion gate where guidance is involved.
- Keep maintenance selection, budgets, cadence, and reporting distinct from
  feature missions. Avoid overlapping work on the same paths and duplicate
  open PRs for the same routine/candidate. Stale or conflicting proposals
  require a fresh base and validation. A maintenance run must not acquire
  feature scope or consume a feature run's grant implicitly.
- Preserve append-only evidence linking observation, base revision, routine
  revision, validation, draft PR, human feedback, proposed routine correction,
  and its eventual activation or refusal. Use existing event/export paths;
  any persisted schema additions require an explicit `contractChangeRequest`.

**Out of scope**

Unattended merge, automatic deletion on the primary branch, autonomous prompt
optimization, a new scheduler or worker pool, general feature generation,
and weakening validation or secret-scan policy to make maintenance pass.

## Acceptance hints

- A successful maintenance run produces a draft PR through an authorized external publisher; neither initial success, retries, nor routine recurrence can invoke merge or automatically mark the PR ready.
- A dead-code candidate first appears as observation evidence with no deletion. A run before 24 hours cannot propose its removal; a later run with sufficient unchanged evidence may create a deletion draft for human review.
- A newly introduced reference, changed candidate fingerprint, observed runtime use, or unresolved dynamic-use case withholds the deletion and records why. Missing observations and a merely quiet log never count as proof.
- A rejected false-positive PR yields a routine-correction proposal and a fixed regression case. Replay still proposes a genuinely safe maintenance change, so suppressing all output cannot pass. Neither rejection nor passing replay alone activates the correction.
- The next run identifies the human-approved routine revision it used. Rejected or stale corrections leave the prior approved revision and its evidence intact; rollback is explicit and auditable.
- A maintenance proposal that includes feature behavior, exceeds its path/budget grant, overlaps an active feature mission, or duplicates an open proposal is refused or deferred with a recorded reason.
- Verify the workflow with deterministic fixtures for the publisher, clock, repository references, and feedback. Drafted contract commands must assert nonzero relevant test counts; run all workspace gates for implementation changes.
