# Mission plan — m-9e4ef3

**Goal:** Any path that drives a mission to a terminal (or blocked) state — `kranz run`, `kranz exec`, REST `/start`, and the drain itself — reconciles the linked ticket's `.status` sidecar via one authoritative, mission-id-keyed helper, healing the stale-"failed" bug and remapping Blocked to needs-you.

Branch `kranz/mission-m-9e4ef3` (from `main`). Approved plan of record; the machine-readable twin is [plan.json](plan.json). Live status: `kranz status` or the dashboard.

## Cost estimate

Estimated **$5.09 – $25.45** (expected ~$11.87). Rough estimate — live usage is authoritative; based on 46 completed mission(s).

## Considered alternatives

**Chosen approach:** Option (a): one authoritative engine helper (`reconcile_ticket_for_mission`) keyed off the ticket's recorded mission_id and the mission's folded terminal status, called by all four paths (drain + the three run() surfaces). Authoritative, single mapping, self-consistent with the drain, and the Delivered/Landed split stays a read-time projection so it is preserved for free.

Rejected shapes:
- **Option (b): lazy reconcile-on-read — have ticket list/show fold the linked mission and re-derive Done/Failed when the ticket is Running/Failed but the mission is terminal.** — Cheaper and self-healing, but non-authoritative: the persisted `.status` stays wrong, every reader must re-fold the mission, and consumers that read `.status` directly (e.g. work_skip_for_failed_blocker) still see stale state unless each is taught the same fold.
- **Bury the reconcile inside `MissionEngine::run()` so it writes the ticket on its own terminal transition.** — Would make the run loop write ticket sidecars from inside a worktree run, double-writing with the drain and risking the claim/race the drain's post-claim ordering is designed to avoid; a shared helper the callers invoke keeps ticket I/O out of the run loop and out of the drain's critical section.
- **Fix each surface (run, exec, run_to_end) with its own local status→ticket mapping.** — Duplicates the Blocked/Complete/Failed mapping in three places that will drift; the incident already came from one path disagreeing with the drain.

## Validation contract

Defined before any feature; gates mission completion.

- **[a1]** The entire workspace test suite passes. 
  `cargo test --workspace --no-fail-fast`
- **[a2]** Formatting is clean (CI gate). 
  `cargo fmt --all --check`
- **[a3]** Clippy is clean with warnings denied (CI gate). 
  `cargo clippy --workspace --all-targets -- -D warnings`
- **[a4]** A mission that folds to Complete reconciles its linked ticket to Done when the reconcile runs outside the drain (reconcile-on-terminal), proven by named tests at the engine and at the `kranz run`/run_mission_loop surface. 
  `cargo test --workspace reconcile_on_terminal`
- **[a5]** A ticket left at Failed by a drain-time block is rewritten to Done when its mission later completes (the m-0f1abd / m-9dc8c1 heal case). 
  `cargo test --workspace reconcile_heals_failed_to_done`
- **[a6]** A mission whose folded status is Blocked reconciles its ticket to the needs-you state (NeedsContext), never Failed. 
  `cargo test --workspace blocked_reconciles_to_needs_you`
- **[a7]** The status→ticket mapping lives in exactly one shared engine helper (`reconcile_ticket_for_mission`) invoked by every non-drain terminal path (`run_mission_loop`, `exec`, `run_to_end`) AND by `drain_queue`; no surface duplicates the mapping, and the Delivered/Landed split remains a read-time projection via `merged::ticket_merged` (unchanged — the helper only ever writes `Done`, never a finer split). *(agent judgement)*

## Milestone 1 — Every terminal path reconciles its ticket through one authoritative helper

### 1.1 Authoritative reconcile helper + corrected status→ticket mapping in the engine

Add a single authoritative reconcile function to `crates/engine/src/work.rs`:

```rust
pub fn reconcile_ticket_for_mission(
    repo_root: &Path,
    mission_id: &str,
) -> crate::error::Result<Option<(String, TicketState)>>
```

Behaviour:
1. Reverse-lookup the linked ticket slug with the EXISTING `Ticket::slug_for_mission(repo_root, mission_id)` (keys off the ticket's recorded `mission_id`). If `None`, return `Ok(None)` — a mission with no linked ticket is a no-op.
2. Load the mission's folded status: build `MissionPaths::new(repo_root, mission_id)`, and if `events_file()` is a file, `EventLog::read_events` + `reducer::fold` to get `state.mission.status`. If the mission is unloadable/missing, return `Ok(None)` (never fail the caller). Mirror the defensive pattern already in `crates/engine/src/merged.rs::ticket_merged`.
3. Map the folded `MissionStatus` to a `TicketState` using an updated `ticket_state_for_mission` (see below). For LIVE statuses (Running, Validating, Paused, Approved, Planning) return `Ok(None)` and write nothing — the ticket is still mid-flight and must not be clobbered.
4. For a terminal-or-blocked status, read the ticket's current state; if the mapped state differs, write it via `Ticket::write_state` (which already preserves the `mission_id` link) and return `Ok(Some((slug, new_state)))`. If it already matches, return `Ok(None)`. The helper MUST be willing to overwrite `Failed` (that is the heal case).

Update `ticket_state_for_mission(status: MissionStatus) -> TicketState` (currently `Complete => Done`, everything-else `=> Failed`) to:
- `MissionStatus::Complete => TicketState::Done`
- `MissionStatus::Failed => TicketState::Failed`
- `MissionStatus::Abandoned => TicketState::Failed`
- `MissionStatus::Blocked => TicketState::NeedsContext`  (the needs-you stage; this is the deliberate fix — a blocked mission is needs-input, not a failure)
- All other (live) statuses: this function is only called by the reconcile helper for terminal/blocked statuses, so keep the existing fallback but ensure the helper itself gates out live statuses per step 3.

Do NOT change `mission_state_from_code` yet and do NOT touch the drain loop or any surface here — wiring is separate features. This feature is the helper + mapping + tests only.

Add tests (each test NAME must contain the exact substrings the contract greps for):
- `reconcile_on_terminal_maps_complete_to_done`: seed a mission event log that folds to Complete, a linked ticket at `Running`; assert the helper writes `Done` and returns `Some`.
- `reconcile_heals_failed_to_done`: linked ticket at `Failed`, mission folds to Complete; assert helper rewrites it to `Done` (the incident scenario).
- `blocked_reconciles_to_needs_you`: mission folds to Blocked; assert helper writes `NeedsContext`, NOT `Failed`.
- helper returns `Ok(None)` when the mission has no linked ticket, and when the mission status is live (e.g. Running) — no write.
- Delivered/Landed preserved: after the helper writes `Done` for a Complete mission whose branch is not merged, `merged::ticket_merged` still returns `Some(false)` (delivered); reuse the seeding style in `crates/engine/tests/merged_test.rs`.
- keep/extend the existing `ticket_state_for_mission_maps_terminal_status` unit test to cover the new Blocked/Abandoned arms.

Ensure `reconcile_ticket_for_mission` is reachable as `kranz_engine::work::reconcile_ticket_for_mission` for later surface wiring.

Done when:
- `reconcile_ticket_for_mission` reverse-looks-up the slug via `Ticket::slug_for_mission` and returns Ok(None) when no ticket links the mission
- A Complete-folding mission with a linked ticket at Running or Failed is reconciled to Done (test names contain `reconcile_on_terminal` and `reconcile_heals_failed_to_done`)
- A Blocked-folding mission reconciles its ticket to NeedsContext and never Failed (test name contains `blocked_reconciles_to_needs_you`)
- A mission whose folded status is live (Running/Validating/Paused/Approved) causes no ticket write
- After reconcile writes Done, `merged::ticket_merged` still returns the correct Delivered/Landed bit (the split is untouched)
- cargo test for the engine crate is green; cargo fmt --all --check and cargo clippy --workspace --all-targets -- -D warnings pass

### 1.2 Unify drain_queue's terminal ticket write onto the shared helper

In `crates/engine/src/work.rs::drain_queue_with_probe`, replace the bespoke post-run terminal ticket write (the `match &status { Ok(code) => Ticket::write_state(.., mission_state_from_code(*code), ..), Err(_) => Ticket::write_state(.., Failed, ..) }` block around the current lines 300-313) so that, after `run_mission` returns and the claim is finished/released, the terminal ticket state is set by calling `reconcile_ticket_for_mission(repo_root, &mission_id)` (the helper added in the prior feature). This makes the drain use the SAME mapping as every other path — in particular a drain-time Blocked now yields `NeedsContext`, not `Failed`.

Constraints:
- Leave the pre-run `Ticket::write_state(.., Running, None)` untouched.
- Leave the skip path (`work_skip_for_failed_blocker` → writes `Failed` with a `skipped: blocked-by X failed` note) and the readiness Park writes untouched — those are drain-owned states the reconcile helper must not override, and they occur when `run_mission` is NOT called.
- Preserve the existing claim finish/release ordering and the `report.ran` / `report.skipped` / `report.parked` bookkeeping and the `Err(_)` propagation for bare (ticketless) entries (`status?`). Only the ticket STATE write changes; error handling and the injected-run seam stay identical.
- `mission_state_from_code` may become unused; remove it (and its test) if so, or keep only if still referenced — do not leave dead code that trips clippy `-D warnings`.

Review interaction: `work_skip_for_failed_blocker` keys off `TicketState::Failed`. A blocker mission that ends Blocked now leaves its ticket at `NeedsContext` instead of `Failed`, so its dependents are no longer force-skipped (they remain gated by `deps::unsatisfied_blockers` requiring the blocker's mission to be Complete, so they cannot hot-loop). Verify the existing `drain_queue_skips_ticket_whose_blocker_failed` test still reflects a genuinely Failed blocker and add a test that a drain-time Blocked mission's own ticket becomes NeedsContext.

Update any existing drain tests that asserted `Failed` for a blocked/non-zero outcome to the new mapping. Add `drain_reconciles_terminal_ticket_via_helper` (or similar) proving a drained Complete mission's ticket is Done and a drained Blocked mission's ticket is NeedsContext.

Done when:
- drain_queue writes the terminal ticket state by calling reconcile_ticket_for_mission, not a local exit-code mapping
- A drained mission that folds to Complete leaves its ticket Done; one that folds to Blocked leaves it NeedsContext (never Failed)
- The skip-on-failed-blocker path and the readiness Park path still write their drain-owned states unchanged
- No dead code remains (mission_state_from_code removed if unused); clippy -D warnings passes
- cargo test --workspace green including updated/added drain tests

### 1.3 Wire reconcile into the three non-drain run() terminal surfaces

Call `kranz_engine::work::reconcile_ticket_for_mission(repo_root, mission_id)` after `engine.run()` returns a terminal/blocked status on each of the three surfaces that today never touch the ticket. In every case, capture `repo_root` and `mission_id` BEFORE `drop(engine)` where needed, and treat a reconcile error/None as non-fatal (log at most — it must never change the process exit code or the HTTP status).

1. `crates/cli/src/commands.rs::run_mission_loop` — the `kranz run` path and the incident path. After `run_result?` yields the `MissionStatus`, reconcile the ticket for `mission`. This must fire on the Complete branch (exit 0) so a `kranz run` resume after a drain-time block heals `Failed → Done`; it should also fire on the Blocked branch before the interactive-guidance prompt so the pipeline shows needs-you while the operator is deciding.
2. `crates/cli/src/exec.rs` — headless `kranz exec`. After `run_result` is resolved to `status`, reconcile the ticket for `mission_id` (before the cloud-push block is fine; use `repo` as repo_root).
3. `crates/server/src/host.rs::run_to_end` — REST `POST /api/missions/:id/start`. After `engine.run()` returns, reconcile the ticket for `mission_id`. Obtain `repo_root` from the engine (e.g. its paths/repo handle) BEFORE `drop(engine)`.

Do NOT re-implement any mapping here — these are one-line calls into the shared helper. Do NOT alter the existing exit-code mapping (`exit_code_for`), the live event tailing, the cloud-push behaviour, or the hosted-mission registry removal ordering.

Add surface tests proving reconcile-on-terminal WITHOUT a drain:
- cli: a test whose name contains `reconcile_on_terminal` driving `run_mission_loop` (or the smallest seam that exercises the post-run reconcile) to Complete on a mission with a linked ticket at Running/Failed, asserting the ticket ends Done. If `run_mission_loop` is hard to unit-test directly, add the test at whatever seam the crate already uses to exercise it, or an integration test under `crates/cli/tests/`.
- server: a test whose name contains `reconcile_on_terminal` asserting that after `POST /api/missions/:id/start` drives a mission to Complete, the linked ticket is Done (extend the existing host/server test harness that already starts missions to completion, e.g. host_test.rs around the start→complete flow).

Done when:
- run_mission_loop, exec, and run_to_end each call reconcile_ticket_for_mission after engine.run() returns, with repo_root/mission_id captured before the engine is dropped
- A mission completed via kranz run / run_mission_loop with no drain leaves its linked ticket Done (test name contains reconcile_on_terminal)
- A mission completed via REST /start leaves its linked ticket Done (server test name contains reconcile_on_terminal)
- A reconcile failure never changes the CLI exit code or the HTTP response status
- cargo test --workspace green; fmt and clippy -D warnings pass

