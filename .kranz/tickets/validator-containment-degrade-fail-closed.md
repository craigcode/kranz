---
title: Revisit validator containment's loud degrade: fail closed or make degrade opt-in
priority: 2
schedule: once
---

## Goal
Revisit the recorded operator decision (validator-mandatory-containment, shipped 224fa73) that uncontainable platforms/backends degrade loudly per spawn: the 14th-pass review shows the degrade reopens the modify->use->restore path the mandatory-containment work was built to close. Make containment fail closed when it cannot apply, or make the degrade an explicit per-repo opt-in (config flag, named at approve time) rather than the default posture. This reverses a D-X decision — the plan must record that explicitly. 14th-pass review finding.

## Context


## Scoping answers

## Acceptance hints
