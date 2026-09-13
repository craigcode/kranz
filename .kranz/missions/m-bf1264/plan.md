# Mission plan — m-bf1264

**Goal:** A mission whose deliverable diff against the pinned base SHA is empty (no non-meta feature commits on the mission branch) must terminate Failed with an honest note, never COMPLETE, regardless of what the contract gate reports.

Branch `kranz/mission-m-bf1264` (from `main`). Approved plan of record; the machine-readable twin is [plan.json](plan.json). Live status: `kranz status` or the dashboard.

## Cost estimate

Estimated **$7.61 – $38.06** (expected ~$15.22). Rough estimate — live usage is authoritative; based on 37 completed mission(s).

## Validation contract

Defined before any feature; gates mission completion.

- **[a1]** A full mock mission whose worker produces no deliverable commit terminates Failed: it emits mission.failed and never emits mission.completed. 
  `cargo test -p kranz-engine empty_deliverable_mission_terminates_failed 2>&1 | grep -qE 'test result: ok\. [1-9][0-9]* passed'`
- **[a2]** A full mock mission whose worker delivers a real (non-meta) commit is unaffected by the safety net and still terminates Complete. 
  `cargo test -p kranz-engine delivering_mission_still_completes 2>&1 | grep -qE 'test result: ok\. [1-9][0-9]* passed'`
- **[a3]** The whole Rust workspace test suite passes, proving the safety net did not break any existing full-mission test (the Complete-asserting tests now deliver real commits). 
  `cargo test --workspace`
- **[a4]** The mission.failed reason on the empty-deliverable path is an honest, specific note that names the empty deliverable (states that base_sha..HEAD carried zero feature commits / only engine-meta commits), not a generic error string. *(agent judgement)*

## Milestone 1 — Mock workers can deliver real commits

### 1.1 Add a file-write side-effect to the mock backend

In crates/engine/src/backend_mock.rs, give a scripted worker session the ability to write files into its session working directory, so that a 'delivering' mock mission produces a REAL commit on the mission branch (the engine's existing §4.4 dirty-tree discipline in orchestrator.rs run_feature checkpoints any dirty tree as a '[<feature-id>] checkpoint (engine commit)' commit — a non-meta commit).

BACKGROUND: Today mock worker sessions touch no files, so every full-mission mock test completes with an EMPTY deliverable diff (see the comment at mission_test.rs 'Both features completed with no commits (mock workers touch nothing)'). The upcoming empty-diff safety net (a later milestone) will treat that as a vacuous mission. This feature adds the seam that lets a mock mission genuinely deliver.

IMPLEMENT: Add an optional list of (relative_path, contents) writes to MockScript (a field plus an ergonomic builder method, e.g. `writes_file(path, contents)`, matching the existing builder style — single_shot/streaming/with_exit/rendezvous). When the MockSession is started via AgentBackend::start(spec), write each file into the SessionSpec working directory (its cwd), creating parent directories as needed. When the writes list is empty, behaviour must be byte-for-byte unchanged (the default). Do not change any existing scripted-event behaviour.

NOTE the seam carefully: the worker session runs in `session_cwd` (a git worktree in worktree mode, or the primary root in checkout mode); the engine checks `active_repo().is_clean()` right after the run and checkpoints a dirty tree. Writing into the session's cwd is what makes that tree dirty. Confirm the SessionSpec exposes the working directory and use it; do not hardcode a path.

TESTS: (1) In crates/engine/tests/backend_mock_test.rs, a unit test that a MockScript configured with a file write causes that file to exist in the session cwd after the session runs. (2) In crates/engine/tests/mission_test.rs, an integration test that a full mock mission whose worker script writes a file lands at least one NON-'[kranz]' commit in base_sha..HEAD on the mission branch AND still reaches MissionStatus::Complete. Use crate::contract_sweep::is_meta_commit or a direct subject-prefix check to distinguish meta from feature commits. Reuse the existing test helpers/harness in those files (init_repo, make_engine, simple_plan, orch_script, etc.).

Do NOT add any empty-diff gate in this feature — that is a later milestone. The whole engine suite must stay green.

Done when:
- A MockScript configured to write a file causes that file to appear in the session's working directory after the session runs (unit test in backend_mock_test.rs).
- A full mock mission whose worker writes a file lands at least one non-'[kranz]' commit in base_sha..HEAD on the mission branch and still reaches MissionStatus::Complete (integration test in mission_test.rs).
- cargo test -p kranz-engine passes (exit 0) with no gate added.


## Milestone 2 — Empty-deliverable missions terminate Failed honestly

### 2.1 Migrate existing full-mission mock tests to deliver a real commit

Using the mock file-write capability added in the previous milestone, make every full-mission mock test that asserts MissionStatus::Complete land a REAL feature commit, so those genuine missions continue to Complete once the empty-diff safety net is added in the next feature.

SCOPE: The 'passing worker' mock scripts across the full-mission integration tests. Concretely: the three `worker_pass()` helpers at crates/engine/tests/mission_test.rs (~line 167), crates/engine/tests/soak_test.rs (~line 142), and crates/engine/tests/worktree_isolation.rs (~line 191), plus any other bespoke passing-worker scripts used by tests in mission_test.rs, soak_test.rs, ticket_queue_test.rs, and worktree_isolation.rs that lead to a MissionStatus::Complete assertion (there are ~37 Complete assertions total across these files). Each passing worker must write a small file so its feature produces a non-meta commit.

IMPORTANT: use a UNIQUE file path per feature/worker (e.g. derived from the feature id or a counter) so that worktree-mode and parallel-batch tests, which merge multiple per-feature branches, do not hit spurious merge conflicts on the same path.

DO NOT touch scripts whose worker is intentionally non-delivering because the test asserts Failed/Blocked/respawn (e.g. worker_fail and respawn-to-fail scenarios) — leave those exactly as they are.

DO NOT add the empty-diff gate in this feature (that is the next feature). This is a test-only change with no production-code behaviour change; the whole workspace suite must stay green.

Done when:
- cargo test --workspace passes (exit 0) after the migration, with no production-code gate present.
- Every full-mission test that asserts MissionStatus::Complete now runs a worker that writes a file (grep-able change in the passing-worker helpers), so its mission branch carries a non-'[kranz]' commit.
- Tests that intentionally assert Failed/Blocked (non-delivering workers) are left unchanged.

### 2.2 Add the empty-deliverable safety net to the final gate

Add a deterministic non-emptiness safety net to crates/engine/src/orchestrator.rs `final_gate()` so a mission with an empty deliverable diff terminates Failed, independent of and BEFORE the contract assertions.

WHERE: At the very top of `final_gate()`, after the MissionValidating event is (re)emitted but BEFORE running any `command` assertion or any `agent-judgement` verdict turn. It must run first so a green contract can never override it.

WHAT: Resolve the base ref as `self.state.mission.base_sha` when it is Some and non-empty, else fall back to `self.state.mission.base_branch`. Get the commits in `base..HEAD` via `self.active_repo().commits_between(&base, "HEAD")` (the same active_repo the gate already uses for diff_stat, so worktree mode is handled). Filter out engine/meta commits with `crate::contract_sweep::is_meta_commit(&commit.subject)` (meta = subjects starting with '[kranz]', e.g. the '[kranz] approved plan' commit that lands after base_sha). If ZERO non-meta commits remain, the deliverable diff is empty: emit `EventKind::MissionFailed { reason }` and return `Ok(Some(MissionStatus::Failed))`.

HONEST NOTE: the `reason` must be specific and name the empty deliverable — e.g. "no deliverable commits landed on the mission branch: base_sha..HEAD contains 0 feature commits (only engine/meta commits). Refusing to COMPLETE on an empty deliverable diff." Do NOT write a mission report on this path (report-writing belongs to complete_mission, the completion path only).

This exactly catches the m-66aff8 case, where the branch carried only the '[kranz] approved plan' commit. It does NOT affect genuine doc-only missions, which still change files outside .kranz/ and therefore have non-meta commits.

TESTS in crates/engine/tests/mission_test.rs (reuse the existing harness helpers):
- `empty_deliverable_mission_terminates_failed`: a full mock mission whose worker writes nothing ends at MissionStatus::Failed, emits a `mission.failed` event with a NON-EMPTY reason, and does NOT emit `mission.completed`.
- `delivering_mission_still_completes`: a full mock mission whose worker delivers a file ends at MissionStatus::Complete and does NOT emit `mission.failed` (regression guard that the net is inert for genuine missions).

Also ensure the full workspace suite passes — if the previous feature missed migrating any Complete-asserting test, fix it here (make its worker deliver).

Done when:
- cargo test -p kranz-engine empty_deliverable_mission_terminates_failed passes and actually runs (>=1 test passed): a non-delivering mock mission ends Failed, emits mission.failed with a non-empty reason, and never emits mission.completed.
- cargo test -p kranz-engine delivering_mission_still_completes passes and actually runs: a delivering mock mission ends Complete and emits no mission.failed.
- The emptiness check runs before any contract assertion and reuses contract_sweep::is_meta_commit against base_sha..HEAD (base_sha, falling back to base_branch when absent).
- cargo test --workspace passes (exit 0).

### 2.3 Investigate and note why the contract gate reported green on an empty tree

Write a concise investigation note (a real committed file under docs/, e.g. docs/notes/empty-deliverable-safety-net.md — NOT under .kranz/) that answers the ticket's investigation questions. This is required by the mission's acceptance hints ('Investigate and note how the contract gate reported green on an empty [tree]').

The note must cover:
1. WHY m-66aff8's contract gate reported all-green against an empty deliverable tree. Read final_gate() and the mission's contract; explain that the contract's command assertions asserted PRE-EXISTING behaviour (e.g. `cargo test --workspace` passes on the base tree), so an empty deliverable still satisfied every assertion. Distinguish this from the new safety net, which is contract-independent.
2. How this relates to the anti-vacuity contract-grep / anti-vacuity technique in the codebase (see crates/engine/src/contract_sweep.rs and the repo's design notes): the anti-vacuity technique guards against tests that do not actually RUN (a filter matching zero tests, '0 passed'); it does NOT guard against a contract that asserts already-true behaviour on an empty tree. State clearly that the new empty-deliverable gate is the complementary defence.
3. The open question of stale/uncommitted worktree state: investigate (read-only) whether the final gate's contract commands run against the integration worktree HEAD or against a stale/uncommitted tree — trace active_root()/active_repo() and how run_shell_command's cwd is set in final_gate. Record the finding (did it run against stale state: yes/no) with the file:line evidence. If — and only if — this uncovers a genuine SEPARATE bug (gate running against the wrong tree), document it and recommend a follow-up ticket; do NOT fix it in this mission (out of scope).

The note is itself a deliverable file outside .kranz/, so it carries this feature's work past the new empty-deliverable gate.

Done when:
- A committed note file exists under docs/ (outside .kranz/) explaining why the contract reported green on an empty tree and how that differs from / complements the anti-vacuity contract-grep.
- The note records an explicit finding, with file:line evidence, on whether the final gate ran contract commands against stale/uncommitted worktree state, and flags any separate bug as a follow-up ticket rather than fixing it here.
- cargo test --workspace still passes (exit 0).

