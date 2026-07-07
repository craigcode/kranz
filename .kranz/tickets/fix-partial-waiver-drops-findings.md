---
title: Partial waiver silently drops unwaived findings (validation gate hole)
priority: 1
schedule: once
---

## Goal
convert_findings (orchestrator.rs ~2985) returns FindingsConversion::Waive
whenever fixFeatures is empty and waived is non-empty — WITHOUT checking
that the waivers cover EVERY finding. If validation returns 2 findings and
the model waives 1 and fixes 0, the other finding is neither fixed nor
preserved; the milestone/mission then completes (orchestrator.rs ~2822,
~3184) with an unresolved validation finding. This can falsely pass a
mission. Require every finding.subject to be covered (waived OR fixed)
before accepting Waive; for any finding not covered by a waiver, synthesize
a fix-feature (the fallback synth path already exists just below).

## Context
Found by a Codex code review 2026-07-07. Trust-critical: the whole product
guarantee is that the validation gate cannot be silently defeated; this is
one way it can. Fix + a regression test: a decision waiving a strict subset
of findings with zero fixFeatures must NOT return Waive — it must synthesize
fixes for the uncovered subjects (or fail closed).

## Acceptance hints
- A FixFeaturesDecision that waives only some findings (fixFeatures empty)
  yields fix-features for the uncovered findings, never a bare Waive.
- Test: 2 findings, waive 1 / fix 0 → conversion produces a fix-feature for
  the unwaived finding; 2 findings both waived → Waive.
- cargo test --workspace green.
