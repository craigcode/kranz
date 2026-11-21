---
title: A fix feature added while unblocking reuses the next fix's id and stops the mission
priority: 1
schedule: once
---

## Goal

Every fix feature a milestone gets has a unique id, however it was created. A validator finding after an operator-directed repair creates its fix feature and the mission carries on.

## Context

Seen in mission m-882080 on 2026-11-20. The milestone was blocked, and the operator's guidance led the orchestrator to choose `unblock-add-fix`. That emitted `fixfeature.created` for `ms-1-fix-1-1`, a docs repair, which ran and passed. The next validation round raised one minor finding. The orchestrator turned it into a fix feature, also numbered `ms-1-fix-1-1`. The event log refused it, `kranz run` exited with "mission is in an invalid state for this operation: duplicate fixfeature.created for feature 'ms-1-fix-1-1' with a different payload", and the mission was left in RUNNING with no way forward.

The cause is in how the id is built. `emit_fix_features` in `crates/engine/src/findings.rs` names each fix `{milestone_id}-fix-{cycle}-{n}`, with `cycle = fix_cycles + 1`. The `unblock-add-fix` branch in `crates/engine/src/orchestrator.rs` deliberately does not spend a fix cycle (its comment says so: the reducer's fix-cycle guard only increments from Validating). So after it, `fix_cycles` is unchanged and the next validator-driven fix computes the same id.

Running `kranz run` again would most likely reach the same decision and fail the same way.

## Scoping answers

- Keep the rule that an operator-directed repair does not spend a fix cycle. Fix only the id.
- The id must be unique among all features of the milestone, including fixes from earlier cycles and earlier repairs. Ids already in existing event logs must not change, and replaying an existing log must give the same state as today.
- Decide whether repairs get their own id form (for example `{milestone_id}-repair-{n}`) or whether the fix id takes the next free number. Either is fine if the rule is simple and tested.
- `events.rs` and `types.rs` are additive-only. This fix should not need a schema change.
- Do not add a way to recover the stuck mission m-882080. Its work was finished as a pull request.

## Acceptance hints

- A mission test: a milestone blocks, the operator's guidance leads to `unblock-add-fix`, the repair passes, the next round raises a finding, and its fix feature is created with an id different from the repair's. The mission completes.
- A test that two `unblock-add-fix` repairs on one milestone get different ids.
- Replaying an existing log that holds a validator fix and no repair gives the same feature ids as today.
- Anti-vacuity: the test filter matches only the new tests.
