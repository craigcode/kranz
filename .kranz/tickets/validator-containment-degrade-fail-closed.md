---
state: done
state-note: "Done: validator containment now FAILS CLOSED on an uncontainable platform/backend (EngineError::Config naming the platform/backend and the remedy); the additive validatorAllowUncontainedDegrade config field (serde default false) opts a repo back into the old loud per-round degrade. Reverses the recorded 224fa73 D-X decision — documented in AGENTS.md rule 10 and docs/config-composition.md. validator_containment filter: 16+ green incl. the fail-closed/opt-in matrix; full workspace gates green."
title: Revisit validator containment's loud degrade: fail closed or make degrade opt-in
priority: 2
schedule: once
---

## Goal
Revisit the recorded operator decision (validator-mandatory-containment, shipped 224fa73) that uncontainable platforms/backends degrade loudly per spawn: the 14th-pass review shows the degrade reopens the modify->use->restore path the mandatory-containment work was built to close. Make containment fail closed when it cannot apply, or make the degrade an explicit per-repo opt-in (config flag, named at approve time) rather than the default posture. This reverses a D-X decision — the plan must record that explicitly. 14th-pass review finding.

## Context


## Scoping answers

## Acceptance hints
