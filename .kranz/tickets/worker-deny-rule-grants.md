---
title: Worker deny-rule grants — lift a WORKER_DENY rule by operator consent
priority: 2
schedule: once
---

## Decision (2026-07-13, Craig): maximum consent

An operator may lift ANY `WORKER_DENY` rule (incl. `sudo`, `git push`, publish)
via an explicit, logged grant — the deny-wins guarantee becomes fully
operator-overridable, per-mission, recorded in the event log. This is the third
`GrantKind` after command + touch-set (both shipped). It is the one that erodes
a safety rail, so it gets its own careful build + adversarial review.

## Scoped design (ready to build)

- **GrantKind::WorkerDeny** — reuse the park/approve/deny/timeout/cap machinery
  and all four surfaces (the flow already generalizes over `GrantKind`).
- **State:** `Mission.deny_exceptions: Vec<String>` (CONTRACT FILE, additive,
  extend-only). NOT on `Plan` — purely operator-granted at runtime; `PlanApproved`
  / `apply_revised_plan` must leave it untouched (orthogonal to command_grants /
  touch_set). Reducer `GrantApproved{WorkerDeny}` → `deny_exceptions.push(rule)`.
- **Mechanism (Option B — exact rule removal):** `permissions::for_role` gains a
  `deny_exceptions: &[String]` param; the Worker arm does
  `disallowed.retain(|rule| !deny_exceptions.contains(rule))` AFTER building
  `WORKER_DENY + deny_patterns`. Exact-match removal keeps the safety-critical
  fold trivially auditable (the event log names the exact rule lifted). Ripples
  to every `for_role` caller (validators/orchestrator pass `&[]`).
- **Trigger (the hard part):** worker command denials happen in `run_feature`,
  not validation. The runner already captures `denied_commands` for workers (the
  fixed positional correlation). Map the denied command → the deny rule that
  blocked it via a new `permissions::matching_deny_rule(command, rules)` (parse
  `Bash(<pat>*)`, prefix-match — best-effort; a wrong/no match just leaves the
  command denied, cap-bounded). Offer a `WorkerDeny` grant with target = the rule.

## Why deferred from the touch-set commit (the integration risks to solve)

The park does NOT fit `run_feature`'s loop as cleanly as validation-round grants:
- **Park point:** must be AFTER the §4.4 dirty-tree resolution (so the worker's
  partial work is committed and the tree is clean) but the grant is about the
  worker run. Parking mid-loop while holding `mi`/`fi` is exactly the deep-park /
  stale-index hazard that killed attempt 1 — route it through the run-loop gate
  (return `Ok(())`, re-enter), never a deep park calling `drain_control`.
- **Deny → wasteful re-run:** on deny (cap-saturated, touch-style), re-entering
  `run_feature` re-spawns the WORKER (expensive; consumes respawn budget) only to
  reach judgement. Command/touch deny re-runs validation (cheaper). Decide
  whether deny should instead judge the already-run outcome (needs the outcome to
  survive the park — it doesn't today).
- **Respawn budget interaction:** confirm the park/re-enter doesn't miscount
  `respawns` vs `max_respawns`, and that approve→lift→respawn is bounded.

Build with the same discipline: bottom-up (state + permissions + reducer, each
tested), then the `run_feature` trigger, then a focused adversarial review BEFORE
commit (attempt 1's lesson). Surfaces need only a `WorkerDeny` label (the flow is
already kind-generic).
