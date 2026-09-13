# Mission plan — m-671b04

**Goal:** Close M7 tier-1's visibility gap: add an engine-computed out-of-contract-write sweep (worker worktree diffs vs a plan-declared touch-set + a primary-checkout cleanliness check) surfaced as a new finding class, and give worker sessions a scratch HOME/CLAUDE_CONFIG_DIR carrying only the documented minimal set the claude CLI needs.

Branch `kranz/mission-m-671b04` (from `main`). Approved plan of record; the machine-readable twin is [plan.json](plan.json). Live status: `kranz status` or the dashboard.

## Cost estimate

Estimated **$7.41 – $37.03** (expected ~$14.81). Rough estimate — live usage is authoritative; based on 27 completed mission(s).

## Validation contract

Defined before any feature; gates mission completion.

- **[a1]** A plan-declared touch-set (glob patterns of allowed repo-relative paths) is a first-class part of the plan data model: it round-trips through plan.json serde exactly as command_grants does, and the reducer folds it onto Mission on PlanApproved. Plans without a touch-set still deserialize (backward compatible). 
  `cargo test -p kranz-engine touch_set 2>&1 | grep -qE 'test result: ok\. [1-9]'`
- **[a2]** Findings carry a machine-readable class field; a Finding tagged out-of-contract-write round-trips through serde and the validator_report_schema, and existing findings that omit class still deserialize (backward compatible). 
  `cargo test -p kranz-engine finding_class 2>&1 | grep -qE 'test result: ok\. [1-9]'`
- **[a3]** The post-run sweep flags worker-authored changed paths that match none of the declared touch-set globs as out-of-contract-write findings, and produces no finding for paths that do match; when the touch-set is empty/absent the path comparison is skipped (no findings) rather than flagging everything. 
  `cargo test -p kranz-engine out_of_contract 2>&1 | grep -qE 'test result: ok\. [1-9]'`
- **[a4]** The sweep asserts primary-checkout cleanliness: a dirty primary checkout or one whose branch moved from mission start yields a critical primary-checkout out-of-contract-write finding, while a clean, unmoved primary checkout yields none. 
  `cargo test -p kranz-engine primary_checkout 2>&1 | grep -qE 'test result: ok\. [1-9]'`
- **[a5]** Engine/meta commits (approved-plan, mission-report, and other [kranz]-authored commits touching plan.json/plan.md/index.md/report.md) are exempt from the sweep and never produce out-of-contract-write findings, even though they change paths outside the touch-set. 
  `cargo test -p kranz-engine engine_commit_exempt 2>&1 | grep -qE 'test result: ok\. [1-9]'`
- **[a6]** The minimal file/dir set the claude CLI needs to authenticate and run headless is encoded as a stable engine constant/function that is non-empty and includes the credential entry. 
  `cargo test -p kranz-engine claude_min_set 2>&1 | grep -qE 'test result: ok\. [1-9]'`
- **[a7]** Worker sessions are spawned with HOME and CLAUDE_CONFIG_DIR pointing at a per-session scratch directory seeded with exactly the allowlisted minimal entries (allowlisted fixtures present, non-allowlisted operator files absent), and KRANZ_BASE_SHA / existing contract env still flow into the worker. 
  `cargo test -p kranz-engine worker_env_hygiene 2>&1 | grep -qE 'test result: ok\. [1-9]'`
- **[a8]** docs/scoping/claude-cli-min-env.md concretely answers worker-sandboxing open question 1 — it names the actual credential path(s) (e.g. ~/.claude/.credentials.json), the CLAUDE_CONFIG_DIR relocation semantics, and the macOS-Keychain-vs-file distinction — and docs/scoping/worker-sandboxing.md records the resolution of open question 1. *(agent judgement)*
- **[a9]** A real worker session still authenticates and runs headless under the scratch HOME/CLAUDE_CONFIG_DIR, and the scratch HOME leaks no operator dotfiles beyond the documented minimal set (judged against the seeding logic, the probe doc, and the diff). *(agent judgement)*
- **[a10]** The full workspace test suite stays green (no regressions in the engine or its consumers). 
  `cargo test --workspace 2>&1 | grep -qE 'test result: ok\.'`
- **[a11]** The dashboard TypeScript type mirror for the touch-set and finding-class additions type-checks cleanly. 
  `cd apps/dashboard && npx tsc --noEmit`

## Milestone 1 — Out-of-contract write detection

### 1.1 Data-model foundations: plan touch-set + out-of-contract-write finding class

Add two additive, backward-compatible fields to the engine data model and mirror them, WITHOUT implementing the sweep (that is the next feature). Work is pure-Rust engine plus a TS mirror.

1) Plan-declared touch-set. In crates/engine/src/types.rs (a CONTRACT FILE — additions only, camelCase serde), add to `Plan` (around the existing `command_grants` field, ~types.rs:66-76) a field `touch_set: Vec<String>` with `#[serde(default, skip_serializing_if = "Vec::is_empty")]`, mirroring command_grants exactly. It holds gitignore/glob-style repo-relative path patterns the mission is allowed to touch. Carry it onto `Mission` (~types.rs:48) and fold it in the reducer's `PlanApproved` arm (crates/engine/src/reducer.rs ~:50-82) alongside base_sha/command_grants. Also add `touchSet` to the orchestrator's Plan structured-output JSON schema so future orchestrators may declare it (locate the schema mirroring Plan — search near command_grants / the planning turn's json_schema).

2) Finding class. In types.rs add to `Finding` (~:277-288) a field `class: String` with `#[serde(default)]` (free string, default ""; convention value "out-of-contract-write"; mirrors how `severity` is a free string). Add an OPTIONAL (not required) `class` string property to `validator_report_schema()` in crates/engine/src/runner.rs (~:462-486). Update EVERY literal `Finding { .. }` construction so the crate still compiles — engine-synthesized findings in orchestrator.rs (~:2718, 2728, 2842, 2852, 2864 and the findings-conversion fallback) set `class` to "" (their existing scrutiny/functional/gate semantics are unchanged).

3) Dashboard mirror. In apps/dashboard/src/lib/types.ts add `touchSet?: string[]` to the Plan type and `class?: string` to the Finding type so the web UI types compile (`npx tsc --noEmit` in apps/dashboard must pass).

Constraints: additions must be backward compatible — a plan.json without touchSet and a finding without class must still deserialize. Do not change any behavior yet. Heed the repo meta-lesson: this is engine-API work — gate `cargo` on `--workspace` (or `-p kranz-engine`), never `-p kranz-engine` mis-typed; run the standard `cargo test --workspace 2>&1 | grep -qE 'result: ok\.'` regression pattern (do NOT add `set -o pipefail`). Extending the widely-constructed `Finding` type touches scattered call sites — grep for `Finding {` across the crate and update all of them.

Name your tests so these substrings appear in test names (the contract greps them and requires a nonzero pass count): use `touch_set` for the plan/reducer/serde tests and `finding_class` for the Finding/schema tests.

Done when:
- A `Plan` (and `Mission`) with a populated touchSet serializes to plan.json with key `touchSet` and deserializes back equal; a plan.json omitting touchSet deserializes with an empty touch-set — covered by tests whose names contain `touch_set`.
- The reducer folds touchSet onto Mission on PlanApproved (test name contains `touch_set`).
- The orchestrator's Plan structured-output JSON schema includes an optional `touchSet` array property.
- A `Finding` with `class = "out-of-contract-write"` round-trips through serde, and validator_report_schema() accepts both a finding carrying `class` and one omitting it — covered by tests whose names contain `finding_class`.
- apps/dashboard/src/lib/types.ts declares Plan.touchSet?: string[] and Finding.class?: string, and `cd apps/dashboard && npx tsc --noEmit` passes.
- `cargo test --workspace 2>&1 | grep -qE 'result: ok\.'` passes (no regressions from the added fields / updated Finding constructions).

### 1.2 Engine post-run out-of-contract-write sweep + primary-checkout cleanliness

Implement the deterministic, engine-computed sweep that surfaces out-of-contract writes as findings. Build on the previous feature's touchSet + Finding.class additions. This is engine code (no LLM validator involved) so it is fully unit-testable.

Where it runs: in `validation_round` (crates/engine/src/orchestrator.rs ~:2362-2536), alongside the spawned validator sessions, compute the sweep and append its results as engine findings using `run_id = ENGINE_RUN_ID` ("engine") — exactly as `final_gate` already synthesizes findings (~:2703-2762) — so they flow unchanged through the existing `convert_findings` → fix-features/waiver machinery and emit as `EventKind::ValidationFinding`.

Path sweep: for the milestone commit range `start_sha..HEAD` (Milestone.start_sha), obtain worker-authored changed paths and compare each against the mission's touch_set globs.
- Use `GitRepo::open(<worktree/integration root>)` then `changed_paths(from, to)` (git_ops.rs:237 — `git diff --name-only from..to`). The mission-branch state lives in the integration worktree; use the orchestrator's active_repo()/active_root() accessors (~:474-496) rather than the primary checkout.
- Exclude engine/meta commits and kranz meta paths: any commit authored with the `[kranz]` message convention (approved-plan, mission-report) and the mission meta files (plan.json, plan.md, index.md, report.md) must NOT produce findings, even though they change paths outside the touch-set. Determine worker-vs-engine attribution by commit message prefix (worker commits are `[<feature-id>]`, engine commits are `[kranz] ...`) and/or by filtering the meta paths.
- Any remaining changed path matching none of the touch_set globs → `Finding { class: "out-of-contract-write", severity: "major", subject: <path>, evidence: <commit + path>, suggested_fix: <relocate or declare in touchSet> }`.
- If mission.touch_set is empty/absent: skip the path comparison entirely (emit no path findings) and log that the sweep is advisory-off for lack of a declared touch-set. Still run the cleanliness check below.

Primary-checkout cleanliness: verify the PRIMARY checkout (self.repo / self.paths repo root — NOT any worktree) is clean and on its original branch. Use `self.repo.is_clean()`/`is_clean_tracked()` (git_ops.rs:151/159); for the branch check, add or reuse a helper that reads the primary's current branch and compare it to the branch recorded at mission start (the primary must never move in worktree mode). A dirty primary or a moved branch → `Finding { class: "out-of-contract-write", severity: "critical", subject: "primary-checkout", evidence: <what changed>, .. }`. A clean, unmoved primary → no finding. This asserts the M7-tier-1 guarantee that the primary checkout never changes.

Glob matching: prefer a crate already in the workspace (check crates/engine/Cargo.toml first — e.g. `globset`/`glob`); only add a dependency if none is present, and if you do, note it in the WorkerReport.

Constraints: deterministic and side-effect-free (read-only git); do not alter the validator sessions' own findings; keep the existing no-findings→milestone-complete fast path working when the sweep is clean. Repo meta-lesson: gate cargo on `--workspace`/`-p kranz-engine`. Follow docs/scoping/worker-sandboxing.md — this is honestly-labeled detection (visibility), not containment.

Name tests so these substrings appear: `out_of_contract` (path in vs out of touch-set), `engine_commit_exempt` (meta commits/paths never flagged), `primary_checkout` (dirty/moved vs clean).

Done when:
- A milestone diff containing a changed path outside the declared touch-set produces exactly one out-of-contract-write finding for that path; a changed path inside the touch-set produces none — tests named with `out_of_contract`.
- When the mission touch-set is empty/absent, the path sweep emits no out-of-contract-write path findings (advisory-off) — test named with `out_of_contract`.
- Engine/meta commits and meta paths (plan.json, plan.md, index.md, report.md, `[kranz]`-authored commits) produce no out-of-contract-write findings even when outside the touch-set — tests named with `engine_commit_exempt`.
- A dirty primary checkout or one whose branch moved from mission start yields a critical `primary-checkout` finding; a clean, unmoved primary yields none — tests named with `primary_checkout`.
- The findings are emitted with run_id ENGINE_RUN_ID and flow through the existing convert_findings path (they can be waived or converted to fix-features like any finding).
- `cargo test --workspace 2>&1 | grep -qE 'result: ok\.'` passes.


## Milestone 2 — Worker env hygiene

### 2.1 Probe & document the minimal claude-CLI env set (open question 1)

Answer docs/scoping/worker-sandboxing.md open question 1: the minimal file/dir set the `claude` CLI needs to authenticate and run headless (`claude -p --output-format stream-json`). This is an investigation-plus-code feature.

Investigate (read-only): what the CLI reads under HOME / CLAUDE_CONFIG_DIR — credentials, settings, config. Sources: the CLI's own docs/`--help`, the semantics of the CLAUDE_CONFIG_DIR env var (which relocates the config directory), and read-only inspection of the local ~/.claude layout. Explicitly capture the macOS distinction between file-based credentials (~/.claude/.credentials.json) and Keychain-stored credentials, and note that Keychain auth is HOME-independent while file-based auth follows CLAUDE_CONFIG_DIR/HOME.

Deliverable 1 — docs/scoping/claude-cli-min-env.md: the concrete minimal set (credential path(s), any required settings/config entries), per-platform notes (macOS Keychain vs file), and how CLAUDE_CONFIG_DIR relocation works. Add a short resolution note to docs/scoping/worker-sandboxing.md marking open question 1 answered and pointing at the new doc.

Deliverable 2 — encode the minimal set in engine code as a stable constant or function (e.g. in crates/engine/src/backend_claude.rs or a small new module) that the next feature (scratch-HOME seeding) will consume. It enumerates the relative entry names to carry into a scratch HOME/config (at minimum the credentials entry). Keep it a single source of truth.

Constraints: do NOT wire it into spawning yet (that is the next feature). Do not print or commit any actual secret values — only path/entry names. Repo meta-lesson: gate cargo on `--workspace`/`-p kranz-engine`.

Name tests so the substring `claude_min_set` appears: assert the constant/function is non-empty and includes the credential entry.

Done when:
- docs/scoping/claude-cli-min-env.md exists and concretely names the credential path(s), CLAUDE_CONFIG_DIR relocation semantics, and the macOS Keychain-vs-file distinction.
- docs/scoping/worker-sandboxing.md records open question 1 as resolved with a pointer to the new doc.
- An engine constant/function enumerates the minimal entry names, is non-empty, and includes the credential entry — covered by a test whose name contains `claude_min_set`.
- No secret values are printed or committed (only path/entry names).
- `cargo test --workspace 2>&1 | grep -qE 'result: ok\.'` passes.

### 2.2 Seed worker sessions with a scratch HOME/CLAUDE_CONFIG_DIR carrying only the minimal set

Give WORKER sessions an isolated scratch HOME/CLAUDE_CONFIG_DIR containing only the documented minimal set, consuming the constant/function from the previous feature.

Mechanism: the child inherits the parent env wholesale and the engine layers extra vars via `.envs(&spec.env)` with NO env_clear (backend_claude.rs ~:606-626). So for worker sessions only, set `spec.env["HOME"] = <scratch>` and `spec.env["CLAUDE_CONFIG_DIR"] = <scratch config dir>` (use the exact var name and layout the previous feature documented). Build this where the worker SessionSpec.env is assembled (crates/engine/src/runner.rs ~:702, where `spec.env = contract_env(base_sha)` today) — extend, do not replace, so KRANZ_BASE_SHA and any existing contract env still flow.

Scratch seeding: create a per-session scratch directory (unique per session id; under the system temp dir) and seed it with ONLY the entries enumerated by the minimal-set constant, copied from the real HOME/config WHEN they exist (so file-based auth survives; Keychain auth survives inherently and needs no copy). The scratch HOME must contain exactly the allowlisted entries and nothing else — no arbitrary operator dotfiles.

Scope: worker role only; leave orchestrator and validator sessions unchanged for now (note this scoping in the WorkerReport). Ensure the scratch dir does not break cwd (cwd stays the worktree) and does not interfere with the worktree machinery.

Testing — CRITICAL to avoid the final-gate env-export trap (a negative live-env assertion has cost a full respawn before): unit-test the SEEDING FUNCTION and the ENV CONSTRUCTION deterministically. Given a fixture source HOME containing both allowlisted and non-allowlisted files, assert the produced scratch dir contains exactly the allowlisted entries (non-allowlisted absent), and assert the constructed worker spec.env sets HOME + CLAUDE_CONFIG_DIR to the scratch paths AND still carries KRANZ_BASE_SHA. Do NOT write a test that inspects the live process environment of a real claude run.

Constraints: repo meta-lesson — gate cargo on `--workspace`/`-p kranz-engine`. Do not copy or log secret contents; copy files opaquely.

Name tests so the substring `worker_env_hygiene` appears.

Done when:
- Given a fixture source HOME with allowlisted + non-allowlisted files, the seeding produces a scratch dir containing exactly the allowlisted entries and none of the non-allowlisted ones — test named with `worker_env_hygiene`.
- The constructed worker SessionSpec.env sets HOME and CLAUDE_CONFIG_DIR to the scratch paths and still contains KRANZ_BASE_SHA (contract env preserved) — test named with `worker_env_hygiene`.
- Only worker-role sessions receive the scratch HOME/CLAUDE_CONFIG_DIR; orchestrator/validator env is unchanged.
- No test inspects the live process environment of a real claude run (seeding + env construction are unit-tested directly).
- `cargo test --workspace 2>&1 | grep -qE 'result: ok\.'` passes.

