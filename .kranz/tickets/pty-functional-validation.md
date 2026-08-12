---
state: done
state-note: Done: pty_harness.rs (libc openpty, no new deps) + AssertionCheck::PtyScript (additive) + validation.pty.transcript events with resolvable file: artifacts; validation-round engine-run under the gate-sandbox posture; skip-loud off unix; post-kill drain-while-reaping fix. pty_validation filter: 6+1 green; dashboard chain green; full gates green.
title: Pty-driven functional validation for terminal-interactive targets
priority: 3
schedule: once
---

## Goal
Functional-QA validators can drive terminal-interactive targets — REPLs,
TUIs, interactive CLIs — through a pty harness: scripted input,
assertions on screen/output state, and the session transcript captured as
a validation artifact.

## Context
Extends the shipped M5 functional-QA lane (browser/computer-use) to
terminal-native targets, closing a gap for repos whose deliverable IS a
TUI or interactive CLI. Feasibility precedent from the Warp scan
(2026-08-04): their pty-mux architecture drives full-screen apps (vim,
gdb, REPLs) reliably — kranz borrows the validation-side capability only.
This is validator tooling, not an execution feature: validators judge
what the delivered software does (positioning ADR's retained list). Same
prerequisite as browser QA: the target repo declares a scriptable run
harness (workspace contract services/readiness). Pty sessions run under
the same sandbox policy as the validator that owns them.

## Acceptance hints
- A fixture interactive target (simple REPL) is driven through scripted
  input; a correct target passes and a seeded-defect variant fails, each
  with the assertion named.
- The pty transcript lands as a validation artifact referenced from
  events.
- Targets with no declared run harness skip cleanly (today's behavior).
- Anti-vacuity grep on a named filter unique to this work.
