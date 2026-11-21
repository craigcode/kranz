---
title: Stop a milestone's validation failing on assertions a later milestone delivers
priority: 1
schedule: once
state: done
state-note: Built by mission m-882080. Operator decision: an optional Assertion.milestone field naming a plan milestone title. Not-yet-due assertions run and are logged as validation.assertion.notdue; findings citing them are diverted to validation.finding.notdue. Finished as a pull request because the final gate cannot pass cargo test --workspace inside the fixed 600s cap and a fix feature id collided (tickets contract-command-timeout and fix-feature-id-collision).
---

## Goal

A validation round judges a milestone against the assertions that milestone is meant to satisfy. An assertion that only a later milestone can satisfy is not reported as a critical finding against an earlier one, and no fix feature is created for it.

## Context

Seen in a mission run on 2026-11-16. The plan had three milestones and twelve contract assertions. The validation contract is one flat list: an `Assertion` (`crates/engine/src/types.rs`) carries no milestone, and a plan milestone carries only a title and its features.

Each validation round for the first milestone ran the whole contract. Five command assertions tested behaviour that the second and third milestones were planned to deliver, so their test filters matched nothing yet and the commands failed. Each was recorded as a critical finding against the first milestone, with class `contract-command-failure`. The same five were reported again in the following round.

The effect was fix features and repair rounds spent on work that was already planned for later, and a first milestone that could not pass on its own terms. Related: ticket `harness-fault-findings-block-milestone` covers findings caused by the harness. This one covers findings caused by scope.

## Scoping answers

- Flag as a D-X operator decision how an assertion is tied to a milestone. Candidates: an optional field on `Assertion` naming the milestone that must satisfy it; an optional list of assertion ids on the milestone; or no schema change, with the round recording a not-yet-due assertion differently. Any schema change is additive with `#[serde(default)]` and must leave existing plans behaving as they do today.
- An assertion with no milestone named applies to every milestone, as now. This keeps the workspace-wide checks (format, lint, the full test suite) running each round.
- The final gate still runs every assertion. Scoping changes when an assertion is due, never whether it is checked.
- An assertion that is not yet due is still run and its result recorded, so a regression in already-delivered behaviour stays visible. It does not produce a critical finding or a fix feature before its milestone.
- How the planner learns to fill the field is prompt text. `prompts.rs` is a frozen surface (docs/knowledge/decisions/positioning-governance-evidence-layer.md). Flag it as D-X and keep any change to the plan schema description, not to how workers are instructed.
- Approval should refuse a plan whose assertion names a milestone that does not exist.
- The approval-time contract gates are unchanged.

## Acceptance hints

- A mission test with two milestones and an assertion due at the second: the first round records the failing result without a critical finding and creates no fix feature; the second round treats a failure as critical.
- A plan with no milestone on any assertion behaves exactly as today.
- The final gate fails on a not-satisfied assertion whatever its milestone.
- A plan naming an unknown milestone is refused at approval, with the assertion id in the message.
- Anti-vacuity: the test filter matches only the new tests.
