# Research — m-9e4ef3

Evidence behind the approved plan (roadmap M1 / repo-knowledge-store slice 1). Candidate knowledge updates feed `docs/knowledge/`.

## Files & docs read

- crates/engine/src/work.rs
- crates/engine/src/ticket.rs
- crates/engine/src/merged.rs
- crates/engine/src/types.rs
- crates/engine/src/orchestrator.rs
- crates/cli/src/exec.rs
- crates/cli/src/commands.rs
- crates/server/src/host.rs
- apps/dashboard/src/lib/pipelineStage.ts
- crates/slack/src/bridge.rs
- .github/workflows/ci.yml

## External sources

- .github/workflows/ci.yml

## Facts

- Only drain_queue writes a ticket's terminal .status; it maps exit code 0→Done, non-zero→Failed via mission_state_from_code. — `crates/engine/src/work.rs:80-88, :300-313`
- engine.run() returns Ok(MissionStatus::Blocked) for a blocked milestone; exit_code_for(Blocked)=2, so the drain currently marks a blocked ticket Failed. — `crates/engine/src/orchestrator.rs:2438-2441; crates/cli/src/exec.rs:269 (exit_code Blocked=2); work.rs mission_state_from_code`
- Three run() terminal paths never touch the ticket: run_mission_loop (kranz run), exec (kranz exec), run_to_end (REST /start). — `crates/cli/src/commands.rs:903-941; crates/cli/src/exec.rs:216-258; crates/server/src/host.rs:1390-1409`
- A ticket→mission reverse lookup already exists: Ticket::slug_for_mission scans .status files for a recorded mission_id. — `crates/engine/src/ticket.rs:456-489`
- The Delivered/Landed split is a read-time projection off ticket=Done + mission=Complete, so writing only Done preserves it. — `crates/engine/src/merged.rs:38-54; apps/dashboard/src/lib/pipelineStage.ts:87-96`
- TicketState::NeedsContext maps to the needs-you pipeline stage on every surface. — `apps/dashboard/src/lib/pipelineStage.ts:79-80; crates/slack/src/bridge.rs:3652`
- CI gates are cargo fmt --all --check, cargo clippy --workspace --all-targets -- -D warnings, and cargo test --workspace --no-fail-fast. — `.github/workflows/ci.yml (rust job steps)`

## Ambiguities & stale docs

- Blocked ticket target chosen as NeedsContext (needs-you) rather than leaving it Running; both are within the goal's stated latitude. Side effect: work_skip_for_failed_blocker no longer force-skips dependents of a blocked blocker.
- kranz abandon is another non-drain terminal path but is out of scope this mission; the helper maps Abandoned→Failed so it is correct if wired later.

## Candidate knowledge updates

- Add a note to docs/knowledge that ticket terminal .status reconciliation is centralized in work::reconcile_ticket_for_mission (keyed off recorded mission_id + folded status) and must be called by any new mission-terminal path; the Delivered/Landed split stays a read-time projection via merged::ticket_merged.
