# Research — m-d1e3c3

Evidence behind the approved plan (roadmap M1 / repo-knowledge-store slice 1). Candidate knowledge updates feed `docs/knowledge/`.

## Files & docs read

- crates/engine/src/events.rs
- crates/engine/src/types.rs
- crates/engine/src/trace_export.rs
- crates/engine/src/control.rs
- crates/server/src/rest.rs
- crates/cli/src/cli.rs
- crates/slack/src/inbound.rs
- apps/dashboard/src/ (component + lib listing)
- AGENTS.md
- .kranz/tickets/flight-surgeon-console.md
- .kranz/tickets/outcomes-view.md

## External sources

- .kranz/tickets/flight-surgeon-console.md
- .kranz/tickets/outcomes-view.md
- AGENTS.md

## Facts

- Every required metric folds from existing events; no new event kinds are needed. — `events.rs:83-111 (grant.requested/approved/denied), :223-236 (milestone.blocked/unblocked), :61-72 (plan.revision.proposed/plan.revised/plan.revision.rejected), :256 (user.message), :52 (plan.approved), :277-285 (mission.completed/failed/abandoned)`
- Operator interventions map to concrete control commands that land as those events. — `crates/engine/src/types.rs:686 ControlCommand { Msg, ApproveRevision, RejectRevision, ApproveGrant, DenyGrant, ... }`
- Cross-mission enumeration already has a canonical pattern to mirror. — `crates/server/src/rest.rs:40-81 list_missions uses MissionPaths::list_missions + orchestrator::mission_index_ids over .kranz/missions/index.md, skipping missions with no events file`
- A pure log→artifact fold with no second source of truth is the established precedent. — `crates/engine/src/trace_export.rs:1-12 (regenerable, folds MissionState, no persisted second source)`
- events.rs and types.rs are additive-only contract files that must not be edited in implementation. — `AGENTS.md rule 6; events.rs:3 'CONTRACT FILE — do not modify in implementation phases'`
- The Slack bridge can reuse the engine fold directly (no Slack-specific math) because it holds the host. — `crates/slack/src/inbound.rs status/todo handlers (inbound.rs:918-966) run over the bridge's Arc<MissionHost>`
- Absence assertions must diff against the pinned base SHA, not a moving branch. — `kranz lesson m-c9c915 (KRANZ_BASE_SHA); used in a8`

## Ambiguities & stale docs

- `kranz outcomes` collides with the unbuilt outcomes-view.md ticket, which also defines the command and an autonomy ratio. Operator was asked to choose ownership but instructed the plan be emitted without answering; default taken: THIS mission owns `kranz outcomes` with the three consent sections and the canonical autonomy-ratio fold, structured so outcomes-view can later add cost-per-change and cycle-time sections to the same command.
- Closed-mission denominator for the autonomy ratio: defaulted to terminal = completed | failed | abandoned (not completed-only).
- Which user.message events count as interventions: defaulted to only those after plan.approved (execution-phase), so planning conversation does not inflate the ratio.

## Candidate knowledge updates

- Record that `kranz outcomes` is the flight-surgeon consent surface and that outcomes-view is expected to extend the same command (avoid a future second command / second autonomy-ratio definition).
- Document the canonical autonomy-ratio definition (closed-mission denominator; post-approval intervention rule) so future planners reuse it rather than re-deriving.
