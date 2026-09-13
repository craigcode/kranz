# Mission report — m-836459

**Goal:** Serve the pipeline view's data layer: persist the calibrated cost estimate into plan.md at park time, expose GET endpoints for a mission's plan.md/report.md/diff-stat, and report a per-mission merged/unmerged bit — pure engine/REST, no UI.

Branch `kranz/mission-m-836459` (from `main`). Plan of record: [plan.md](plan.md).

**Elapsed:** 59m 37s
**Tokens:** 38785 in / 122449 out / 13914723 cache read / 755950 cache write
**Cost:** $39.05 actual vs $8.15–$40.75 estimated (expected $16.30)

## What shipped

### Milestone 1 — Estimate persisted into plan.md at park time ✅

- ✅ **Compute and render the calibrated cost estimate into plan.md's header** — 1 run
  - `acbd1d7` [f-1-1] persist calibrated cost estimate into plan.md at park time

### Milestone 2 — Serve stage artifacts and the merged/unmerged bit over REST ✅

- ✅ **GET endpoints for a mission's plan.md and report.md** — 1 run
  - `1f3b560` [f-2-1] add GET endpoints for mission plan.md and report.md
- ✅ **GET diff-stat endpoint (pinned base_sha … mission tip)** — 1 run
  - `f3c2347` [f-2-2] checkpoint (engine commit)
- ✅ **git ancestry probe + merged bit on the missions list** — 1 run
  - `64417ac` [f-2-3] add git ancestry probe + merged bit on missions list
- ✅ **Cover the git-failure error branches for diff-stat and is_ancestor with tests** *(fix)* — 1 run
  - `d243956` [ms-2-fix-1-1] cover git-failure error branches for is_ancestor and diff-stat

## Validation history

### ms-1 round 1 — Estimate persisted into plan.md at park time

No findings.

### ms-2 round 1 — Serve stage artifacts and the merged/unmerged bit over REST

- [minor] f-2-2: A genuine git failure surfaces as a 5xx ApiError::internal, not a panic — crates/server/src/rest.rs:196-203 correctly propagates EngineError::Git via `?` to ApiError::internal (error.rs:54-72 maps the `_` arm to a 500), so the behaviour is implemented. But no test in crates… [truncated]
- [minor] f-2-3 / a5: is_ancestor errors only on genuine git failure — crates/engine/src/git_ops.rs:117-123 maps any exit code other than 0/1 to EngineError::Git, which is correct, but git_ops_test.rs only tests exit 0 (true/self), exit 1 (false) and the pre-invocation f… [truncated]

Disposition: 1 fix feature(s) created.

### ms-2 round 2 — Serve stage artifacts and the merged/unmerged bit over REST

- [minor] f-2-3 / [a4] per-mission git failure degrades merged to null without failing the list — crates/server/src/rest.rs merged_bit() degrades every ref failure to None via `.ok()?`, but the only resilience test (crates/server/tests/server_test.rs missions_list_still_returns_when_repo_root_is_n… [truncated]

Disposition: waived.
- f-2-3 / [a4] per-mission git failure degrades merged to null without failing the list: Behaviour is correct and uniformly guarded (.ok()? on every ref call in merged_bit); the a4 'git failure never fails the list' criterion is already covered by the non-git-repo resilience test. The narrower per-mission mid-probe failure needs a fragile, git-version-sensitive corrupt-ref fixture — not worth a fresh session on a second fix cycle for a low-probability regression.

## Contract outcomes

- ✅ **[a1]** At plan-park (approval) time, the committed plan.md carries a stable, labeled cost-estimate block showing the low/expected/high USD range AND its calibration provenance (number of completed missions used, or a built-in-defaults note), derived from the calibrated estimate — not the built-in defaults when calibration data exists. *(command: `cargo test -p kranz-engine --test mission_test`)*
- ✅ **[a2]** GET /api/missions/{id}/plan.md and GET /api/missions/{id}/report.md return the rendered markdown of those files (JSON-wrapped as {"markdown": ...}); plan.md 404s until a plan is approved and report.md 404s until the mission is complete; path-traversal ids are rejected. *(command: `cargo test -p kranz-server --test server_test`)*
- ✅ **[a3]** GET /api/missions/{id}/diff-stat returns the git diff --stat between the mission's pinned base_sha and its mission-branch tip (JSON-wrapped with the diff stat plus the baseSha and tip it compared), and 404s when the mission has no mission branch or no pinned base_sha. *(command: `cargo test -p kranz-server --test server_test`)*
- ✅ **[a4]** GET /api/missions rows carry a merged bit computed by probing whether the mission-branch tip is an ancestor of the LIVE base-branch tip (true once the base has absorbed the branch, false while unmerged); the bit is null/absent for missions with no mission branch, and a git failure never fails the whole list. *(command: `cargo test -p kranz-server --test server_test`)*
- ✅ **[a5]** The engine exposes a git ancestry probe (git merge-base --is-ancestor) that returns true when the first ref is an ancestor of (or equal to) the second, false when it is not, and errors only on genuine git failure. *(command: `cargo test -p kranz-engine --test git_ops_test`)*
- ✅ **[a6]** The entire workspace builds and every crate's tests pass. *(command: `cargo test --workspace`)*
- ✅ **[a7]** Code is formatted and lint-clean. *(command: `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings`)*
- ✅ **[a8]** No UI/frontend files are touched by this mission — nothing under apps/ changes relative to the pinned mission base. *(command: `[ -z "$(git diff --name-only $KRANZ_BASE_SHA -- apps)" ]`)*

All assertions passed at the final contract gate (waivers, if any, appear in the validation history).
