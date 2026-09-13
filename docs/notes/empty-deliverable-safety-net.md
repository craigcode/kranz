# Why m-66aff8's contract gate reported green on an empty tree

Investigation for the ticket behind feature f-2-2 (the deterministic
non-emptiness safety net in `final_gate()`) and feature f-2-3 (this note).

## 1. Why the contract was all-green against zero deliverable commits

`final_gate()` (`crates/engine/src/orchestrator.rs:2964`) runs the mission's
`validation_contract` assertions and treats an empty `findings` list as
success (`crates/engine/src/orchestrator.rs:3036`: `if findings.is_empty() {
self.complete_mission().await?; ... }`). For `command` assertions, that means
literally running the shell command in `active_root()` and checking its exit
code (`crates/engine/src/orchestrator.rs:3015`:
`run_shell_command(self.active_root(), command, &env).await`).

m-66aff8's contract commands were things like `cargo test --workspace`
(a workspace-wide regression check) — assertions that the **existing,
already-committed** behaviour of the repo still holds. That is true whether
or not the mission's workers added anything: `cargo test --workspace` passes
on the pinned base tree by construction (it's the tree the mission started
from), so it keeps passing on a mission branch that never diverged from that
base. The contract never asserted "the mission added N commits" or "file X
now exists" — it asserted "the workspace still builds and its tests still
pass", which is a property of the *base* tree, not of the mission's
deliverable. An empty diff trivially satisfies "the tree I started from still
works."

This is a property of what the contract's authors chose to assert, not a bug
in `final_gate()`'s command-running mechanics: the gate did exactly what it
was told to check, and every check happened to be true independent of the
mission's work. **A contract is only as strong as what it asserts**; nothing
in the assertion evaluator distinguished "true because the mission delivered
it" from "true regardless of the mission."

## 2. Relationship to the anti-vacuity contract technique

The anti-vacuity technique (`.kranz/lessons/m-8b7db0.md`, `.kranz/lessons/m-671b04.md`)
is a **contract-authoring discipline**, not an engine mechanism: it tells
plan/contract authors to avoid filtered `cargo test <substr>` assertions that
pass vacuously when the filter matches zero tests (a missing or renamed test
still exits 0 with "0 passed"). The recommended fix is to grep the command's
output for a nonzero passed count (`test result: ok\. [1-9]`) plus bind the
filter substring to a specific feature's test names in
`validationCriteria`, per that lesson.

That guards a **different, narrower** failure mode: "the assertion command
ran but exercised nothing" (0 tests matched). It says nothing about the
failure mode in section 1 above: "the assertion command ran, exercised real
pre-existing tests, and they were always going to pass because they don't
depend on the mission's deliverable at all." A perfectly anti-vacuity-safe
assertion (`cargo test --workspace` with a `result: ok\. [1-9]` guard,
verified to match hundreds of pre-existing tests) still reports green on an
empty deliverable diff, because none of those tests were written to fail in
the empty-diff case.

In short:
- **Anti-vacuity contract-grep** — guards against a command that *looks* like
  it ran an assertion but actually tested nothing (0 passed).
- **Empty-deliverable safety net** (f-2-2, `crates/engine/src/orchestrator.rs:2969-2994`)
  — guards against a command that *did* run real, passing assertions, but
  those assertions were satisfiable by the base tree alone, independent of
  the mission's `base_sha..HEAD` diff.

The two are complementary and neither subsumes the other: a contract could be
anti-vacuity-clean and still pass on an empty tree (m-66aff8's case), and a
contract could reference a real, mission-specific test yet still pass
vacuously if the test-name filter typo'd (the case the lesson warns about).
The new safety net is deliberately **contract-independent** — it runs first,
before any assertion is evaluated (`orchestrator.rs:2975-2994`), and fails the
mission on `non_meta_commit_count == 0` regardless of what the contract
concludes. A green contract can no longer override an empty deliverable.

## 3. Open question: did the gate run against a stale/uncommitted worktree?

Traced `active_root()` / `active_repo()` and how `run()` wires up the
integration worktree, to check whether `final_gate()`'s `command` assertions
could have run against stale or uncommitted state rather than the mission
branch's actual `HEAD`.

**Finding: no, this was not stale-tree bug.** Evidence:

- `active_root()` / `active_repo()` (`crates/engine/src/orchestrator.rs:489-502`)
  resolve to `self.active_tree`, set once per `run()` call in worktree mode
  (`crates/engine/src/orchestrator.rs:1359-1360`:
  `let (path, wt_repo) = self.setup_mission_worktree()?; self.active_tree =
  Some((path, wt_repo));`) and held for the entire `run_loop()` — including
  the same call that eventually reaches `final_gate()`.
- Worker features are integrated into that *same* worktree/repo handle: the
  sequential-merge path calls `self.active_repo().merge_no_ff(&ws.branch)?`
  (`crates/engine/src/orchestrator.rs:2292`), which both updates the branch
  ref and checks out the merged tree into the integration worktree's working
  directory — there is no separate "build tree" that could fall out of sync.
- `final_gate()`'s command assertions run via
  `run_shell_command(self.active_root(), command, &env)`
  (`crates/engine/src/orchestrator.rs:3015`), i.e. in the *same* directory
  that just received every merge for this run. `active_root()` /
  `active_repo()` never change mid-run (no reassignment between
  `setup_mission_worktree` and `teardown_mission_worktree` at
  `crates/engine/src/orchestrator.rs:1391-1399`), so there is no window where
  `final_gate` could see an older commit than the one workers just merged.

So the gate evaluated the actual, current `HEAD` of the mission's integration
worktree at the time `final_gate()` ran — not a stale checkout. This
confirms the diagnosis in section 1 is the complete explanation: the false
green on m-66aff8 was a contract-content issue (assertions that don't depend
on the deliverable), not a tree-freshness bug. No separate defect was found
here, so no follow-up ticket is filed for worktree staleness.
