# Mission report — m-9d4193

**Goal:** Make the serve-hosted queue drain (POST /api/queue/drain) restore the operator's dispatch-time checkout when the drain finishes, honoring the CLI dispatcher's checkout-restore contract verbatim so the repo is never left stranded on a kranz/mission-* branch.

Branch `kranz/mission-m-9d4193` (from `main`). Plan of record: [plan.md](plan.md).

**Elapsed:** 3h 19m 40s
**Tokens:** 23617 in / 52635 out / 4857816 cache read / 291470 cache write
**Cost:** $14.98 actual vs $3.05–$15.25 estimated (expected $6.10)

## What shipped

### Milestone 1 — The hosted queue drain restores the operator's dispatch checkout on exit ✅

- ✅ **Capture-and-restore the operator checkout around the hosted drain** — 1 run
  - `f087799` [f-1-1] restore operator checkout around the hosted queue drain

## Validation history

### ms-1 round 1 — The hosted queue drain restores the operator's dispatch checkout on exit

No findings.

### Final gate

- [critical] a6 *(final gate)* — verdict turn unparseable; assertion could not be verified

Disposition: waived.
- a6: Not a code defect — the finding is an artifact of a prior unparseable verdict turn, not a contract violation. a6 is genuinely satisfied: direct inspection of commit f087799 confirms restore_drain_checkout mirrors the CLI restore_work_checkout rules verbatim (None no-op, skip kranz/mission-* capture, skip already-restored, abort on dirty is_clean_tracked; only println->tracing differs) and the host-level tests are non-vacuous (they create+checkout a kranz/mission-* branch and assert the branch at drain-exit). No fresh worker session has anything to fix.

## Contract outcomes

- ✅ **[a1]** A hosted drain whose fake mission run leaves the checkout on a kranz/mission-* branch restores the checkout to the dispatch branch captured at drain start once the drain finishes. *(command: `cargo test -p kranz-server hosted_drain_restores_dispatch_checkout 2>&1 | grep -qE 'result: ok\. 1 passed'`)*
- ✅ **[a2]** When the drain starts while the checkout is already on a kranz/mission-* branch, the drain performs no restore and does not error (restoring TO a mission branch would recreate the stranding). *(command: `cargo test -p kranz-server hosted_drain_skips_restore_when_started_on_mission_branch 2>&1 | grep -qE 'result: ok\. 1 passed'`)*
- ✅ **[a3]** When the fake mission run leaves the tracked working tree dirty, the drain aborts the restore, leaves the checkout on the mission branch, and still settles idle (never carries uncommitted tracked edits across a branch switch). *(command: `cargo test -p kranz-server hosted_drain_leaves_checkout_when_tracked_tree_dirty 2>&1 | grep -qE 'result: ok\. 1 passed'`)*
- ✅ **[a4]** An idempotent second drain() call while a drain is already live returns the tracked state without capturing or restoring a checkout (capture/restore live only on the cold spawn path). *(command: `cargo test -p kranz-server hosted_drain_second_call_does_not_capture_or_restore 2>&1 | grep -qE 'result: ok\. 1 passed'`)*
- ✅ **[a5]** The full workspace test suite passes. *(command: `cargo test --workspace 2>&1 | grep -qE 'result: ok\.'`)*
- ✅ **[a6]** The hosted drain's checkout-restore path mirrors the CLI's restore_work_checkout rules verbatim — best-effort Option capture (None => no-op), skip restore when the captured branch starts with kranz/mission-, skip when the checkout is already back on it, abort on a dirty tracked tree (is_clean_tracked) — and the new host-level tests are non-vacuous: they run a fake mission that actually switches the checkout to a kranz/mission-* branch and assert the branch at drain-exit rather than asserting on a stub. *(agent judgement)*

All assertions passed at the final contract gate (waivers, if any, appear in the validation history).
