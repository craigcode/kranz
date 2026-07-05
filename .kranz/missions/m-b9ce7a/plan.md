# Mission plan — m-b9ce7a

**Goal:** Pin the validation-contract diff base at plan approval by recording the mission's base-branch commit SHA, exposing it to every worker and validator session as KRANZ_BASE_SHA, and teaching the prompt templates to reference it — so contracts never diff against the moving base branch.

Branch `kranz/mission-m-b9ce7a` (from `main`). Approved plan of record; the machine-readable twin is [plan.json](plan.json). Live status: `kranz status` or the dashboard.

## Validation contract

Defined before any feature; gates mission completion.

- **[a1]** The engine crate's full test suite passes. 
  `cargo test -p kranz-engine`
- **[a2]** The engine crate is clippy-clean across all targets with warnings denied. 
  `cargo clippy -p kranz-engine --all-targets -- -D warnings`
- **[a3]** A plan.approved event round-trips through serde with the baseSha field present AND absent, and an old-log JSON fixture that omits baseSha deserializes and folds cleanly to a None base SHA (backward compatibility with pre-existing event logs). 
  `cargo test -p kranz-engine plan_approved_base_sha_backcompat`
- **[a4]** After a plan is approved with a recorded base SHA, both the spawned worker SessionSpec and the spawned validator SessionSpec carry an environment variable KRANZ_BASE_SHA equal to the recorded SHA, asserted via the MockBackend started_specs seam without spawning any real session. 
  `cargo test -p kranz-engine base_sha_reaches_worker_and_validator_env`
- **[a5]** Approving a plan records, in the plan.approved event, the base branch's commit SHA as it stood at the moment of approval (equal to git rev-parse of the base branch). 
  `cargo test -p kranz-engine approval_records_base_branch_sha`
- **[a6]** All four role prompt templates (orchestrator, worker, validator-scrutiny, validator-functional) contain the literal token KRANZ_BASE_SHA. 
  `cargo test -p kranz-engine all_prompts_mention_base_sha`
- **[a7]** The orchestrator prompt's contract-authoring guidance explicitly forbids bare branch names (e.g. main) in diff-based assertions and directs the author to use $KRANZ_BASE_SHA; the worker and both validator prompts describe $KRANZ_BASE_SHA as the immutable base for any diff-based claim. *(agent judgement)*
- **[a8]** docs/design.md gains a short deviation note describing the pinned-base contract (base SHA recorded at approval, exposed as KRANZ_BASE_SHA). 
  `grep -q KRANZ_BASE_SHA docs/design.md`

## Milestone 1 — Base SHA is recorded at approval and durable across old logs

### 1.1 Additive optional baseSha on plan.approved, folded into mission state

Add the base commit SHA to the durable event schema as a STRICTLY ADDITIVE, OPTIONAL field, and fold it into mission state.

SANCTIONED CONTRACT-FILE EDIT: crates/engine/src/events.rs carries a header saying it must not be modified in implementation phases. This mission explicitly authorizes ONE additive change to it: adding an optional field to the PlanApproved variant. Keep the change additive and optional only — do not alter or reorder any existing field, and do not touch any other variant. Do not edit crates/engine/src/backend.rs (SessionSpec already has an `env` map; nothing there changes).

Changes:
1. In crates/engine/src/events.rs, `EventKind::PlanApproved` currently is `PlanApproved { plan: Plan }`. Add a second field: `base_sha: Option<String>` with serde attributes `#[serde(rename = "baseSha", default, skip_serializing_if = "Option::is_none")]`. The variant already serializes under `"type":"plan.approved"` with a `payload` object (see the enum's `#[serde(tag="type", content="payload")]`), so baseSha lands inside payload alongside plan. `default` makes a missing baseSha deserialize to None; `skip_serializing_if` keeps events that have no base SHA byte-identical to today on the wire.
2. In crates/engine/src/types.rs, add `base_sha: Option<String>` to the `Mission` struct (right after `base_branch`), with `#[serde(default, skip_serializing_if = "Option::is_none")]` so existing state.json caches stay byte-identical when it is None. In crates/engine/src/reducer.rs, `initial_state` builds Mission from the MissionCreated event — set `base_sha: None` there.
3. In crates/engine/src/reducer.rs, the `EventKind::PlanApproved { plan }` match arm (~line 50) must now bind the new field, e.g. `PlanApproved { plan, base_sha }`, and set `state.mission.base_sha = base_sha.clone();` (in addition to everything it already does). Update the `type_name()` match and any other exhaustive match on PlanApproved in the crate so the new field compiles (use `..` or bind it).
4. Grep the crate for every construction and match of `PlanApproved` (events.rs type_name, reducer, orchestrator.rs approve_plan emit site, and any tests/fixtures) and update them to the new shape. In this feature, construct approve_plan's emit with `base_sha: None` for now — the real SHA resolution is a separate feature; do not implement git rev-parse here.

Backward compatibility is the hard requirement (see docs/reviews/event-log-review.md for how seriously this repo treats the log). Prove it with tests.

Done when:
- A unit test named plan_approved_base_sha_backcompat serializes an EventKind::PlanApproved with base_sha = Some("deadbeef") and asserts it round-trips (deserialize back to Some("deadbeef")).
- The same or a sibling test serializes a PlanApproved with base_sha = None and asserts the serialized payload JSON does NOT contain the key "baseSha" and round-trips back to None.
- plan_approved_base_sha_backcompat deserializes a hand-written old-log Event JSON string for a plan.approved event whose payload has NO baseSha key, and asserts it deserializes successfully and (after fold or direct inspection) yields base_sha == None.
- cargo test -p kranz-engine passes and cargo clippy -p kranz-engine --all-targets -- -D warnings is clean.

### 1.2 approve_plan resolves and records the base branch SHA

Make plan approval capture the base branch's current commit SHA and record it in the plan.approved event.

This builds on the additive baseSha field already added to EventKind::PlanApproved and Mission.base_sha (assume both exist; if the emit site still passes base_sha: None, change it to the resolved SHA).

Changes:
1. In crates/engine/src/git_ops.rs, add a small helper on GitRepo to rev-parse an arbitrary ref, mirroring the existing `head_sha` (which runs `rev-parse HEAD`). Suggested: `pub fn rev_parse(&self, refname: &str) -> Result<String>` returning the trimmed stdout of `git rev-parse <refname>`. Follow the file's established guard style: reject a flag-shaped argument (one starting with '-') with an EngineError::Git before invoking git, as add_worktree/merge_no_ff/push_mission_branch do. Use the existing `run`/`run_os` plumbing; do not add new dependencies.
2. In crates/engine/src/orchestrator.rs `approve_plan` (~line 607), resolve the base branch SHA using `self.repo.rev_parse(&self.state.mission.base_branch)` (the base branch name is already cloned there as `base`). Resolve it and pass it as `base_sha: Some(sha)` when emitting `EventKind::PlanApproved`. Committing plan files onto the mission branch does not move the base branch ref, so resolving it anywhere within approve_plan yields the base tip as of approval — resolve it and thread it into the emitted event. Do NOT resolve or backfill a SHA anywhere except at approval (a late-resolved SHA would reintroduce the very race this fixes).
3. Ensure the reducer already sets state.mission.base_sha from the event (from the prior feature); no reducer change should be needed here beyond confirming it.

Keep the change surgical: approve_plan already does git branch/checkout/commit via self.repo; add exactly the rev-parse and thread its result into the emit.

Done when:
- A GitRepo unit test asserts rev_parse(<current branch>) equals head_sha() in a temp repo with at least one commit, and that rev_parse("-somethingflagshaped") returns an EngineError::Git without invoking git.
- An integration test named approval_records_base_branch_sha sets up a mission on a base branch, records the base branch tip via git rev-parse (or GitRepo::rev_parse) BEFORE approval, approves a minimal valid plan, and asserts the folded state.mission.base_sha (and/or the emitted plan.approved event's baseSha) equals that recorded SHA.
- cargo test -p kranz-engine passes and cargo clippy -p kranz-engine --all-targets -- -D warnings is clean.


## Milestone 2 — Base SHA reaches every session and the prompts reference it

### 2.1 Propagate KRANZ_BASE_SHA into worker and validator session env

Expose the recorded base SHA to every worker and validator session as the environment variable KRANZ_BASE_SHA, sourced from mission state, only when a base SHA is present.

Context: SessionSpec (crates/engine/src/backend.rs) already has `pub env: HashMap<String, String>`. Worker specs are built in crates/engine/src/runner.rs `build_worker_spec` (~line 581, `env: HashMap::new()`), shared by run_worker/run_worker_in/run_worker_in_buffered. Validator specs are built in crates/engine/src/runner.rs `run_validator` (~line 661, `env: HashMap::new()`). This feature assumes mission state already carries the base SHA as `state.mission.base_sha: Option<String>` (added by an earlier milestone).

Changes:
1. Thread the base SHA (as `Option<&str>`) from the orchestrator's mission state into the spec builders. `MissionEngine` holds `self.state`; read `self.state.mission.base_sha.as_deref()` at the worker/validator spawn sites (orchestrator.rs ~lines 1209 run_worker, ~1639 run_worker_in_buffered, ~1927 run_validator) and pass it down.
2. Add a `base_sha: Option<&str>` parameter to `run_worker`, `run_worker_in`, `run_worker_in_buffered`, and `build_worker_spec`, and to `run_validator`. These functions already carry `#[allow(clippy::too_many_arguments)]` where needed; add it if a new one trips clippy. Keep the thin wrappers (run_worker -> run_worker_in) forwarding the new arg.
3. In both `build_worker_spec` and `run_validator`, after constructing the spec (or when initializing `env`), insert `KRANZ_BASE_SHA` into `spec.env` only when the base SHA is `Some` and non-empty. When it is None, insert nothing (old missions omit the var entirely — no fallback, no empty value).
4. Update every existing caller and test of these functions to pass the new argument (pass None where there is no base SHA in scope). Do not change SessionSpec itself.

Do not alter permissions::apply behavior; env is independent of the allow/deny tool wiring.

Done when:
- An integration test named base_sha_reaches_worker_and_validator_env uses a MockBackend, drives a mission through plan approval with a known recorded base SHA, and lets it spawn at least one worker and at least one validator; via backend.started_specs() it asserts the worker spec's env contains KRANZ_BASE_SHA equal to the recorded SHA AND a validator spec's env contains KRANZ_BASE_SHA equal to the recorded SHA — with no real session spawned.
- A test asserts that when mission state has base_sha == None, a built worker (and validator) spec's env does NOT contain the key KRANZ_BASE_SHA at all.
- cargo test -p kranz-engine passes and cargo clippy -p kranz-engine --all-targets -- -D warnings is clean.

### 2.2 Teach the four prompts and document the deviation

Update all four role prompt templates to reference $KRANZ_BASE_SHA for diff-based claims, and record the deviation in the design doc.

Files: crates/engine/prompts/orchestrator.md, worker.md, validator-scrutiny.md, validator-functional.md, and docs/design.md.

Changes:
1. orchestrator.md: in the contract-authoring section (step 2, 'Define the VALIDATION CONTRACT'), add explicit guidance that when a validationContract command compares against the pre-mission state it MUST use `$KRANZ_BASE_SHA` (e.g. `git diff --name-only $KRANZ_BASE_SHA`) and MUST NOT use a bare branch name like `main`, because the base branch ref moves as other work lands and would make the assertion race concurrent commits. Mention that KRANZ_BASE_SHA is the base commit pinned at plan approval.
2. worker.md: add a short line (near the diff/commit protocol) stating that `$KRANZ_BASE_SHA` is the immutable pre-mission base commit; use it for any diff-based claim rather than a branch name.
3. validator-scrutiny.md and validator-functional.md: both already reference the `{startSha}..HEAD` milestone range; add that `$KRANZ_BASE_SHA` is the immutable base for any diff comparison against the pre-mission state, and to prefer it over a branch name like `main`.
4. docs/design.md: under '## Deviations from the plan document' (~line 98), add a short note describing the pinned-base contract: the base branch SHA is captured at plan approval, recorded on the plan.approved event (optional/additive for backward compatibility), exposed to worker and validator sessions as the KRANZ_BASE_SHA env var, and referenced by the prompts so contracts never diff against the moving base branch. Reference the m-660ffc incident that motivated it if concise.
5. Add a unit test (in the engine crate, where prompts::text(role) is accessible) named all_prompts_mention_base_sha asserting that each of the four role prompt strings contains the literal substring "KRANZ_BASE_SHA".

Do not change prompt-rendering code or variable interpolation; these are literal text additions. Prompt-hash values may change — that is expected and fine.

Done when:
- A unit test named all_prompts_mention_base_sha asserts each of the four role prompts (orchestrator, worker, validator-scrutiny, validator-functional) contains the literal substring KRANZ_BASE_SHA.
- The orchestrator prompt text explicitly forbids using a bare branch name (e.g. main) in diff-based contract assertions and directs the author to $KRANZ_BASE_SHA instead.
- grep -q KRANZ_BASE_SHA docs/design.md succeeds, and the note describes the base SHA being pinned at approval and exposed to sessions.
- cargo test -p kranz-engine passes and cargo clippy -p kranz-engine --all-targets -- -D warnings is clean.

