# Mission plan — m-9d4193

**Goal:** Make the serve-hosted queue drain (POST /api/queue/drain) restore the operator's dispatch-time checkout when the drain finishes, honoring the CLI dispatcher's checkout-restore contract verbatim so the repo is never left stranded on a kranz/mission-* branch.

Branch `kranz/mission-m-9d4193` (from `main`). Approved plan of record; the machine-readable twin is [plan.json](plan.json). Live status: `kranz status` or the dashboard.

## Cost estimate

Estimated **$2.52 – $12.62** (expected ~$5.05). Rough estimate — live usage is authoritative; based on 27 completed mission(s).

## Validation contract

Defined before any feature; gates mission completion.

- **[a1]** A hosted drain whose fake mission run leaves the checkout on a kranz/mission-* branch restores the checkout to the dispatch branch captured at drain start once the drain finishes. 
  `cargo test -p kranz-server hosted_drain_restores_dispatch_checkout 2>&1 | grep -qE 'result: ok\. 1 passed'`
- **[a2]** When the drain starts while the checkout is already on a kranz/mission-* branch, the drain performs no restore and does not error (restoring TO a mission branch would recreate the stranding). 
  `cargo test -p kranz-server hosted_drain_skips_restore_when_started_on_mission_branch 2>&1 | grep -qE 'result: ok\. 1 passed'`
- **[a3]** When the fake mission run leaves the tracked working tree dirty, the drain aborts the restore, leaves the checkout on the mission branch, and still settles idle (never carries uncommitted tracked edits across a branch switch). 
  `cargo test -p kranz-server hosted_drain_leaves_checkout_when_tracked_tree_dirty 2>&1 | grep -qE 'result: ok\. 1 passed'`
- **[a4]** An idempotent second drain() call while a drain is already live returns the tracked state without capturing or restoring a checkout (capture/restore live only on the cold spawn path). 
  `cargo test -p kranz-server hosted_drain_second_call_does_not_capture_or_restore 2>&1 | grep -qE 'result: ok\. 1 passed'`
- **[a5]** The full workspace test suite passes. 
  `cargo test --workspace 2>&1 | grep -qE 'result: ok\.'`
- **[a6]** The hosted drain's checkout-restore path mirrors the CLI's restore_work_checkout rules verbatim — best-effort Option capture (None => no-op), skip restore when the captured branch starts with kranz/mission-, skip when the checkout is already back on it, abort on a dirty tracked tree (is_clean_tracked) — and the new host-level tests are non-vacuous: they run a fake mission that actually switches the checkout to a kranz/mission-* branch and assert the branch at drain-exit rather than asserting on a stub. *(agent judgement)*

## Milestone 1 — The hosted queue drain restores the operator's dispatch checkout on exit

### 1.1 Capture-and-restore the operator checkout around the hosted drain

PROBLEM: `MissionHost::drain` in `crates/server/src/host.rs` (method starts at ~line 910) spawns a background tokio task that runs `kranz_engine::work::drain_queue(&repo_root, false, <closure using run_mission_headless>)`, but it never captures or restores the operator's git checkout. Observed live on 2026-07-06: after mission m-ba8d58 completed via the hosted drain, `git branch --show-current` reported `kranz/mission-m-ba8d58` — the operator's next terminal interaction landed on a mission branch. The CLI dispatcher `kranz work` (see `cmd_work` and `restore_work_checkout` in `crates/cli/src/backlog.rs`) already handles this; the hosted drain must honor the SAME contract.

CONTRACT TO MIRROR (verbatim) — this is `restore_work_checkout` in `crates/cli/src/backlog.rs:438`:
1. Capture the operator's current branch at drain start as a best-effort `Option<String>` (open the repo, read `current_branch()`; any failure => `None`). `None` means never restore.
2. On drain exit, skip restore if: the captured value is `None`; OR the captured branch starts with `"kranz/mission-"` (restoring TO a mission branch would recreate the stranding); OR the checkout is already back on the captured branch.
3. Otherwise, if `is_clean_tracked()` is true, `git checkout <captured>` and log success; if `is_clean_tracked()` is false, abort the restore and emit a warning (leave the checkout on the mission branch — never carry uncommitted TRACKED edits across a branch switch; untracked files ride along, matching `is_clean_tracked` semantics); if probing the tree errors, leave the checkout in place with a warning.
4. Respect `DrainReport.stopped_busy`: if set, do NOT restore (mirrors `cmd_work`). At `once=false` this is always false, but wire the guard anyway for faithful parity.

PRIMITIVES (all already exist on `kranz_engine::git_ops::GitRepo`, already imported in host.rs): `GitRepo::open(&repo_root)`, `.current_branch()`, `.is_clean_tracked()`, `.checkout(name)`. Use these; do not shell out to git directly.

PLACEMENT / IDEMPOTENCE: `drain()` has idempotent early-return branches (when the `DrainSlot` is already `Starting`/`Running`) that return the tracked state WITHOUT spawning. Capture MUST happen only on the COLD path that actually spawns a task — never in the early-return branches — so a second concurrent `drain()` call can never capture a mission branch. Capture the dispatch branch at drain start on the cold path (before or as the spawned task begins; nothing has run yet so the branch is still the operator's). Perform the restore after `drain_queue(...)` returns inside the spawned task (before or after setting `live = false`; both are acceptable as long as it runs on drain-exit).

TESTABILITY SEAM (required): refactor the spawned-task body so the per-mission runner is INJECTABLE, so a host-level test can drive the drain with a fake mission run. Extract a private async helper (e.g. `async fn drain_task(repo_root, state, run_mission)` or an equivalent method) that does: capture branch -> `drain_queue(&repo_root, false, run_mission)` -> restore branch (honoring the rules above). Production `drain()` passes the real runner (the existing closure built around `run_mission_headless`); tests call the helper directly (NOT via `tokio::spawn`, to stay deterministic) with a FAKE runner. Do not change the public REST behavior of `drain()` or its idempotency guarantees — the existing tests `empty_queue_drain_returns_ok_and_settles_idle`, `second_drain_while_live_returns_tracked_state_without_spawning_second`, and `two_concurrent_cold_drains_spawn_exactly_one` must still pass unchanged.

TESTS TO ADD (in the `#[cfg(test)] mod tests` in `crates/server/src/host.rs`, using the existing `init_repo()` harness which inits a clean repo on branch `main`, `queue::enqueue` to seed one queued entry, and a fake runner that does `GitRepo::open` + `create_branch("kranz/mission-<id>", None)` + `checkout("kranz/mission-<id>")` to simulate what a real mission run leaves behind). The fake runner should return `Ok(0)` so the drain treats it as a completed run. Name the tests EXACTLY as below so the validation commands match:
- `hosted_drain_restores_dispatch_checkout`: on `main`, seed one queued entry, run the drain helper with a fake runner that checks out `kranz/mission-<id>`; assert that after the helper returns, `current_branch()` is back to `main`.
- `hosted_drain_skips_restore_when_started_on_mission_branch`: check out a `kranz/mission-*` branch FIRST, then run the helper with a fake runner; assert no error and the checkout is left as-is (no attempt to restore to a mission branch).
- `hosted_drain_leaves_checkout_when_tracked_tree_dirty`: fake runner checks out `kranz/mission-<id>` AND makes a tracked-file modification (e.g. overwrite the committed `README.md`); assert the checkout is left on the mission branch (restore aborted) and the drain still completes without error.
- `hosted_drain_second_call_does_not_capture_or_restore`: assert the idempotent second-`drain()`-while-live path returns tracked state without capturing/restoring — fabricate a live `DrainSlot::Running` tracker (as `second_drain_while_live_returns_tracked_state_without_spawning_second` does), then confirm a second `drain()` call performs no checkout mutation (e.g. the checkout the test set up beforehand is unchanged) and no new task is spawned.

Each of these tests uses `init_repo()` which returns `None` (early-return the test) when git is unavailable — follow that existing pattern.

DO NOT hoist or modify `restore_work_checkout` in the CLI crate; keep the blast radius inside `crates/server`. Mirror the rules in a small private helper in host.rs. Keep the module's existing comment style (the drain methods are heavily doc-commented; match that density).

Run `cargo test -p kranz-server` and `cargo test --workspace` before reporting; include the passing test-runner output as testEvidence.

Done when:
- A host-level test `hosted_drain_restores_dispatch_checkout` seeds a queued entry on branch main, runs the drain with a fake mission run that checks out a kranz/mission-* branch, and asserts the checkout is back on main after the drain finishes.
- A host-level test `hosted_drain_skips_restore_when_started_on_mission_branch` starts the drain while on a kranz/mission-* branch and asserts no restore is attempted and no error occurs.
- A host-level test `hosted_drain_leaves_checkout_when_tracked_tree_dirty` has the fake mission run leave a modified tracked file and asserts the checkout is left on the mission branch (restore aborted) while the drain still completes.
- A host-level test `hosted_drain_second_call_does_not_capture_or_restore` asserts the idempotent second drain() call returns tracked state without mutating the checkout or spawning a second task.
- The host restore path mirrors restore_work_checkout verbatim: None-capture => no-op, skip when captured branch starts with kranz/mission-, skip when already restored, abort on a dirty tracked tree via is_clean_tracked, and respect DrainReport.stopped_busy.
- Capture happens only on the cold spawn path of drain(), never in the idempotent early-return branches; the existing empty_queue/second_drain/two_concurrent_cold_drains tests still pass unchanged.
- `cargo test --workspace` passes.

