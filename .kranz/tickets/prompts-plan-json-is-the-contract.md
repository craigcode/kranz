---
state: done
title: Make plan.json-not-plan.md authoritative in worker/orchestrator prompts
priority: 3
schedule: once
---

## Goal
State explicitly in the worker and orchestrator prompts
(`crates/engine/prompts/*.md`) that the validation contract lives ONLY in
the approved plan.json folded from `plan.approved`: editing plan.md does
not change the gate, assertions cannot be edited by any agent, and a
suspected contract-authoring bug must be escalated to the operator (blocked
+ message) rather than "fixed" by editing plan text.

## Context
In m-0c885b, two fix features spent the entire fix-cycle cap editing
plan.md's assertion text (commits 189f992, bf2b532, f7e22e2) before the
orchestrator concluded the gate reads plan.json — an expensive, doomed
repair of the wrong file. The prompts never say where the contract lives or
what to do when an assertion looks buggy, so the model guessed. One
paragraph in the role prompts closes it; the material fact (final gate
reads the folded approved plan) is already true in code.

## Acceptance hints
- worker.md and orchestrator.md both state: contract = approved plan.json;
  plan.md edits never affect gating; buggy-looking assertion → escalate to
  operator, never repair.
- prompts.rs hash tests updated; a prompt-content test asserts the rule
  text exists in both roles.
- cargo test --workspace green.
