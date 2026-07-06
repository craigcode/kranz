# Mission plan — m-836459

**Goal:** Serve the pipeline view's data layer: persist the calibrated cost estimate into plan.md at park time, expose GET endpoints for a mission's plan.md/report.md/diff-stat, and report a per-mission merged/unmerged bit — pure engine/REST, no UI.

Branch `kranz/mission-m-836459` (from `main`). Approved plan of record; the machine-readable twin is [plan.json](plan.json). Live status: `kranz status` or the dashboard.

## Validation contract

Defined before any feature; gates mission completion.

- **[a1]** At plan-park (approval) time, the committed plan.md carries a stable, labeled cost-estimate block showing the low/expected/high USD range AND its calibration provenance (number of completed missions used, or a built-in-defaults note), derived from the calibrated estimate — not the built-in defaults when calibration data exists. 
  `cargo test -p kranz-engine --test mission_test`
- **[a2]** GET /api/missions/{id}/plan.md and GET /api/missions/{id}/report.md return the rendered markdown of those files (JSON-wrapped as {"markdown": ...}); plan.md 404s until a plan is approved and report.md 404s until the mission is complete; path-traversal ids are rejected. 
  `cargo test -p kranz-server --test server_test`
- **[a3]** GET /api/missions/{id}/diff-stat returns the git diff --stat between the mission's pinned base_sha and its mission-branch tip (JSON-wrapped with the diff stat plus the baseSha and tip it compared), and 404s when the mission has no mission branch or no pinned base_sha. 
  `cargo test -p kranz-server --test server_test`
- **[a4]** GET /api/missions rows carry a merged bit computed by probing whether the mission-branch tip is an ancestor of the LIVE base-branch tip (true once the base has absorbed the branch, false while unmerged); the bit is null/absent for missions with no mission branch, and a git failure never fails the whole list. 
  `cargo test -p kranz-server --test server_test`
- **[a5]** The engine exposes a git ancestry probe (git merge-base --is-ancestor) that returns true when the first ref is an ancestor of (or equal to) the second, false when it is not, and errors only on genuine git failure. 
  `cargo test -p kranz-engine --test git_ops_test`
- **[a6]** The entire workspace builds and every crate's tests pass. 
  `cargo test --workspace`
- **[a7]** Code is formatted and lint-clean. 
  `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings`
- **[a8]** No UI/frontend files are touched by this mission — nothing under apps/ changes relative to the pinned mission base. 
  `[ -z "$(git diff --name-only $KRANZ_BASE_SHA -- apps)" ]`

## Milestone 1 — Estimate persisted into plan.md at park time

### 1.1 Compute and render the calibrated cost estimate into plan.md's header

GOAL: Today the pre-mission cost estimate is computed only in the CLI review path and printed to stdout, then lost — the human queue gate at the Reviewable stage is uninformed. Make `approve_plan` persist the estimate into the committed plan.md so any surface reading plan.md sees it.

CONTEXT / FILES:
- `crates/engine/src/orchestrator.rs`: `approve_plan(&mut self, mut plan: Plan)` (~line 630) writes `plan.json` and `plan.md` (the latter via `render_plan_markdown(&plan, &self.state.mission)`, ~line 3286) and commits both. `self.paths.repo_root` is the repo root. `self.state.config` is the `MissionConfig`.
- `crates/engine/src/cost.rs`: `pub fn calibrate(repo_root: &Path) -> Calibration` (returns `Calibration { params, missions_used }`, never fails — falls back to defaults with `missions_used == 0`) and `pub fn estimate(plan: &Plan, cfg: &MissionConfig, p: &EstimateParams) -> CostEstimate` (fields `low_usd`, `expected_usd`, `high_usd`). This is exactly what the CLI does at review time (see `crates/cli/src/commands.rs` ~line 595 and `crates/cli/src/planning_tui.rs` ~line 1200).
- The CLI's user-facing wording lives in `crates/cli/src/output.rs::render_cost_estimate` — the engine CANNOT depend on the cli crate, so render an equivalent string inside the engine.

WHAT TO DO:
1. In `approve_plan`, before rendering plan.md, compute `let calibration = cost::calibrate(&self.paths.repo_root); let estimate = cost::estimate(&plan, &self.state.config, &calibration.params);`. `calibrate` never errors.
2. Change `render_plan_markdown` to also accept the estimate and `missions_used` (e.g. `render_plan_markdown(plan: &Plan, mission: &Mission, estimate: &cost::CostEstimate, missions_used: usize)`), and have it emit a STABLE, clearly-labeled block near the top of the document (a dedicated `## Cost estimate` section is preferred over a bare line so it is greppable and the future pipeline view can locate it). The block MUST contain: the low, expected, and high USD figures formatted to 2 decimals (e.g. `$3.50`), and a calibration-provenance sentence — `based on N completed mission(s)` when `missions_used > 0`, or `built-in defaults — no completed missions yet` when it is 0. Mirror the phrasing of `crates/cli/src/output.rs::render_cost_estimate` for consistency (rough estimate; live usage is authoritative), but implement it in the engine.
3. Update the single call site in `approve_plan` to pass the estimate + `missions_used`. Do NOT change plan.json (the machine-readable `Plan` struct stays as-is). Do NOT alter `render_mission_report` (it renders its own estimate section for report.md and is out of scope).
4. Keep `render_plan_markdown` pure/deterministic given its inputs (no wall-clock reads inside it — the estimate is passed in).

TESTS (encode FIRST, in `crates/engine/tests/mission_test.rs`, following the existing approve-plan test patterns there that read the committed plan.md): after approving a plan, assert the committed plan.md contains the low/expected/high dollar figures and the calibration-provenance sentence. Add a case that exercises the `missions_used == 0` provenance wording (a fresh repo with no completed missions). Prefer asserting on the exact provenance strings so the block is pinned. Run: `cargo test -p kranz-engine --test mission_test`. Also ensure `cargo fmt --all --check` and `cargo clippy -p kranz-engine --all-targets -- -D warnings` are clean before reporting.

Done when:
- After approve_plan, the committed plan.md contains a labeled cost-estimate section with the low, expected, and high USD figures (2-decimal formatted).
- The estimate section states its calibration provenance: 'based on N completed mission(s)' when calibration data exists, or the built-in-defaults wording when missions_used == 0.
- The estimate is derived from cost::calibrate + cost::estimate (calibrated params), not the built-in EstimateParams::default when completed missions exist.
- plan.json is unchanged in shape; render_mission_report is untouched.
- cargo test -p kranz-engine --test mission_test passes; fmt and clippy are clean.


## Milestone 2 — Serve stage artifacts and the merged/unmerged bit over REST

### 2.1 GET endpoints for a mission's plan.md and report.md

GOAL: Add read-only REST endpoints that return the rendered markdown of a mission's plan.md and report.md so a UI can show them inline. Pure REST — no git, no mutation.

CONTEXT / FILES:
- `crates/server/src/rest.rs`: existing read handlers (`mission_plan` ~line 114 returns plan.json as JSON; `run_transcript`) show the exact patterns to reuse: `mission_paths(&server, &id)?` (rejects path traversal via `safe_id`), `read_file_or_404(path, || msg)` (returns `ApiError::not_found` on ENOENT, `internal` on other IO errors), and `unknown_mission(&id)`.
- `crates/engine/src/paths.rs`: `MissionPaths` has `plan_file()` (=> `<mission_dir>/plan.json`) and `mission_dir()`. There is NO helper for plan.md or report.md — build them as `paths.mission_dir().join("plan.md")` and `paths.mission_dir().join("report.md")` (or add small `plan_md_file()` / `report_file()` helpers to paths.rs; either is fine, but if you add helpers, add them consistently and unit-test them like the existing `lessons_paths` test).
- `crates/server/src/lib.rs`: routes are registered in `router_with_shared_host` (~line 125). Add the two new GET routes next to the existing `/api/missions/{id}/plan` route. Axum 0.8 path syntax uses `{id}`.
- `docs/protocol.md` is authoritative for routes — add the two new endpoints to its REST table.

WHAT TO DO:
1. Add `pub(crate) async fn mission_plan_md` and `pub(crate) async fn mission_report_md` handlers in `rest.rs`. Each resolves `mission_paths`, reads the file with `read_file_or_404`, and returns `Json(json!({ "markdown": <contents> }))`. plan.md 404s with a message like `mission '{id}' has no approved plan yet` (mirroring `mission_plan`); report.md 404s with `mission '{id}' has no report yet` (report.md only exists after mission completion).
2. Register routes `GET /api/missions/{id}/plan.md` and `GET /api/missions/{id}/report.md` in lib.rs. (A literal `.md` segment after the `{id}` capture is valid in axum; if you hit any routing ambiguity, fall back to `/plan-md` and `/report-md` and note it in docs/protocol.md — but prefer the `.md` spelling.)
3. Update the REST table in `docs/protocol.md`.

TESTS (encode FIRST, in `crates/server/tests/server_test.rs`, using the existing oneshot-router harness in that file — it drives `router(...)` with `tower::ServiceExt::oneshot` and seeds mission files on disk; see how existing tests stage `.kranz/missions/<id>/` fixtures and the plan.md fixture around line 304): assert plan.md endpoint returns the file's markdown for an approved mission and 404s for a mission with no plan; assert report.md endpoint returns markdown when report.md exists and 404s when it does not; assert a traversal id (e.g. `../foo`) is rejected. Run: `cargo test -p kranz-server --test server_test`. Ensure fmt + clippy clean.

CONSTRAINT: touch only crates/server, crates/engine/src/paths.rs (optional helpers), and docs/protocol.md. Do NOT modify anything under apps/.

Done when:
- GET /api/missions/{id}/plan.md returns {"markdown": <plan.md contents>} for an approved mission and 404s (not_found) when plan.md is absent.
- GET /api/missions/{id}/report.md returns {"markdown": <report.md contents>} when report.md exists and 404s when it is absent.
- Path-traversal / unsafe ids are rejected via the existing safe_id guard.
- The two routes are registered in lib.rs and documented in docs/protocol.md's REST table.
- cargo test -p kranz-server --test server_test passes; fmt and clippy clean; nothing under apps/ changed.

### 2.2 GET diff-stat endpoint (pinned base_sha … mission tip)

GOAL: Add a read-only endpoint returning the `git diff --stat` of a mission's work — the pinned base commit vs the mission branch tip — so a UI can show the deliverable's diff summary at the Delivered stage.

CONTEXT / FILES:
- `crates/engine/src/git_ops.rs`: `GitRepo::open(root)`, `rev_parse(refname)`, `branch_exists(name)`, and `diff_stat(from, to)` (=> `git diff --stat <from>..<to>`, returns the stat text verbatim) already exist. Use them; do NOT add new git methods in this feature.
- The mission's pinned base commit is `Mission.base_sha: Option<String>` (`crates/engine/src/types.rs` ~line 51) — pinned at approval, immutable. The mission branch is `Mission.mission_branch` (`kranz/mission-<id>`). Fold the mission state to read these: `crates/server/src/rest.rs` has `fold_log(paths)` -> `MissionState` and `mission_state` shows the pattern. `state.mission.base_sha`, `state.mission.mission_branch`.
- The server does not currently open a GitRepo. Open one at `server.repo_root` inside the handler: `kranz_engine::git_ops::GitRepo::open(&server.repo_root)`.
- Routes registered in `crates/server/src/lib.rs::router_with_shared_host`; docs/protocol.md is authoritative.

WHAT TO DO:
1. Add `pub(crate) async fn mission_diff_stat` in `rest.rs`. Resolve `mission_paths`; 404 if `events_file()` is absent (unknown mission). Fold the log to a `MissionState`.
2. If `state.mission.base_sha` is `None`, return 404 with a clear message (e.g. `mission '{id}' has no pinned base yet` — a mission is only diffable once its plan is approved, which is when base_sha is set).
3. Resolve the mission branch tip: check `repo.branch_exists(&state.mission.mission_branch)?` first and return 404 with `mission '{id}' has no mission branch yet` when it does not exist (distinguish 'branch missing' from a real git failure); otherwise `repo.rev_parse(&state.mission.mission_branch)?`.
4. Compute `repo.diff_stat(&base_sha, &tip)` and return `Json(json!({ "diffStat": <stat>, "baseSha": <base_sha>, "tip": <tip> }))`. Note: this uses the PINNED base_sha (a stable snapshot of the mission's work), NOT the live base branch.
5. Register `GET /api/missions/{id}/diff-stat` in lib.rs and document it in docs/protocol.md.

ERROR MODEL: reuse `ApiError` (`crates/server/src/error.rs`) — `not_found`, `internal`, `bad_request`. A genuine git spawn failure maps to `internal` (an `EngineError` converting into `ApiError`; follow how existing handlers propagate engine errors with `?`).

TESTS (encode FIRST, in `crates/server/tests/server_test.rs`): build a real temp git repo fixture (several engine/server tests init git and commit — follow the nearest existing pattern that makes commits, e.g. approve-plan integration tests). Assert: the endpoint returns a diffStat plus baseSha and tip for a mission whose branch has commits beyond base_sha; 404 when the mission has no base_sha; 404 when the mission branch is absent. Run: `cargo test -p kranz-server --test server_test`. fmt + clippy clean.

CONSTRAINT: touch only crates/server and docs/protocol.md. Do NOT modify apps/.

Done when:
- GET /api/missions/{id}/diff-stat returns {"diffStat": ..., "baseSha": ..., "tip": ...} where diffStat is git diff --stat between the pinned base_sha and the mission-branch tip.
- It 404s when the mission has no pinned base_sha (unapproved) and 404s when the mission branch does not exist — neither is a 500.
- A genuine git failure surfaces as a 5xx ApiError::internal, not a panic.
- The route is registered in lib.rs and documented in docs/protocol.md.
- cargo test -p kranz-server --test server_test passes; fmt and clippy clean; nothing under apps/ changed.

### 2.3 git ancestry probe + merged bit on the missions list

GOAL: Report per mission whether it is merged (Landed) vs unmerged (Delivered), via a cheap server-side probe: is the mission-branch tip an ancestor of the LIVE base branch tip. Surface it on the missions list — the one-row-per-work-item surface the pipeline view consumes.

PART A — engine primitive (`crates/engine/src/git_ops.rs`):
- Add `pub fn is_ancestor(&self, ancestor: &str, descendant: &str) -> Result<bool>` implemented with `git merge-base --is-ancestor <ancestor> <descendant>` using the existing `probe`/`probe_os` plumbing (do NOT use `run`, which demands exit 0). git's contract: exit 0 => ancestor is an ancestor of (or equal to) descendant => `Ok(true)`; exit 1 => not an ancestor => `Ok(false)`; any OTHER exit code => `Err(EngineError::Git(...))` with `failure_detail`. Guard both refs against a leading `-` (flag-shaped), mirroring `rev_parse`/`merge_no_ff`.
- TESTS (in `crates/engine/tests/git_ops_test.rs`, following the existing temp-repo git fixtures there): a commit is an ancestor of a later commit on the same branch (true); the later commit is NOT an ancestor of the earlier (false); a commit is an ancestor of itself (true); flag-shaped ref is rejected. Run: `cargo test -p kranz-engine --test git_ops_test`.

PART B — merged bit on the list (`crates/server/src/rest.rs::list_missions`, ~line 31):
- For each mission row that folds successfully, compute a `merged` value: open a `GitRepo` once for the whole list (`kranz_engine::git_ops::GitRepo::open(&server.repo_root)`), and for each mission with a mission branch that exists (`repo.branch_exists(&state.mission.mission_branch)`), resolve the mission tip (`rev_parse(mission_branch)`) and the LIVE base branch tip (`rev_parse(base_branch)`) and call `repo.is_ancestor(mission_tip, base_tip)`. IMPORTANT: this probe MUST use the live base branch ref (which moves as merges land), NOT the pinned base_sha — merged-detection is exactly 'has the base absorbed this branch.'
- Add `"merged": <bool>` to the row's JSON when computable; set it to `null` (or omit) when the mission has no mission branch, or when git can't resolve a ref. A git failure for one mission must NOT fail the whole list — degrade that mission's `merged` to null and keep going (same resilience contract as the existing corrupt-log handling that yields a row with an error field instead of failing the list). If opening the GitRepo itself fails, every row simply gets merged=null.
- TESTS (in `crates/server/tests/server_test.rs`): a real temp git repo with a mission branch merged into base => row has `merged: true`; a mission branch with commits NOT merged into base => `merged: false`; a mission with no branch => `merged` null/absent; the list still returns for a mission whose git refs can't be resolved.

Run both: `cargo test -p kranz-engine --test git_ops_test` and `cargo test -p kranz-server --test server_test`. Update docs/protocol.md's description of the /api/missions row shape to mention the merged field. fmt + clippy clean.

CONSTRAINT: touch only crates/engine/src/git_ops.rs, crates/server, and docs/protocol.md. Do NOT modify apps/.

Done when:
- git_ops::is_ancestor returns Ok(true) when the first ref is an ancestor of (or equal to) the second, Ok(false) (from exit code 1) when not, and Err only on a real git failure; flag-shaped refs are rejected.
- GET /api/missions rows include a merged bit: true when the mission tip is an ancestor of the LIVE base branch tip, false when unmerged.
- The merged bit is null/absent for missions with no mission branch, and a per-mission git failure degrades that row's merged to null without failing the whole list.
- The merged probe uses the live base branch ref, not the pinned base_sha.
- cargo test -p kranz-engine --test git_ops_test and cargo test -p kranz-server --test server_test pass; docs/protocol.md updated; fmt and clippy clean; nothing under apps/ changed.

