---
title: Egress grants — sandbox egress-denial instrumentation (prerequisite)
priority: 3
schedule: once
---

## Why this is blocked (feasibility finding 2026-07-13)

Egress grants would extend the grant-request decision flow to sandbox egress
denials: a worker/validator blocked from reaching a host parks for an operator
approve/deny → extend the mission's egress allowlist. The park/approve/deny
machinery already exists (see the shipped `GrantKind` flow — command + touch
grants). The BLOCKER is the trigger: there is no observable egress-denial
signal to park on.

`crates/engine/src/sandbox.rs` uses OS-level containment (macOS Seatbelt SBPL
`network-outbound` allowlist; Linux bubblewrap). A blocked connection is a
kernel-level failure delivered to the sandboxed process as a generic connection
error — NOT a structured event carrying the destination host back to the
engine's event stream. Unlike command denials (a `tool_result` the runner
correlates) or touch-set writes (the out-of-contract sweep names the path),
nothing tells the engine "egress to `<host>` was denied." So the grant flow has
no signal to trigger on, and can't name the destination the operator would
grant.

## What this ticket needs FIRST (the actual work)

Instrument the sandbox to emit a per-destination egress-denial signal the engine
can attribute to a run:
- Investigate whether Seatbelt / bubblewrap can log denied outbound connections
  with the destination (Seatbelt has `(deny network-outbound (with report))`
  style reporting; bubblewrap has no native per-host deny logging — may need a
  userspace proxy or eBPF). Feasibility is genuinely uncertain, especially on
  Linux.
- If feasible: parse those denials into a structured signal (host + run id),
  surface it on `RunOutcome` (mirror `denied_commands`), and add an
  `egress` `GrantKind` that extends the mission's egress allowlist on approval.

Only once the emission exists is the grant flow itself a small addition (a third
`GrantKind`, reusing park/approve/deny/timeout/cap and all four surfaces).

Deferred from the grant-request-decision-flow work (see
`grant-request-decision-flow.md`), where command + touch-set grants shipped and
egress was scoped out as blocked-on-infra.
