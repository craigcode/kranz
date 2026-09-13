---
title: Point the Gas City pack at the kranz-native queue instead of its private spool
priority: 2
schedule: once
state: done
state-note: "Native create-then-enqueue shipped behind KRANZ_NATIVE_QUEUE=1 on 2026-08-25: exec --enqueue creates an approved mission, work --once --expect drains only the intended front, and the pack retries failed City returns without mission replay. The private spool remains the documented rollback until the separate human-gated disposable-city receipt."
---

## Goal

Re-scope and then swap `packaging/gascity/` off the private `KRANZ_SPOOL`
waiting room onto kranz's already-shipped per-repo queue (`.kranz/queue/` +
`kranz work` / `POST /api/queue/drain`), without losing the City claim,
lease/reclaim, exit-code, or event-emit contracts.

## Context

`docs/gascity-citizenship.md` D5 called the `.env`-file spool a spike-era
stand-in and pointed at a kranz-native queue. Brief 2 of that plan scoped
*building* the queue; that work shipped (M2.75 + pipeline-view D-B). The
part Brief 2 explicitly deferred — pointing the pack at that queue — was
never ticketed.

This is a re-scope, not a rewrite. The two waiting rooms are not the same
shape:

- **Pack spool today:** `kranz-dispatch` claims a `kranz`-labelled bead,
  writes a ticket-shaped `mission.md` plus a sidecar `.env` into
  `KRANZ_SPOOL` (default `$GC_CITY/.gc/kranz-spool`). `kranz-city-worker`
  drains those `.env` files serially. `kranz-run-bead` runs
  `kranz exec -f mission.md` and maps exit 0/2/3/1 back to City state plus
  `kranz.mission.*` events.
- **Native queue today:** entries are `{mission_id, ticket_slug?, priority,
  seq}` under `.kranz/queue/`. `kranz ticket queue` enqueues an already
  planned Review mission. `kranz work` / serve drain *runs* that mission.
  There is no queue entry kind for "here is a raw brief, please `exec` it."

So `kranz ticket queue <slug>` is the wrong drop-in. A bead is a brief, not
a reviewed plan. Blindly forcing every bead through draft → Review → queue
would add a planning round-trip the current bridge does not take
(`kranz exec` plans and runs in one headless shot).

Design question the mission must answer in the ticket report before
changing scripts (pick one; do not invent a fourth waiting room):

1. **Create-then-enqueue.** Add a kranz-side "create mission from brief,
   enqueue, do not run" entry (split of `kranz exec`'s create/plan/approve
   from its run half). Dispatch writes that handle into the rig's
   `.kranz/queue/`. The City worker becomes `kranz work --once` (or serve
   drain) plus the existing City return-path.
2. **Keep `kranz exec`, replace only the waiting room.** Leave
   `kranz-run-bead` as the runner. Replace `$KRANZ_SPOOL/*.env` with a
   kranz-owned inbox that is *not* a new public execution primitive — only
   if an existing queue/API can carry an exec brief without a schema break.
   If it cannot, this option is closed; take (1).
3. **Do not use `kranz ticket queue` for unplanned beads.** That verb
   requires a parked Review plan. Out of scope as the production path.

Constraints (from the citizenship plan; do not weaken):

- No autonomous `gc init` / `gc start` / `gc pack release`. Stub-test City
  integration only (D6).
- Hold the opacity boundary (D1): no City-side LLM session.
- Keep the claim/lease/reclaim sweep. That is City-side first-wins
  (`gc bd update --claim` + client-side lease). The kranz queue's own
  dead-claim recovery does not replace it.
- Keep the exit-code contract and `kranz.mission.started/blocked/complete`
  emits. Those live in `kranz-run-bead` today; if the worker becomes
  `kranz work`, the return-path must still fire.
- Do not change the demo-night path without a flag. Tomorrow's live city
  still uses the spool until this ticket is validated stub-side and then
  human-gated against a disposable city.
- Kranz stays usable standalone. The pack remains optional.

## Scoping answers

Written 2026-08-17 (re-scope only; no pack cutover).

- **Waiting-room choice: (1) create-then-enqueue.** Option 2 is closed
  unless someone finds an existing queue/API that can carry a raw exec
  brief without a schema break — today's `QueueEntry` is
  `{mission_id, ticket_slug?, priority, seq}` only. Option 3
  (`kranz ticket queue` of an unplanned bead) is forbidden: that verb
  requires a parked Review plan; a bead is a brief. `kranz exec` already
  plans and runs in one headless shot. The missing kranz-side slice is
  a create/plan/approve-without-run entry so dispatch can enqueue a
  `mission_id` and `kranz work` / serve drain can run it.
- **City return-path:** stays with `kranz-run-bead` (or a thin wrapper
  around `kranz work --once`) — exit 0/2/3/1, `kranz.mission.*` emits,
  comments, lease heartbeat. Do not fold that into `kranz work` itself;
  City translation is pack-owned.
- **Flag / rollback:** keep `KRANZ_SPOOL` as the production waiting room
  until the exec split exists and stub tests pass. A
  `KRANZ_NATIVE_QUEUE=1` flag is acceptable for a later cutover; default
  off through any live-city demo. Do not change the demo-night path.

First implementation slice (when scheduled, not demo night): split
`kranz exec` so create/plan/approve can enqueue instead of run. Pack
scripts stay on the spool until that handle exists. Claim/lease/reclaim
stay City-side.

## Acceptance hints

- A written re-scope in this ticket (or the mission report) names option 1
  or 2 and why the other closed.
- `kranz-dispatch` no longer writes the production waiting room to
  `KRANZ_SPOOL` (a compatibility shim is fine if flagged and documented).
- Drain is `kranz work` / serve `POST /api/queue/drain` (or a thin wrapper
  that only adds the City return-path), not a private `.env` directory as
  the source of truth.
- Claim, lease, reclaim, exit-code mapping, and `kranz.mission.*` emits
  still hold under the existing stub tests.
- `gc lint packaging/gascity` still exits 0.
- No live city, no publish, no fleet work.
- Anti-vacuity: new tests match a unique filter (not a substring that
  already passes).
