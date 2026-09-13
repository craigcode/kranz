# Mission plan — m-468277

**Goal:** Introduce an explicit Approved mission status that the reducer folds on plan.approved before any run loop starts (Running is reserved for after a milestone or worker spawns), and surface Approved distinctly in kranz status, GET /api/missions, the dashboard, and the Slack card copy.

Branch `kranz/mission-m-468277` (from `main`). Approved plan of record; the machine-readable twin is [plan.json](plan.json). Live status: `kranz status` or the dashboard.

## Validation contract

Defined before any feature; gates mission completion.

- **[a1]** The reducer folds an approved-but-not-run event log (mission.created + plan.approved) to status Approved, not Running; adding milestone.started or worker.spawned then yields Running; a full lifecycle ending in mission.completed still folds to Complete; and the Approved->Running transition is guarded so it never overwrites a terminal, paused, or blocked status. 
  `cargo test -p kranz-engine approved_status 2>&1 | grep -qE 'result: ok\. [1-9][0-9]* passed'`
- **[a2]** kranz status rendering maps an approved-not-run mission to the label APPROVED (never RUNNING). 
  `cargo test -p kranz-cli approved_status 2>&1 | grep -qE 'result: ok\. [1-9][0-9]* passed'`
- **[a3]** GET /api/missions reports status "approved" for a mission whose log holds plan.approved with no worker.spawned. 
  `cargo test -p kranz-server approved_status 2>&1 | grep -qE 'result: ok\. [1-9][0-9]* passed'`
- **[a4]** The Slack status card and App Home mission rows render the Approved state with distinct copy (the word "Approved", not "Running"). 
  `cargo test -p kranz-slack approved_status 2>&1 | grep -qE 'result: ok\. [1-9][0-9]* passed'`
- **[a5]** The entire workspace test suite passes with no regressions introduced by the new status variant. 
  `cargo test --workspace`
- **[a6]** The dashboard renders the Approved state as a visually distinct chip and treats it as abandonable: the MissionStatus union includes 'approved', StatusStrip's STATUS_LABEL maps it to a label, ABANDONABLE contains 'approved', and styles.css defines a distinct pill-approved appearance. *(agent judgement)*

## Milestone 1 — Approved status folded by the reducer and surfaced across every consumer

### 1.1 Add MissionStatus::Approved and fold it in the reducer (Rust core)

In crates/engine, add a new variant `Approved` to the `MissionStatus` enum in crates/engine/src/types.rs (it is `#[serde(rename_all = "lowercase")]`, so it serializes as "approved"). Place it immediately after `Planning`. This is the state for a mission whose plan is approved but whose run loop has not started.

Reducer changes in crates/engine/src/reducer.rs:
- The `EventKind::PlanApproved` arm currently ends with `state.mission.status = MissionStatus::Running;` (around line 82). Change it to set `MissionStatus::Approved`.
- In the `EventKind::MilestoneStarted` arm and the `EventKind::WorkerSpawned` arm, after their existing logic, add: if `state.mission.status == MissionStatus::Approved` then set it to `MissionStatus::Running`. GUARD strictly on `== Approved` so Paused/Blocked/Validating/terminal statuses are never overwritten. (These two events are the first status-relevant events the run loop emits; this is the deterministic Approved->Running transition — do NOT lock-sniff or reference queue.rs.)

Fix the two exhaustive `match status` sites that will otherwise fail to compile:
- crates/engine/src/digest.rs (~line 117): add `MissionStatus::Approved => "approved",`.
- crates/cli/src/output.rs `mission_status_label` (~line 24): add `MissionStatus::Approved => "APPROVED",`.

Do NOT change `is_terminal_status` (Approved is correctly non-terminal), `exit_code_for` (its `_ => 1` default already covers Approved like Running), or `run()`'s start gate (it only rejects `Planning`, so an Approved mission remains runnable). Run `cargo build --workspace` to catch any other exhaustive match you missed and fix them the same way (map Approved parallel to how Running/Planning are handled at that site).

Add reducer unit tests to crates/engine (e.g. in crates/engine/tests/reducer_test.rs or the reducer's own #[cfg(test)] module), each named with the prefix `approved_status_` so a name-filtered `cargo test` selects them: (1) mission.created + plan.approved folds to `MissionStatus::Approved`; (2) that log plus a milestone.started folds to `Running`; (3) that log plus a worker.spawned folds to `Running`; (4) a full lifecycle log ending in mission.completed still folds to `Complete`; (5) after a terminal event the guarded transition leaves the terminal status untouched (construct whatever the reducer's contiguous-seq rules allow to demonstrate the guard). Look at existing tests in reducer_test.rs for the event-construction helpers and follow their style.

Also add a CLI test named with prefix `approved_status_` (in crates/cli, e.g. output.rs #[cfg(test)] or crates/cli/tests) asserting `mission_status_label(MissionStatus::Approved) == "APPROVED"` and that folding an approved-only event log then labelling it yields "APPROVED", never "RUNNING".

Done when:
- cargo test -p kranz-engine approved_status prints a 'result: ok. N passed' line with N>=1 and no failures
- cargo test -p kranz-cli approved_status prints a 'result: ok. N passed' line with N>=1 and no failures
- cargo build --workspace succeeds (all exhaustive matches on MissionStatus handle Approved)
- Folding [mission.created, plan.approved] yields MissionStatus::Approved; appending milestone.started or worker.spawned yields Running

### 1.2 GET /api/missions reports "approved" for an approved-not-run mission

Depends on the Rust-core feature having added `MissionStatus::Approved` (serde lowercase "approved"). In crates/server, the GET /api/missions handler folds each mission's event log and serializes `mission.status`. Confirm no handler code needs changing (serialization is automatic), then add an integration/unit test named with the prefix `approved_status_` (in crates/server, following the existing test style in crates/server/src/rest.rs or crates/server/tests) that: constructs or points the handler at a mission directory whose event log contains mission.created + plan.approved and NO worker.spawned, invokes the GET /api/missions code path (or the fold-and-serialize helper it uses), and asserts the returned JSON status field for that mission equals "approved" (and is not "running"). Reuse existing server test fixtures/helpers for building a mission dir and event log — inspect how other rest tests set up a mission before writing new setup. Also confirm the docs assertion in protocol.md:14 (GET /api/missions shape) still holds — the status field simply gains an additional possible value.

Done when:
- cargo test -p kranz-server approved_status prints a 'result: ok. N passed' line with N>=1 and no failures
- The GET /api/missions response status field is "approved" for a mission with plan.approved and no worker.spawned

### 1.3 Slack status card + App Home render Approved distinctly

Depends on the Rust-core feature. In crates/slack, `status_word` (crates/slack/src/bridge.rs ~line 1650) uses `format!("{status:?}")`, so MissionStatus::Approved already renders as the word "Approved" in both the status card (crates/slack/src/format.rs `build_status`) and the App Home active-mission rows (`build_active_missions`/home view). Verify that path yields "Approved" and that it reads distinctly from "Running". If the card/home applies any per-status emoji or styling elsewhere, extend it so Approved is visibly distinct (do not regress other statuses). Add a Slack format/bridge test named with the prefix `approved_status_` (follow the style in crates/slack/tests/formatting.rs and the #[cfg(test)] block in format.rs, e.g. the existing `status_frames_the_summary_with_id_and_pill` test): build a StatusSummary (or drive `status_word`) for an Approved mission and assert the rendered blocks contain "Approved" and do not contain "Running". Keep the assertion resilient to surrounding copy.

Done when:
- cargo test -p kranz-slack approved_status prints a 'result: ok. N passed' line with N>=1 and no failures
- status_word(MissionStatus::Approved) == "Approved"
- The rendered Slack status card for an Approved mission contains "Approved" and not "Running"

### 1.4 Dashboard shows a distinct Approved chip and treats it as abandonable

In apps/dashboard (React/TS, no test runner — do NOT add one):
- apps/dashboard/src/lib/types.ts: add `| 'approved'` to the `MissionStatus` union (place it after 'planning').
- apps/dashboard/src/components/StatusStrip.tsx: add `approved: 'Approved',` to the `STATUS_LABEL` Record<MissionStatus,string> (it is exhaustive over the union, so this is required to typecheck) and add `'approved'` to the `ABANDONABLE` set (an approved-but-not-run mission should be abandonable, like planning/running). Approved is a pre-run state: do not show the 'running' pause button for it; abandon should be available.
- apps/dashboard/src/styles.css: add a `.pill-approved` (and, if the pattern requires, `.pill-approved .status-dot` and any `.status-approved` strip class) styled to be visually distinct from `.pill-running` and `.pill-planning` — follow the existing pill class conventions in that file.
Verify the app typechecks/builds locally with `npm install` then `npm run build` (tsc -b + vite build) before finishing; the exhaustive Record over the union guarantees the label is complete. Keep the change minimal and consistent with the surrounding component style.

Done when:
- apps/dashboard/src/lib/types.ts MissionStatus union includes 'approved'
- StatusStrip STATUS_LABEL maps 'approved' and ABANDONABLE contains 'approved'
- styles.css defines a .pill-approved appearance distinct from pill-running and pill-planning
- cd apps/dashboard && npm install && npm run build succeeds (typecheck + build pass)

### 1.5 Document the Approved status in the docs status references

Update the docs to record the new status, mirroring how `Validating` was documented:
- docs/design.md 'Deviations from the plan document' section: add a new numbered entry (parallel to entry #2 which documents adding `MissionStatus::Validating`) explaining that `MissionStatus::Approved` was added so an approved-but-not-yet-run mission no longer misreports as Running; note that plan.approved folds to Approved and the first milestone.started/worker.spawned folds to Running.
- docs/protocol.md line ~14 (the GET /api/missions row): note that the status field's possible values now include "approved" (an approved mission with no run activity) — keep it additive/backward-compatible.
- docs/dashboard-reference.md: if it enumerates mission statuses or status-strip pills, add Approved to that table/list, described as approved-but-not-started, distinct from Running.
Do not alter behaviour; docs only. Match the existing markdown table/list style at each site.

Done when:
- docs/design.md Deviations section has a new entry documenting MissionStatus::Approved and the plan.approved->Approved, milestone.started/worker.spawned->Running transition
- docs/protocol.md GET /api/missions documentation notes "approved" as a possible status value
- docs/dashboard-reference.md references the Approved status where statuses/pills are enumerated

