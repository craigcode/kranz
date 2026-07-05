# Mission report — m-b9ce7a

**Goal:** Pin the validation-contract diff base at plan approval by recording the mission's base-branch commit SHA, exposing it to every worker and validator session as KRANZ_BASE_SHA, and teaching the prompt templates to reference it — so contracts never diff against the moving base branch.

Branch `kranz/mission-m-b9ce7a` (from `main`). Plan of record: [plan.md](plan.md).

**Elapsed:** 30m 24s
**Tokens:** 33091 in / 84804 out / 11282639 cache read / 489810 cache write
**Cost:** $39.81 actual vs $8.15–$40.75 estimated (expected $16.30)

## What shipped

### Milestone 1 — Base SHA is recorded at approval and durable across old logs ✅

- ✅ **Additive optional baseSha on plan.approved, folded into mission state** — 1 run
  - `78c2145` [f-1-1] add optional baseSha to plan.approved, fold into Mission
  - `b783578` [f-1-1] checkpoint (engine commit)
- ✅ **approve_plan resolves and records the base branch SHA** — 1 run
  - `8e9e08f` [f-1-2] approve_plan resolves and records the base branch SHA
  - `054eb5a` [f-1-2] checkpoint (engine commit)

### Milestone 2 — Base SHA reaches every session and the prompts reference it ✅

- ✅ **Propagate KRANZ_BASE_SHA into worker and validator session env** — 1 run
  - `0ee7bb1` [f-2-1] checkpoint (engine commit)
- ✅ **Teach the four prompts and document the deviation** — 1 run
  - `9a7e73c` [f-2-2] teach the four role prompts to reference $KRANZ_BASE_SHA
  - `8807b3a` [f-2-2] checkpoint (engine commit)

## Validation history

### ms-1 round 1 — Base SHA is recorded at approval and durable across old logs

- [critical] a4 — cargo test -p kranz-engine base_sha_reaches_worker_and_validator_env — every test binary reports '0 tests' / '0 passed; ... filtered out' with no matching test name found anywhere. Grep for `base_sha_… [truncated]
- [critical] a6 — cargo test -p kranz-engine all_prompts_mention_base_sha — every test binary reports '0 tests' with no matching test name found. Grep for `all_prompts_mention_base_sha` across the repo (excluding targe… [truncated]
- [critical] a8 — grep -q KRANZ_BASE_SHA docs/design.md exited 1 (no match). Repo-wide grep for KRANZ_BASE_SHA (excluding target/) only matches .kranz/missions/m-b9ce7a/plan.md and plan.json — the plan documents, not a… [truncated]

Disposition: waived.
- a4: Not an ms-1 defect — a4 (worker/validator SessionSpec KRANZ_BASE_SHA env) is the deliverable of pending feature f-2-1 in ms-2; will be implemented and validated there and at the final gate.
- a6: Not an ms-1 defect — a6 (all-prompts-mention-KRANZ_BASE_SHA test) is the deliverable of pending feature f-2-2 in ms-2; enforced at ms-2 validation and the final gate.
- a8: Not an ms-1 defect — a8 (docs/design.md pinned-base deviation note) is the deliverable of pending feature f-2-2 in ms-2; enforced at ms-2 validation and the final gate.

### ms-2 round 1 — Base SHA reaches every session and the prompts reference it

No findings.

## Contract outcomes

- ✅ **[a1]** The engine crate's full test suite passes. *(command: `cargo test -p kranz-engine`)*
- ✅ **[a2]** The engine crate is clippy-clean across all targets with warnings denied. *(command: `cargo clippy -p kranz-engine --all-targets -- -D warnings`)*
- ✅ **[a3]** A plan.approved event round-trips through serde with the baseSha field present AND absent, and an old-log JSON fixture that omits baseSha deserializes and folds cleanly to a None base SHA (backward compatibility with pre-existing event logs). *(command: `cargo test -p kranz-engine plan_approved_base_sha_backcompat`)*
- ✅ **[a4]** After a plan is approved with a recorded base SHA, both the spawned worker SessionSpec and the spawned validator SessionSpec carry an environment variable KRANZ_BASE_SHA equal to the recorded SHA, asserted via the MockBackend started_specs seam without spawning any real session. *(command: `cargo test -p kranz-engine base_sha_reaches_worker_and_validator_env`)*
- ✅ **[a5]** Approving a plan records, in the plan.approved event, the base branch's commit SHA as it stood at the moment of approval (equal to git rev-parse of the base branch). *(command: `cargo test -p kranz-engine approval_records_base_branch_sha`)*
- ✅ **[a6]** All four role prompt templates (orchestrator, worker, validator-scrutiny, validator-functional) contain the literal token KRANZ_BASE_SHA. *(command: `cargo test -p kranz-engine all_prompts_mention_base_sha`)*
- ✅ **[a7]** The orchestrator prompt's contract-authoring guidance explicitly forbids bare branch names (e.g. main) in diff-based assertions and directs the author to use $KRANZ_BASE_SHA; the worker and both validator prompts describe $KRANZ_BASE_SHA as the immutable base for any diff-based claim. *(agent judgement)*
- ✅ **[a8]** docs/design.md gains a short deviation note describing the pinned-base contract (base SHA recorded at approval, exposed as KRANZ_BASE_SHA). *(command: `grep -q KRANZ_BASE_SHA docs/design.md`)*

All assertions passed at the final contract gate (waivers, if any, appear in the validation history).
