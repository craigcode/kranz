# Research — m-7886d9

Evidence behind the approved plan (roadmap M1 / repo-knowledge-store slice 1). Candidate knowledge updates feed `docs/knowledge/`.

## Files & docs read

- crates/cli/src/backlog.rs
- crates/cli/src/commands.rs
- crates/cli/src/cli.rs
- crates/engine/src/draft.rs
- crates/engine/src/mission_catalog.rs
- crates/slack/src/commands.rs
- crates/slack/src/bridge.rs
- crates/slack/src/dispatch.rs
- crates/server/src/host.rs
- apps/dashboard/src/lib/pipelineStage.ts
- apps/dashboard/src/components/PipelineView.tsx
- apps/dashboard/src/components/TicketDetail.tsx
- docs/tickets.md
- docs/scoping/mid-mission-replanning.md
- docs/knowledge/validation/gates.md
- docs/knowledge/CONVENTIONS.md
- .kranz/tickets/draft-parked-reshape-hint.md
- .kranz/lessons/index.md

## External sources

- repository (read-only)

## Facts

- The parked-draft hint names `kranz plan --mission <id>` as the way to reshape — `crates/cli/src/backlog.rs:622-626`
- cmd_plan refuses any mission not in Planning status — `crates/cli/src/commands.rs:1141-1146`
- drive_draft approves the plan before parking the ticket in Review — `crates/engine/src/draft.rs:293-334`
- abandon_mission accepts any non-terminal mission and reconciles the ticket (Abandoned maps to Failed) — `crates/engine/src/mission_catalog.rs:134-180`
- drive_draft does not check ticket state and records the new mission on the ticket — `crates/engine/src/draft.rs:175-183`
- The Slack parked-draft message names no reshape route; the other `kranz plan --mission` mentions in Slack apply only to Planning-status missions — `crates/slack/src/commands.rs:73-79; crates/slack/src/dispatch.rs:1141-1152`
- The dashboard's Reviewable 'Reshape' secondary action only links to the ticket page, which offers Approve for REVIEW tickets — `apps/dashboard/src/lib/pipelineStage.ts:122; apps/dashboard/src/components/PipelineView.tsx:433-446; apps/dashboard/src/components/TicketDetail.tsx:102`
- gates.md lists docs/tickets.md in verified_against with check-on-touch freshness — `docs/knowledge/validation/gates.md:4,61`
- No existing test name uses the prefix parked_draft_reshape — `grep parked_draft_reshape crates/ -> 0 matches`

## Ambiguities & stale docs

- Whether a parked draft should be reshapable in place is left as an OPEN operator decision (D-A in docs/scoping/parked-draft-reshape.md)
- The dashboard Reshape button and the Slack parked-draft message are left unchanged by default; the operator gave no direction

## Candidate knowledge updates

- docs/knowledge/surfaces: note that a parked draft (approved mission, ticket REVIEW) is reshaped only by abandon + redraft until D-A in docs/scoping/parked-draft-reshape.md is decided
- docs/scoping/mid-mission-replanning.md:25 and slice 3 still describe the backlog.rs hint as stale; a later docs pass could mark it fixed
