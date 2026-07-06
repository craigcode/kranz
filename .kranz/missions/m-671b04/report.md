# Mission report — m-671b04

**Goal:** Close M7 tier-1's visibility gap: add an engine-computed out-of-contract-write sweep (worker worktree diffs vs a plan-declared touch-set + a primary-checkout cleanliness check) surfaced as a new finding class, and give worker sessions a scratch HOME/CLAUDE_CONFIG_DIR carrying only the documented minimal set the claude CLI needs.

Branch `kranz/mission-m-671b04` (from `main`). Plan of record: [plan.md](plan.md).

**Elapsed:** 1h 53m 41s
**Tokens:** 51505 in / 218739 out / 21246205 cache read / 948065 cache write
**Cost:** $62.98 actual vs $9.18–$45.88 estimated (expected $18.35)

## What shipped

### Milestone 1 — Out-of-contract write detection ✅

- ✅ **Data-model foundations: plan touch-set + out-of-contract-write finding class** — 3 runs, 2 respawns
  - `696aedc` [f-1-1] fix clippy items_after_test_module lint in runner.rs
- ✅ **Engine post-run out-of-contract-write sweep + primary-checkout cleanliness** — 2 runs, 1 respawn
  - `57d6531` [f-1-2] fix primary-checkout sweep to ignore untracked mission housekeeping files

### Milestone 2 — Worker env hygiene ✅

- ✅ **Probe & document the minimal claude-CLI env set (open question 1)** — 1 run
  - `e3db4b1` [f-2-1] probe and document minimal claude-CLI env set, add engine constant
- ✅ **Seed worker sessions with a scratch HOME/CLAUDE_CONFIG_DIR carrying only the minimal set** — 1 run
  - `4da9918` [f-2-2] seed worker sessions with scratch HOME/CLAUDE_CONFIG_DIR
- ✅ **Preserve worker git-commit identity under scratch HOME + honor CLAUDE_CONFIG_DIR as credential source** *(fix)* — 1 run
  - `fba103e` [ms-2-fix-1-1] fix worker env hygiene: preserve git identity, honor CLAUDE_CONFIG_DIR source

## Validation history

### ms-1 round 1 — Out-of-contract write detection

No findings.

### ms-2 round 1 — Worker env hygiene

- [critical] f-2-2 / worker env hygiene — integration seam (worker git commits) — seed_worker_env (runner.rs:641) overrides the worker's HOME/CLAUDE_CONFIG_DIR with a scratch dir seeded only with .claude/.credentials.json (backend_claude::seed_worker_scratch_home copies just claude… [truncated]
- [minor] a9 — real worker auth: credential source hardcoded to $HOME/.claude — seed_worker_scratch_home (backend_claude.rs) copies the allowlist from real_home.join(".claude") only, and seed_worker_env derives real_home from the HOME env var. It ignores an operator's CLAUDE_CONF… [truncated]

Disposition: 1 fix feature(s) created.

### ms-2 round 2 — Worker env hygiene

No findings.

## Contract outcomes

- ✅ **[a1]** A plan-declared touch-set (glob patterns of allowed repo-relative paths) is a first-class part of the plan data model: it round-trips through plan.json serde exactly as command_grants does, and the reducer folds it onto Mission on PlanApproved. Plans without a touch-set still deserialize (backward compatible). *(command: `cargo test -p kranz-engine touch_set 2>&1 | grep -qE 'test result: ok\. [1-9]'`)*
- ✅ **[a2]** Findings carry a machine-readable class field; a Finding tagged out-of-contract-write round-trips through serde and the validator_report_schema, and existing findings that omit class still deserialize (backward compatible). *(command: `cargo test -p kranz-engine finding_class 2>&1 | grep -qE 'test result: ok\. [1-9]'`)*
- ✅ **[a3]** The post-run sweep flags worker-authored changed paths that match none of the declared touch-set globs as out-of-contract-write findings, and produces no finding for paths that do match; when the touch-set is empty/absent the path comparison is skipped (no findings) rather than flagging everything. *(command: `cargo test -p kranz-engine out_of_contract 2>&1 | grep -qE 'test result: ok\. [1-9]'`)*
- ✅ **[a4]** The sweep asserts primary-checkout cleanliness: a dirty primary checkout or one whose branch moved from mission start yields a critical primary-checkout out-of-contract-write finding, while a clean, unmoved primary checkout yields none. *(command: `cargo test -p kranz-engine primary_checkout 2>&1 | grep -qE 'test result: ok\. [1-9]'`)*
- ✅ **[a5]** Engine/meta commits (approved-plan, mission-report, and other [kranz]-authored commits touching plan.json/plan.md/index.md/report.md) are exempt from the sweep and never produce out-of-contract-write findings, even though they change paths outside the touch-set. *(command: `cargo test -p kranz-engine engine_commit_exempt 2>&1 | grep -qE 'test result: ok\. [1-9]'`)*
- ✅ **[a6]** The minimal file/dir set the claude CLI needs to authenticate and run headless is encoded as a stable engine constant/function that is non-empty and includes the credential entry. *(command: `cargo test -p kranz-engine claude_min_set 2>&1 | grep -qE 'test result: ok\. [1-9]'`)*
- ✅ **[a7]** Worker sessions are spawned with HOME and CLAUDE_CONFIG_DIR pointing at a per-session scratch directory seeded with exactly the allowlisted minimal entries (allowlisted fixtures present, non-allowlisted operator files absent), and KRANZ_BASE_SHA / existing contract env still flow into the worker. *(command: `cargo test -p kranz-engine worker_env_hygiene 2>&1 | grep -qE 'test result: ok\. [1-9]'`)*
- ✅ **[a8]** docs/scoping/claude-cli-min-env.md concretely answers worker-sandboxing open question 1 — it names the actual credential path(s) (e.g. ~/.claude/.credentials.json), the CLAUDE_CONFIG_DIR relocation semantics, and the macOS-Keychain-vs-file distinction — and docs/scoping/worker-sandboxing.md records the resolution of open question 1. *(agent judgement)*
- ✅ **[a9]** A real worker session still authenticates and runs headless under the scratch HOME/CLAUDE_CONFIG_DIR, and the scratch HOME leaks no operator dotfiles beyond the documented minimal set (judged against the seeding logic, the probe doc, and the diff). *(agent judgement)*
- ✅ **[a10]** The full workspace test suite stays green (no regressions in the engine or its consumers). *(command: `cargo test --workspace 2>&1 | grep -qE 'test result: ok\.'`)*
- ✅ **[a11]** The dashboard TypeScript type mirror for the touch-set and finding-class additions type-checks cleanly. *(command: `cd apps/dashboard && npx tsc --noEmit`)*

All assertions passed at the final contract gate (waivers, if any, appear in the validation history).
