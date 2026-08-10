---
title: Revive Gas City integration for a demo
priority: 1
schedule: once
state: open
---

## Goal
Get the Gas City pack (`packaging/gascity/`) demo-ready for a live training-course city next week: make dispatch event-driven and surface kranz mission progress as City events, while keeping the existing cooldown-era scripts and exit-code contract unchanged.

## Context
`docs/gascity-citizenship.md` is the plan of record. The pack is currently stub-verified and parked (docs/gascity.md). Two small, autonomous code changes from the citizenship roadmap are enough for a demo:

- **Brief 1 / Stage 2:** switch `orders/kranz-dispatch.toml` from `trigger = "cooldown"`/`interval = "5m"` to an `event` trigger on `bead.created`/`bead.ready` for the `kranz` label.
- **Brief 3 / Stage 3:** emit `kranz.mission.started`, `kranz.mission.blocked`, and `kranz.mission.complete` events from `bin/kranz-run-bead` at the points its exit-code mapping already branches (0/2/1/3).

The actual live-city validation (Stage 1) is intentionally human-gated: the operator will run it at the training course with a disposable city. This ticket does **not** require a live city or any state-mutating `gc` command.

## Acceptance hints
- `orders/kranz-dispatch.toml` uses `trigger = "event"` and `on = "bead.created"` (or `bead.ready`) scoped to the `kranz` label, with no cooldown/interval keys.
- `bin/kranz-run-bead` calls `gc event emit kranz.mission.started/blocked/complete` with `--subject "$ID"` and a short `--message` at the right branches.
- `gc lint packaging/gascity` still exits 0.
- Existing bridge tests still pass; new stub/mock tests prove the event emit calls fire with the expected arguments.
- Docs updated so `docs/gascity.md` no longer says the integration is "parked" — it is demo-ready pending live validation.
