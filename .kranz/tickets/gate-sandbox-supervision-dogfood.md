---
title: "Gate-sandbox supervision policy for self-testing (dogfood unblock)"
priority: 2
schedule: once
---

# Gate-sandbox supervision policy for self-testing

Source: 13th-pass review (2026-08-03, P2) — `command_exec.rs` ~2065
(documents 11 failures when kranz's own engine suite runs inside the gate
wrapper: cross-process SIGKILL, `kill(pid,0)` liveness probes, `ps`
identity reads, and nested sandbox operations are all denied under
`(allow signal (target self))`). Consequence: a kranz mission using
process enforcement cannot satisfy this repo's mandatory
`cargo test --workspace` contract — dogfooding with enforcement on is
blocked.

## Problem

The wrapped gate's supervision policy is session-parity: signals to self
only, no process inspection. The engine's own test suite legitimately
spawns and supervises children (the exact machinery the wrap tests).
Widening the profile globally would defeat the wrap's purpose; not
widening it blocks the dogfood loop that produces most of this repo's
validation evidence.

## Design (direction)

A gate-SPECIFIC supervision policy, not a global widening: the wrapped
gate may signal and inspect processes INSIDE its own sandboxed process
tree (descendants only), never arbitrary host processes. Options to
evaluate: Seatbelt signal target = same-sandbox (not just same-pid), a
proclaimed `ps`-read allowance scoped to the tree, or the container
boundary (container-gate-wrapper) where the tree question is structural.
Whatever the mechanism, the policy must stay hereditary (a wrapped gate's
own wrapped children inherit the same posture).

## Test gate

- `cargo test --workspace` runs green as a wrapped contract command on
  this repo (the 11 self-referential failures go to zero) — as a
  fixture, not a one-off manual run.
- No new host-wide signal/inspect capability (prove a probe against an
  unrelated host process still fails).
- Workspace gates green, bare exit codes, never piped.

## Out of scope

The gate wrap itself (landed), container gate wrapper (its own ticket —
noted as the alternative boundary), validator containment.
