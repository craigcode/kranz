# Mission report — m-c22294

**Goal:** Run the engine's existing secret scan over each feature's own commit range when the worker's run ends, using the allowlist pinned at the approval base, so an unwaived finding in a worker-made commit is reported against that feature (fingerprint, rule, location, never the matched text) before the milestone gate, while the merge and push scans stay unchanged.

Branch `kranz/mission-m-c22294` (from `main`). Plan of record: [plan.md](plan.md).

## The plan

2 milestones, 7 features, gated by 18 contract assertions (17 command, 1 judgement).

1. **Sequential missions refuse a worker-committed secret at the feature**
   - Shared range-scan helper with a separate allowlist commit — Repository: Kranz, a Rust workspace (crates/engine is package `kranz-engine`, crates/cli is package `kranz`).
   - Sequential path: scan the feature's commit range before judgement — Repository: Kranz (Rust; crates/engine = package `kranz-engine`).
2. **Parallel worktrees and dispatch-pool candidates are scanned, and the gate docs describe it**
   - Parallel and pool paths: scan each worktree's commit range — Repository: Kranz (Rust; crates/engine = package `kranz-engine`).
   - Document the feature-range scan and recheck the knowledge vault — Repository: Kranz.

**Chosen approach:** A deterministic engine-side scan of each feature's net commit range, immediately before the judgement turn.
**Elapsed:** 10h 39m 16s
**Tokens:** 650 in / 180575 out / 22149788 cache read / 1035474 cache write
**Cost:** $12.08 actual vs $7.76–$38.81 estimated (expected $17.87)

## Workspace
- **Isolation:** `worktree`
- **Worker/validator cwd:** `/var/folders/09/j5btthkd6_qb3trdtjs4wwd80000gn/T/kranz-wt-8fc3563e44c243a57868260c-m-c22294-_integration`
- **Sandbox:** worker `off`; scrutiny `off`; functional `off`
- **Provider:** local-worktree (source isolation) · template: worktree · no workspace contract
- **Workspace contract:** no workspace contract (source isolation only)
- **Preflight:** preflight: 1 issue(s): [warn] command assertion [a13] uses '!', which was not found on PATH

## What shipped

### Milestone 1 — Sequential missions refuse a worker-committed secret at the feature ✅

- ✅ **Shared range-scan helper with a separate allowlist commit** — 1 run
  Repository: Kranz, a Rust workspace (crates/engine is package `kranz-engine`, crates/cli is package `kranz`).
  - `1b1f1f2` [f-1-1] add GitRepo::secret_scan_range with a separate allowlist commit
  - ✓ `cargo test -p kranz-engine --test git_ops_test -- --exact secret_scan_range_reads_allowlist_from_a_separate_commit` passes, and proves waiver only by a fingerprint present at the named allowlist commit, no waiver for None or for an in-range allowlist, and a `path:line` location with the rule id
  - ✓ `cargo test -p kranz-engine --test git_ops_test -- --exact secret_scan_range_errors_on_unreadable_range` passes (Err, never an empty pass)
  - ✓ `range_scan_refuses_a_secret_and_ignores_a_self_added_waiver` and the cli test `push_is_refused_when_the_mission_range_fails_the_secret_scan` still pass unmodified
  - ✓ `git diff --quiet <base> -- crates/engine/src/merge.rs crates/cli/src/exec.rs .kranz/secret-allowlist` exits 0
  - ✓ `kranz scan --range <base>..HEAD` reports 'secret scan passed' over the feature's diff
- ✅ **Sequential path: scan the feature's commit range before judgement** — 3 runs, 2 respawns
  Repository: Kranz (Rust; crates/engine = package `kranz-engine`).
  - `9e23bec` [f-1-2] checkpoint (engine commit)
  - `7e95b84` [f-1-2] checkpoint (engine commit)
  - `2e8a43f` [f-1-2] fix respawn test script order and reword fixture, satisfy fmt/clippy
  - ✓ Each of `feature_range_scan_refuses_worker_committed_canary`, `feature_range_scan_passes_clean_worker_commit`, `feature_range_scan_honours_base_allowlist`, `feature_range_scan_ignores_allowlist_added_in_range`, `feature_range_scan_respawn_after_rewording_completes` and `feature_range_scan_never_quotes_matched_text` passes alone via `cargo test -p kranz-engine --test mission_test -- --exact <name>`
  - ✓ The refusal's feature.failed reason contains fingerprint, rule id and path:line; the milestone is Blocked with cause SecretScan, and no milestone.validating event is emitted
  - ✓ A respawn's worker prompt names the fingerprint and location and never the canary
  - ✓ The existing tests matching `secret_scan_refusal` in mission_test still pass
  - ✓ merge.rs, exec.rs, events.rs and .kranz/secret-allowlist are unchanged; `kranz scan --range <base>..HEAD` passes
- ✅ **Make the separate-allowlist-commit waiver test exercise a range that contains the canary** *(fix)* — 1 run
  Repository: Kranz, a Rust workspace (crates/engine is package `kranz-engine`).
  - `179d692` [ms-1-fix-1-1] exercise the canary in the allowlist waiver test range
  - ✓ `cargo test -p kranz-engine --test git_ops_test -- --exact secret_scan_range_reads_allowlist_from_a_separate_commit` passes
  - ✓ The waiver assertion scans a range whose `from` differs from `to` and includes the canary commit, and is paired with a `None` assertion on the same range expecting exactly one finding
  - ✓ The test has no duplicate assertion block with identical arguments
  - ✓ Only crates/engine/tests/git_ops_test.rs changes; `cargo fmt --all --check` and `cargo clippy --workspace --all-targets -- -D warnings` exit 0; `kranz scan --range <base>..HEAD` prints 'secret scan passed'
- ✅ **Fix finding: a9 / f-1-1: secret_scan_range_reads_allowlist_from_a_separate_commit** *(fix)* — 1 run
  Address this validation finding.
  - ✓ finding 'a9 / f-1-1: secret_scan_range_reads_allowlist_from_a_separate_commit' no longer reproduces
- ✅ **Fix finding: a9 / f-1-1: secret_scan_range_reads_allowlist_from_a_separate_commit** *(fix)* — 1 run
  Address this validation finding.
  - ✓ finding 'a9 / f-1-1: secret_scan_range_reads_allowlist_from_a_separate_commit' no longer reproduces

### Milestone 2 — Parallel worktrees and dispatch-pool candidates are scanned, and the gate docs describe it ✅

- ✅ **Parallel and pool paths: scan each worktree's commit range** — 2 runs, 1 respawn
  Repository: Kranz (Rust; crates/engine = package `kranz-engine`).
  - `638a224` [f-2-1] scan each worktree's own commit range in parallel and pool paths
  - `2d0cc9f` [f-2-1] rename pool-scan test canary var to match allowlisted fixture shape
  - ✓ `cargo test -p kranz-engine --test mission_test -- --exact parallel_worktree_scan_refuses_worker_committed_canary` passes: both features are Failed with reasons naming fingerprint, rule and leak.txt:1, and leak.txt is not on the mission branch
  - ✓ `cargo test -p kranz-engine --lib pool_candidate_line_notes_feature_range_findings` passes: the candidate line notes findings without the canary, and the pool still parks
  - ✓ The existing tests `parallel_secret_scan_refusal_is_recorded_not_swallowed` and the existing dispatch-pool tests still pass
  - ✓ merge.rs, exec.rs, events.rs and .kranz/secret-allowlist are unchanged; `kranz scan --range <base>..HEAD` passes
- ✅ **Document the feature-range scan and recheck the knowledge vault** — 2 runs, 1 respawn
  Repository: Kranz.
  - `b283768` [f-2-2] document the feature-range secret scan as a fourth layer in gates.md
  - `ec500f8` [f-2-2] fix gates.md dispatch-pool unreadable-range wording
  - ✓ `cargo run --locked --package kranz -- knowledge-refresh` exits 0 and prints 'knowledge refresh: ok'
  - ✓ gates.md's secret-scanning section describes the feature-range scan: timing, range, pinned-base allowlist, respawn then fail-and-block, the parallel and pool behaviour, and unchanged merge and push
  - ✓ gates.md has an updated last_verified and a dated Rechecked note
  - ✓ Only files under docs/knowledge/ change in this feature; `kranz scan --range <base>..HEAD` passes

## Validation history

### ms-1 round 1 — Sequential missions refuse a worker-committed secret at the feature

- [major] a9 / f-1-1: secret_scan_range_reads_allowlist_from_a_separate_commit — crates/engine/tests/git_ops_test.rs:1665-1673 — `let tip = commit_b.clone();` then `repo.secret_scan_range(Some(&commit_a), &commit_b, &tip)`, i.e. from==to==commit_b. `diff_full(commit_b, commit_b)` … [truncated]
- [minor] a9 / f-1-1: secret_scan_range_reads_allowlist_from_a_separate_commit — crates/engine/tests/git_ops_test.rs:1691-1709 — two consecutive assertion blocks call `repo.secret_scan_range(Some(&base), &base, &tip)` with identical arguments and identical expected result (len 1),… [truncated]
- [critical] a7 — Engine-captured result: `out=$(cargo test -p kranz-engine --test mission_test -- --exact parallel_worktree_scan_refuses_worker_committed_canary 2>&1) && printf '%s' "$out" | grep -qE 'test result: ok\… [truncated]
- [critical] a8 — Engine-captured result: `out=$(cargo test -p kranz-engine --lib pool_candidate_line_notes_feature_range_findings 2>&1) && printf '%s' "$out" | grep -qE 'test result: ok\. [1-9]'` → FAIL. No output tai… [truncated]

Disposition: 3 fix feature(s) created.

### ms-1 round 2 — Sequential missions refuse a worker-committed secret at the feature

No findings.

### ms-2 round 1 — Parallel worktrees and dispatch-pool candidates are scanned, and the gate docs describe it

- [critical] a16 — cargo test --workspace → FAIL. Captured output: "...cat: .../.tmpJrDsKo/repo/.kranz/domain-terms.local: Operation not permitted\ncat: .../.tmpJrDsKo/repo/.kranz/hook-status/m-x/run-1.json: Operation n… [truncated]

Disposition: waived.
- a16: These failures come from the validator sandbox, not from a product defect. Every error is an OS-level 'Operation not permitted' when existing tests run cat or sh on their own temp-dir files, or create git lock files in scratch repos. That includes `.kranz/serve.token`, `domain-terms.local`, `hook-status`, `control/approve.json`, `index.lock` in a `validator-snapshot-scrutiny` worktree, and `refs/heads/side.lock`. None of these involve the secret-scan code this mission changed. Outside the sandbox, the f-2-1 worker's output shows `cargo test -p kranz-engine --lib` at 1468 passed, 0 failed, and mission_test at 145 passed. A fix feature cannot change sandbox file permissions. a16 still applies at the final gate, and if it fails there outside the sandbox, I will treat that as a real defect.

## Contract outcomes

- ✅ **[a1]** Sequential path: when a worker commits a detector canary inside its session, the feature does not complete. Once the respawn budget is spent it is failed with a reason naming the fingerprint, the rule id and the path:line location, and its milestone blocks without ever reaching validating. *(command: `out=$(cargo test -p kranz-engine --test mission_test -- --exact feature_range_scan_refuses_worker_committed_canary 2>&1) && printf '%s' "$out" | grep -qE 'test result: ok\. [1-9]'`)*
- ✅ **[a2]** Sequential path: a worker commit with no secret findings passes the feature-range scan, and the feature completes through the normal judgement. *(command: `out=$(cargo test -p kranz-engine --test mission_test -- --exact feature_range_scan_passes_clean_worker_commit 2>&1) && printf '%s' "$out" | grep -qE 'test result: ok\. [1-9]'`)*
- ✅ **[a3]** A canary whose fingerprint is on the allowlist committed at the pinned approval base does not fail the feature. *(command: `out=$(cargo test -p kranz-engine --test mission_test -- --exact feature_range_scan_honours_base_allowlist 2>&1) && printf '%s' "$out" | grep -qE 'test result: ok\. [1-9]'`)*
- ✅ **[a4]** An allowlist entry for the canary's fingerprint, added inside the mission's own range (even by the same worker commit), waives nothing: the feature is still refused. *(command: `out=$(cargo test -p kranz-engine --test mission_test -- --exact feature_range_scan_ignores_allowlist_added_in_range 2>&1) && printf '%s' "$out" | grep -qE 'test result: ok\. [1-9]'`)*
- ✅ **[a5]** A finding is reported to the worker: the respawned worker's prompt names the fingerprint and location. When the respawned worker removes the flagged line in a new commit, the feature completes. *(command: `out=$(cargo test -p kranz-engine --test mission_test -- --exact feature_range_scan_respawn_after_rewording_completes 2>&1) && printf '%s' "$out" | grep -qE 'test result: ok\. [1-9]'`)*
- ✅ **[a6]** The matched canary text never appears in events.jsonl or in any worker or orchestrator session prompt during a feature-range refusal. *(command: `out=$(cargo test -p kranz-engine --test mission_test -- --exact feature_range_scan_never_quotes_matched_text 2>&1) && printf '%s' "$out" | grep -qE 'test result: ok\. [1-9]'`)*
- ✅ **[a7]** Parallel path: a worker that commits a canary in its parallel worktree has its feature failed, with a reason naming fingerprint, rule and location. Its branch is never merged into the mission branch. *(command: `out=$(cargo test -p kranz-engine --test mission_test -- --exact parallel_worktree_scan_refuses_worker_committed_canary 2>&1) && printf '%s' "$out" | grep -qE 'test result: ok\. [1-9]'`)*
- ✅ **[a8]** Dispatch pool: a candidate whose committed range carries an unwaived finding has the fingerprint, rule and location noted on its line in the dispatch decision record. The record does not quote the matched text. *(command: `out=$(cargo test -p kranz-engine --lib pool_candidate_line_notes_feature_range_findings 2>&1) && printf '%s' "$out" | grep -qE 'test result: ok\. [1-9]'`)*
- ✅ **[a9]** The shared range-scan helper takes its allowlist from a separately named commit, waives only fingerprints present at that commit, and returns an error (never an empty pass) when the range cannot be read. *(command: `out=$(cargo test -p kranz-engine --test git_ops_test -- --exact secret_scan_range_reads_allowlist_from_a_separate_commit 2>&1) && printf '%s' "$out" | grep -qE 'test result: ok\. [1-9]' && out=$(cargo test -p kranz-engine --test git_ops_test -- --exact secret_scan_range_errors_on_unreadable_range 2>&1) && printf '%s' "$out" | grep -qE 'test result: ok\. [1-9]'`)*
- ✅ **[a10]** The merge and push scans are unchanged and still refuse the same content: merge.rs and exec.rs are byte-identical to the base, and the existing merge, push and range-scan refusal tests pass. *(command: `git diff --quiet $KRANZ_BASE_SHA -- crates/engine/src/merge.rs crates/cli/src/exec.rs && out=$(cargo test -p kranz-engine --test merge_test secret 2>&1) && printf '%s' "$out" | grep -qE 'test result: ok\. [3-9]' && out=$(cargo test -p kranz-engine --test git_ops_test -- --exact range_scan_refuses_a_secret_and_ignores_a_self_added_waiver 2>&1) && printf '%s' "$out" | grep -qE 'test result: ok\. [1-9]' && out=$(cargo test -p kranz push_is_refused_when_the_mission_range_fails_the_secret_scan 2>&1) && printf '%s' "$out" | grep -qE 'test result: ok\. [1-9]'`)*
- ✅ **[a11]** The engine's existing dirty-tree checkpoint refusals still behave as before (sequential fail-and-block, parallel recorded refusal). *(command: `out=$(cargo test -p kranz-engine --test mission_test secret_scan_refusal 2>&1) && printf '%s' "$out" | grep -qE 'test result: ok\. [3-9]'`)*
- ✅ **[a12]** The mission itself adds no unwaived secret finding and does not touch the secret allowlist, so its own merge scan will pass. *(command: `git diff --quiet $KRANZ_BASE_SHA -- .kranz/secret-allowlist && cargo run -q --locked -p kranz -- scan --range $KRANZ_BASE_SHA..HEAD`)*
- ✅ **[a13]** Contract files change additively only: docs/protocol.md, crates/engine/src/events.rs and crates/engine/schemas/ have no removed lines relative to the base. *(command: `! git diff -U0 $KRANZ_BASE_SHA -- docs/protocol.md crates/engine/src/events.rs crates/engine/schemas | grep -vE '^--- (a/|/dev/null)' | grep -qE '^-'`)*
- ✅ **[a14]** Formatting is clean across the workspace. *(command: `cargo fmt --all --check`)*
- ✅ **[a15]** Clippy passes across the workspace with warnings denied. *(command: `cargo clippy --workspace --all-targets -- -D warnings`)*
- ✅ **[a16]** The full workspace test suite passes. *(command: `cargo test --workspace`)*
- ✅ **[a17]** The knowledge vault is fresh: no note citing a file this mission changed is left stale or unverifiable. *(command: `cargo run --locked --package kranz -- knowledge-refresh`)*
- ✅ **[a18]** docs/knowledge/validation/gates.md accurately describes the new feature-range scan. It covers when it runs (each worker run, before judgement, on sequential and parallel paths; record-only for pool candidates), the range it reads (the feature's own base..HEAD net diff), the allowlist source (the pinned approval base; in-range edits waive nothing), what happens on a finding (respawn with the findings as guidance, then fail-and-block), and that the merge and push scans are unchanged. *(agent judgement)*

All assertions passed at the final contract gate (waivers, if any, appear in the validation history).
