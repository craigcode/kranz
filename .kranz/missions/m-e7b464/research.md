# Research — m-e7b464

Evidence behind the approved plan (roadmap M1 / repo-knowledge-store slice 1). Candidate knowledge updates feed `docs/knowledge/`.

## Files & docs read

- crates/engine/src/orchestrator.rs
- crates/engine/src/orchestrator/finalization.rs
- crates/engine/src/git_ops.rs
- crates/engine/src/events.rs
- crates/engine/src/reducer.rs
- crates/engine/src/merged.rs
- crates/cli/src/tail.rs
- apps/dashboard/src/components/ProgressLog.tsx
- crates/engine/tests/mission_test.rs
- crates/engine/tests/worktree_isolation.rs
- docs/knowledge/decisions/inviolable-invariants.md
- .github/workflows/ci.yml

## External sources

- mission m-dd7d22 event log excerpt in the goal (seq 691, 703, 1056)
- lessons m-6112e3, m-dd7d22, m-777557, m-ec06f2, m-c22294

## Facts

- tag_milestone runs a plain `git tag -a`, so a second completion hits 'already exists', which falls into the same warn branch as a real failure and is recorded as tag None — `crates/engine/src/orchestrator.rs:7386-7395; crates/engine/src/git_ops.rs:1887-1895`
- Only the last milestone is reopened after completion: the final-gate fix path emits milestone.validating and then fix features; the completion-review drift check can reopen it with no new commit — `crates/engine/src/orchestrator/finalization.rs:954, 993, 1044; reducer.rs:609-612`
- No code resolves the milestone tag to pick a commit: merged.rs uses branch tips, report_render and reducer ignore the field, tail.rs prints the name, and the dashboard ProgressLog does not show it — `crates/engine/src/merged.rs:27-38; reducer.rs:758; cli/src/tail.rs:634; apps/dashboard/src/components/ProgressLog.tsx:153`
- The tamper fingerprint includes refs/tags, but tagging happens after validator sessions and completion review, at the same point where the tag is already created today — `crates/engine/src/git_ops.rs:931-942; orchestrator.rs:6786-6801`
- Adding fields to MilestoneCompleted breaks struct literals in about 10 files across the engine and the CLI — `grep MilestoneCompleted: cli_test.rs, otel/map.rs, otel/run.rs, corpus_export.rs, trace_export.rs, reviewer_independence_tests.rs, orchestrator.rs, report_test.rs, reducer_test.rs, config_cost_prompts_test.rs, tail.rs`
- mission_test runs in checkout mode; worktree_isolation.rs has worktree_cfg and the primary-untouched patterns — `crates/engine/tests/mission_test.rs:207-219; worktree_isolation.rs:389, 1224, 1882`
- Kranz only ever pushes a single kranz/* branch with --no-follow-tags, so moving a local tag rewrites nothing published — `crates/engine/src/git_ops.rs:2563-2572; docs/knowledge/decisions/inviolable-invariants.md:175-188`

## Ambiguities & stale docs

- The user did not answer the planning questions (move vs. new ref, archival ref, field names); the plan uses the proposed defaults: move, no archival ref, and the names tagCommit/retaggedFrom/staleTag
- Whether features in one milestone run in parallel worktrees is not specified; features are split so that same-milestone features touch disjoint files

## Candidate knowledge updates

- docs/knowledge/architecture/mission-pipeline.md: note that milestone tags are moved to the last-passing commit on re-completion, with the history in the milestone.completed tagCommit/retaggedFrom/staleTag fields
