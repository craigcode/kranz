# Research — m-c22294

Evidence behind the approved plan (roadmap M1 / repo-knowledge-store slice 1). Candidate knowledge updates feed `docs/knowledge/`.

## Files & docs read

- docs/knowledge/validation/gates.md
- docs/merge-gates.md
- crates/engine/src/git_ops.rs
- crates/engine/src/merge.rs
- crates/engine/src/orchestrator.rs
- crates/engine/src/judgement.rs
- crates/engine/src/scrub.rs
- crates/engine/src/reducer.rs
- crates/engine/src/backend_mock.rs
- crates/cli/src/commands.rs
- crates/engine/tests/git_ops_test.rs
- crates/engine/tests/mission_test.rs
- .kranz/secret-allowlist

## External sources

- docs/knowledge/validation/gates.md#secret-scanning--three-layers
- docs/merge-gates.md

## Facts

- The push scan reads the allowlist from the pinned base, but the range start and the allowlist commit are the same parameter — `crates/engine/src/git_ops.rs:1280-1296 secret_scan_range_refusal; called from crates/cli/src/exec.rs:508`
- The merge scans diff_full(base_sha, mission_tip) with the allowlist from the pinned live base — `crates/engine/src/merge.rs:520-528`
- The checkpoint scan covers only the engine's own dirty-path commit and reads the allowlist from the working tree, not the base — `crates/engine/src/git_ops.rs:1211-1268 secret_scan_refusal`
- The sequential path judges a run via judge_worker_run after the dirty-tree checkpoint and record_feature_progress; the worker's own commits are never scanned there — `crates/engine/src/orchestrator.rs:3810-3910`
- Parallel worktrees judge at append_and_judge_worktree; NotReady features fail with a generic reason and are never merged — `crates/engine/src/orchestrator.rs:5404-5476, 5198-5226`
- Dispatch-pool candidates never complete a feature; they park for a human — `crates/engine/src/orchestrator.rs:4407-4553`
- Fingerprints are rule+value, and format_findings prints fingerprint, rule, path:line and offsets without the value — `crates/engine/src/scrub.rs:400, 868-879, 749-789`
- The mock backend can make a worker-shaped commit inside a session — `crates/engine/src/backend_mock.rs:171-184 writes_file + commits_all`
- Existing checkpoint refusal tests and the LEAKED_SECRET canary constant live in mission_test section 13 — `crates/engine/tests/mission_test.rs:10427-10694`

## Ambiguities & stale docs

- Q1 (respawn-then-fail-and-block vs immediate fail), Q2 (pool record-only vs out of scope), Q3 (which files are 'contract files') and Q4 (is knowledge-refresh green on main) were not answered before the plan was requested; the plan takes the recommended defaults: respawn-then-fail-and-block, pool record-only, contract files = docs/protocol.md + crates/engine/src/events.rs + crates/engine/schemas/, knowledge-refresh as a command assertion.
- The checkpoint scan reads the allowlist from the working tree while merge and push read the base; this plan leaves it unchanged, but the new range scan rechecks checkpoint commits against the pinned base, so a mission-added allowlist line no longer slips past at the feature level.
- The cargo test --workspace and clippy assertions each run under the 10-minute contract command timeout.

## Candidate knowledge updates

- docs/knowledge/validation/gates.md: change 'Secret scanning — three layers' to four, adding the feature-range scan (done by feature 4)
- A lesson candidate: test fixtures in a Kranz mission must reuse already-allowlisted canary values, because the mission's own merge scan judges them against the base allowlist and in-mission allowlist edits waive nothing
