---
state: done
title: Reconcile ticket .status when a mission terminates outside the drain (kranz run / REST start)
priority: 3
schedule: once
---

## Goal
A mission that reaches a terminal state via `kranz run`, REST `/start`, or
any path other than `work::drain_queue` must still reconcile its ticket's
`.status` sidecar (Queued/Running → Done/Failed). Today only the drain writes
it: complete a mission manually and the ticket stays "failed" forever even
though the mission log says mission.completed. The reconcile must key off the
ticket's recorded `mission_id` and the mission's folded terminal status, and
preserve the DELIVERED/LANDED split (merged probe) exactly as the drain does.

## Context
Hit twice in one day (2026-07-19): m-9dc8c1 and m-0f1abd both COMPLETE but
their tickets rendered "failed" in the pipeline because they completed via
`kranz run` resumes after drain-time blocks. Both sidecars were hand-fixed.
The drain mapping lives in `crates/engine/src/work.rs`; the mission-terminal
paths (`orchestrator::run`, server `host::start`'s `run_to_end`) never call
it. Options: (a) hoist the terminal→ticket mapping into the engine so any
terminal event reconciles, or (b) a lazy reconcile on read (ticket list/show
folds the linked mission when state is Running and the mission is terminal).
(a) is authoritative; (b) is cheaper and self-healing. Also reconsider the
Blocked→"failed" mapping — a blocked mission is needs-input, not failed;
the pipeline already has a needs-you stage.

## Acceptance hints
- A ticket whose mission completes via `kranz run` (no drain) shows Done
  without hand-editing the sidecar; a mission that fails shows Failed.
- The blocked case no longer maps to "failed" (maps to needs-you or keeps
  Running with a blocked marker).
- cargo test --workspace green with a test proving reconcile-on-terminal
  outside the drain path.
