# Research — m-4ad3ae

Evidence behind the approved plan (roadmap M1 / repo-knowledge-store slice 1). Candidate knowledge updates feed `docs/knowledge/`.

## Files & docs read

- crates/engine/src/orchestrator.rs
- crates/engine/src/runner.rs
- crates/engine/src/backend_mock.rs
- crates/engine/src/permissions.rs
- crates/engine/tests/mission_test.rs
- docs/scoping/validator-park-only-when-denial-stopped-it.md
- .kranz/tickets/worker-deny-grant-only-when-denial-stopped-it.md

## External sources

- m-e36de6 validator fix commits f66fe9a7 and 1afb9749

## Facts

- The worker deny-grant path offers denied_commands.first(), regardless of whether that denial stopped the session. — `crates/engine/src/orchestrator.rs:2488`
- The validator path already keys on stopping_denied_command and emits a 'not offering a grant for' decision with role_label. — `crates/engine/src/orchestrator.rs:2393-2404`
- validator_session_end_summary checks validator_report, which a worker run never sets. — `crates/engine/src/orchestrator.rs:8339`
- stopping_denied_command is Some only when tool_calls_after_last_denial == Some(0). — `crates/engine/src/runner.rs:828-832`
- Respawn credit for grant re-runs comes only from grant_respawns, which is incremented only when the run parks. — `crates/engine/src/orchestrator.rs:810-813, 3990, 4143`
- Bash(git push*) and Bash(git stash*) are both built-in worker deny rules. — `crates/engine/src/permissions.rs:102,104`

## Ambiguities & stale docs

- The operator did not answer whether a stopping denial matching no liftable rule should get a decision. The plan records one, following the scoping answer 'when no grant is offered for a denied command, record one orchestrator decision'.
- The operator did not answer the docs question. The plan updates the stale 'Not changed here' bullet in the validator scoping note and leaves the ticket file untouched.

## Candidate knowledge updates

- The validator scoping note's 'Not changed here' bullet about the worker deny-grant path becomes stale with this mission; it is updated in-mission.
