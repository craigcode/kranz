# Research — m-e36de6

Evidence behind the approved plan (roadmap M1 / repo-knowledge-store slice 1). Candidate knowledge updates feed `docs/knowledge/`.

## Files & docs read

- crates/engine/src/orchestrator.rs
- crates/engine/src/runner.rs
- crates/engine/src/backend.rs
- crates/engine/tests/mission_test.rs
- crates/engine/tests/runner_test.rs
- AGENTS.md
- .kranz/tickets/validator-park-only-when-denial-stopped-it.md
- docs/scoping/worker-long-command-stall.md
- docs/knowledge/validation/gates.md
- docs/knowledge/architecture/mission-pipeline.md
- CHANGELOG.md
- .kranz/missions/m-7886d9/events.jsonl

## External sources

- .kranz/missions/m-7886d9/events.jsonl
- .kranz/lessons/

## Facts

- maybe_park_for_grant offers denied_commands.first() on any untrusted validator outcome, before and after the single retry. — `crates/engine/src/orchestrator.rs:2381-2392, 6528, 6635`
- The runner collects denied shell commands across the whole session with positional ToolUse/ToolResult correlation and no record of later tool calls. — `crates/engine/src/runner.rs:519-633`
- In m-7886d9 run 266c60e5 the last denial was at seq 507, about 33 tool calls followed, the last call was an async Agent at seq 572, the run ended partial at 574 and grant.requested for the seq-470 grep line came at 575. — `.kranz/missions/m-7886d9/events.jsonl lines 470-575`
- run_outcome_summary is shared with worker judgement, so the validator end description must be a separate function. — `crates/engine/src/judgement.rs:79`
- Only four RunOutcome struct literals exist: one in runner.rs and three in orchestrator.rs tests. — `grep 'RunOutcome {' crates/`
- Contract files are events.rs, types.rs, backend.rs, error.rs; RunOutcome lives in runner.rs. — `AGENTS.md rule 6; .kranz/tickets/hardening-dashboard-decisions.md:25`
- Default grant wait is DEFAULT_GRANT_REQUEST_TIMEOUT = 3600s, overridable by grantRequestTimeoutSecs. — `crates/engine/src/orchestrator.rs:152; crates/engine/src/types.rs:1799`

## Ambiguities & stale docs

- Lesson m-dd7d22 recommends pre-declaring piped git diff | grep commandGrants; the CHANGELOG now says validators have a sanctioned `git diff $KRANZ_BASE_SHA..HEAD` form and pipes stay denied, so no piped grants are declared here.
- The worker deny-grant path has the same first-denial shape; left out of scope per the ticket's validator focus and recorded as open.
- Denial attribution inside a parallel tool batch can name the wrong call; pre-existing and unchanged.

## Candidate knowledge updates

- docs/knowledge/architecture/mission-pipeline.md: recheck paragraph describing the stopping-denial grant rule.
- docs/knowledge/validation/gates.md: a short note under 'What a validator is allowed to run' that a grant is offered only for the denial that stopped the session.
