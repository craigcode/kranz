---
title: Stage 5 — Gas City fleets and cross-machine execution (parked)
priority: 3
schedule: once
state: open
---

## Goal

Hold a parked ticket for Stage 5 of `docs/gascity-citizenship.md` so the
citizenship plan no longer names unpublished work. Do not implement
anything until the trigger is real.

## Context

Stage 5 is explicitly speculative. The spike named two futures:

- **Heterogeneous fleets** — land `gc sling` and re-evaluate `gc mcp`,
  both deferred until a city actually routes more than one agent type.
  First real work item at that point: scope `kranz-worker` as a `gc sling`
  target.
- **Cross-machine execution** — City k8s (or similar) runtimes behind
  kranz's `AgentBackend` seam. No mechanism in the assessment is this
  trigger; do not invent one.

Rejected regardless of fleet size (opacity boundary, D1): `gc handoff`,
`gc nudge`, `gc session`. Those need a named interactive City-side
session. Do not reopen them here.

kranz already has a heterogeneous *dispatch pool* (ticket
`heterogeneous-dispatch-pool`, landed). That is an in-harness evidence
primitive (N backends on one unit of work). It is not a City fleet and
does not satisfy this trigger.

## Scoping answers

- Heterogeneous fleet actually assembled (kranz + at least one other
  agent type under one City router):
- Cross-machine runtime actually offered behind `AgentBackend`:

## Acceptance hints

- No code lands on this ticket until one of the two triggers above is
  filled in with a real city/runtime, not a hypothetical.
- When the fleet trigger fires, the first mission is a scoping note for
  `kranz-worker` as a `gc sling` target (~existing pack agent), not a
  sling implementation.
- When the cross-machine trigger fires, the first mission is a scoping
  note against the current `AgentBackend` seam, not a k8s provider.
- `gc handoff` / `gc nudge` / `gc session` stay rejected.
- Until then this ticket stays `open` and unqueued.
