---
title: Mission pipeline & event-sourced core
owner: agent
freshness: check-on-touch
last_verified: 2026-08-24
verified_against:
  - crates/engine/src/reducer.rs
  - crates/engine/src/events.rs
  - crates/engine/src/types.rs
  - crates/engine/src/event_log.rs
  - crates/engine/src/orchestrator.rs
  - crates/engine/src/queue.rs
  - crates/engine/src/merge.rs
  - crates/engine/src/paths.rs
  - crates/engine/tests/reducer_test.rs
  - docs/design.md
---

## The one invariant

`events.jsonl` (append-only JSONL, one `Event` per line) is the sole source of
truth. Everything else — `MissionState`, `state.json`, the dashboard — is
derived by folding that log. If you change how state is computed, change the
[reducer](../../../crates/engine/src/reducer.rs), never the on-disk state; a
reader can always rebuild. See [`crates/engine/src/paths.rs`](../../../crates/engine/src/paths.rs)
for the per-mission directory layout (`.kranz/missions/<id>/`).

## Log rules (event_log.rs)

- **Single writer.** One engine process owns the log, guarded by a lock *file*
  `events.jsonl.lock` (not POSIX advisory locks, so Windows stays first-class),
  acquired via `EventLog::acquire`. A dead holder is detected by `kill(pid,0)`
  plus a process-identity token that proves pid reuse; a live/unknown holder is
  honored unless an explicit `LockForce` tier steals it. Two engines on one log
  is the cardinal sin the whole liveness dance exists to prevent.
- **Contiguous seq.** `seq` starts at 1 and increases by exactly 1 (assigned by
  the writer). `parse_log` refuses any gap/duplicate as `LogCorruption`. Only an
  unparseable *final* line is tolerated (torn write) and repaired on acquire.
- **Durability asymmetry.** Lifecycle events are write+flush+fsync per append;
  `worker.message` stream deltas are buffered and drained by throttle age
  (`event_stream_throttle_ms`). Losing buffered deltas on a crash is
  recoverable; losing a lifecycle event is not.
- The write boundary scrubs secrets from every string payload and appends
  `secret.redacted` audit markers (`append_with_redaction_audits`).

## fold == apply, and state.json is a cache

`reducer::fold(events)` builds `MissionState` from scratch: `initial_state`
from `mission.created` (must be event 1), then `apply` for each subsequent
event. `apply` asserts `event.seq == state.last_seq + 1` and mutates in place.
The property `fold(events) == fold(first) + repeated apply(...)`, and
determinism, are proptest-checked (`fold_equals_incremental_apply_and_is_deterministic`
in [reducer_test.rs](../../../crates/engine/tests/reducer_test.rs)), so the live
engine maintains state incrementally while any reader rebuilds byte-identical
JSON. `write_snapshot` writes `state.json` via temp-file + fsync + atomic
rename; it is a rebuildable cache, not authority.

The engine funnels every state change through `MissionEngine::emit`
(append → `reducer::apply` → `write_snapshot`) so log/state/snapshot never
drift. Events that `runner::run_worker`/`run_validator` append directly are
folded back in by `catch_up` (flush, `read_events_after(last_seq)`, apply,
snapshot) right after each run.

## MissionState shape

`MissionState` (types.rs) holds `mission` (goal, pinned `base_sha`, milestones,
status), a `runs` BTreeMap (deterministic serialization), token/cost totals,
pending messages/revisions/grants/questions, capped `recent_decisions`, live
`config`, workspace/provenance projections, and `last_seq`. Mission →
milestones → features is the work tree; full `WorkerRun` records live in
`runs` (features hold run-id refs).
`types.rs` and `events.rs` are marked CONTRACT FILEs: report needed changes,
don't edit them in an implementation phase.

## Lifecycle

`MissionStatus`: `Planning → Approved → Running` (also `Paused`/`Blocked`/
`Validating`) → terminal `Complete`/`Failed`/`Abandoned` (`is_terminal_status`).
Conceptual stages and where they live:

- **Draft / review plan** — `create` emits `mission.created` (Planning);
  `approve_plan` validates, pins `base_sha` at approval, commits `plan.json` +
  `plan.md` to the mission branch, and emits `plan.approved` (Approved).
- **Queue** — approval *enqueues* rather than starting; the
  [queue](../../../crates/engine/src/queue.rs) is per-repo, priority-ordered
  files under `.kranz/queue/`, since missions share one working tree and at most
  one runs at a time. `claim_front_when_repo_free` claims the front entry under
  a repo-wide busy guard; `auto_work` drains automatically.
- **Drain / run** — `MissionEngine::run` → `run_loop` (below).
- **Deliver** — workers commit on the mission branch; `final_gate` +
  `complete_mission` write the report and emit `mission.completed`.
- **Gated merge / land** — [`merge::merge_mission`](../../../crates/engine/src/merge.rs)
  refuses a dirty tracked tree, secret-scans the diff, runs the gate suite, then
  merges `--no-ff` into the base branch. It never pushes.

## The run loop (orchestrator.rs `run_loop`)

Before the loop, preflight and routing/standards ownership checks are surfaced,
then the pinned workspace provider provisions and runs bootstrap/readiness.
Each iteration drains control; returns on Complete/Failed; parks while Paused
or while a revision/capability grant awaits a human decision; then finds the
first incomplete milestone (none ⇒ `final_gate`). A Blocked milestone waits
for guidance; queued messages consult the orchestrator; a Pending milestone
records `start_sha`; then the next feature runs or the milestone enters
`validation_round`. Parallel batches remain opt-in via
`max_parallel_workers > 1`.

So the loop walks **milestones → features → workers → validators → judgement**:
`run_feature` runs the worker (bounded respawn, dirty-tree discipline, a JSON
judgement turn); `validation_round` runs scrutiny + functional validators (each
skippable) plus a deterministic out-of-contract-write sweep, converts findings
to fix-features or waives them (blocking at `max_fix_cycles_per_milestone`);
`final_gate` fails an empty deliverable outright, runs `check:"command"`
assertions itself and sends `agent-judgement` assertions to the orchestrator
against the `base_sha..HEAD` diff. Roles: `Orchestrator`, `Worker`,
`ValidatorScrutiny`, `ValidatorFunctional`.

## The dispatch pool (`workerCandidates`, KRZ-303)

With ≥2 entries in `workerCandidates` (`[{backend, model}, …]`, validated like
role selections; mutually exclusive with `maxParallelWorkers > 1`),
`run_feature` instead forks to `run_feature_dispatch_pool`: ONE unit of work
(the feature) runs on ALL N configured backends concurrently — one git
worktree per stream (`kranz-pool-*` dirs, `kranz/pool/<mission>/<feature>-c<i>`
branches, kept as the deliverables), the M3 buffered/replay single-writer
idiom. Each stream's `worker.spawned` carries an additive `candidate` link
(`{unit, index, count, backend}`) tying the sibling set to the unit; one
stream's failure never aborts its siblings — every terminal state is recorded.

Three freeze properties (the positioning ADR's 2026-07-31 boundary gloss),
enforced in code, not just docs:

1. **Outputs are candidates for judgement — never a winner.** The pool path
   calls no judgement turn, emits no `feature.completed`, selects/ranks/merges
   nothing; the milestone blocks for the human judgement act (surfaced by the
   `divergence-first-class-event` follow-up). A re-dispatch guard never
   silently re-fans a recorded unit.
2. **The claimed value is divergence for scrutiny — never throughput.** All N
   streams run the SAME unit; there is no "run N to go faster" path, and a
   candidate whose backend is unavailable fails its own stream loudly (no
   claude fallback) rather than silently duplicating a sibling's backend.
3. **The cost multiplier is explicit in consent.** `cost::estimate` multiplies
   worker runs by N; plan.md's "Dispatch pool" section names N and the
   candidates and states that the per-mission budget applies to the SUM.

Empty pool ⇒ the sequential path is byte-identical (regression-tested).

## Resumability

`MissionEngine::resume` reads the log, `fold`s it back to `MissionState`,
re-acquires the single-writer lock, recovers the last orchestrator sdk session
id for `--resume`, reaps crash-leaked worktrees/branches, and re-snapshots. No
agent session starts here — sessions are lazy. Because state is a pure fold, a
killed engine loses nothing durable: the run loop's status/feature checks pick
up exactly where the log left off. Guard this: `dry_run_revised_plan` validates
a revised plan against a clone *before* emit, because `emit` appends before it
folds — an event the reducer would reject would otherwise brick the mission on
every future load.
