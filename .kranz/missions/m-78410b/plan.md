# Mission plan — m-78410b

**Goal:** Make plan-level command grants and worker-executed verification commands first-class allowlist entries so worker and validator sessions share one source of truth and validators can re-run what workers ran.

Branch `kranz/mission-m-78410b` (from `main`). Approved plan of record; the machine-readable twin is [plan.json](plan.json). Live status: `kranz status` or the dashboard.

## Cost estimate

Estimated **$5.93 – $29.65** (expected ~$11.86). Rough estimate — live usage is authoritative; based on 19 completed mission(s).

## Validation contract

Defined before any feature; gates mission completion.

- **[a1]** A plan whose commandGrants lists a read-only command produces both a worker SessionSpec and a validator SessionSpec whose allowed_tools admit that command, including its --help form. 
  `cargo test -p kranz-engine grants_reach_worker_and_validator`
- **[a2]** A milestone's validator allowlist includes both the contract's command assertions and the worker-executed commands cited in that milestone's worker reports. 
  `cargo test -p kranz-engine validator_allowlist_includes`
- **[a3]** Plans and worker reports serialized before the new fields existed still deserialize, defaulting commandGrants and commandsRun to empty. 
  `cargo test -p kranz-engine backcompat_defaults_empty`
- **[a4]** The whole workspace test suite passes. 
  `bash -o pipefail -c "cargo test --workspace 2>&1 | grep -qE 'result: ok\. [1-9][0-9]* passed'"`
- **[a5]** The orchestrator and worker role prompts document the new commandGrants and commandsRun fields so future missions actually populate them. 
  `bash -c "grep -q commandGrants crates/engine/prompts/orchestrator.md && grep -q commandsRun crates/engine/prompts/worker.md"`

## Milestone 1 — Grants and worker-executed commands reach both surfaces via one source of truth

### 1.1 Add plan-level commandGrants to Plan, Mission, schema, reducer, and orchestrator prompt

Rust workspace; the engine crate is at `crates/engine` (package name `kranz-engine`). Introduce a plan-level command-grant set: read-only shell commands the plan declares as runnable by BOTH worker and validator sessions (the single source of truth this mission establishes). This feature only adds the field and carries it into mission state; wiring it into permission allowlists is a LATER feature — do not touch `permissions.rs` here.

Changes:
1. `crates/engine/src/types.rs`: add `pub command_grants: Vec<String>` to the `Plan` struct (around line 64) and to the `Mission` struct (around line 38). Annotate each with `#[serde(default, skip_serializing_if = "Vec::is_empty")]` so old JSON without the field deserializes to an empty vec and clean state stays byte-stable. Both structs use `#[serde(rename_all = "camelCase")]`, so the JSON key is `commandGrants`.
2. `crates/engine/src/reducer.rs`: in the `EventKind::PlanApproved { plan, base_sha }` arm (around line 50) add `state.mission.command_grants = plan.command_grants.clone();`. Also update the initial `Mission { .. }` struct literal (around line 315) to include `command_grants: Vec::new()`.
3. Update EVERY other `Plan { .. }` and `Mission { .. }` struct literal so the code compiles (Rust requires exhaustive literals). Known sites: `crates/engine/src/events.rs` `sample_plan()` (~257); `crates/engine/src/orchestrator.rs` (~3049 and ~4593). Grep `Plan {` and `Mission {` across `crates/` to be sure none are missed.
4. `crates/engine/src/orchestrator.rs` `plan_schema()` (~4400): add a `"commandGrants": { "type": "array", "items": { "type": "string" } }` property. It is OPTIONAL (do not add to `required`). The schema uses `additionalProperties: false`, so it MUST be listed or valid plans that include it would be rejected. Keep the existing `plan_schema_matches_plan_shape` test (~4591) green — update it if it enumerates expected fields.
5. `crates/engine/prompts/orchestrator.md`: near the plan JSON schema block, document a top-level `commandGrants` array: read-only shell commands granted mission-wide; every worker AND validator session may run them and their `--help` forms; use it for brief-granted command exceptions (e.g. a project CLI like `gc lint`) that validators must be able to re-run to independently verify a worker's claim. The word `commandGrants` must appear literally in this file.

Tests to add (in `types.rs`, `reducer.rs`, or `events.rs` unit-test modules):
- `command_grants_backcompat_defaults_empty`: deserialize a `Plan` from a JSON string that omits `commandGrants` and assert `command_grants.is_empty()`; do the same for a `Mission`/plan.approved payload string that omits it. (The mission's contract runs `cargo test -p kranz-engine backcompat_defaults_empty`.)
- A reducer test: fold a `plan.approved` event whose plan has `command_grants = vec!["gc lint".into()]` and assert `state.mission.command_grants == ["gc lint"]`.

Run `cargo test -p kranz-engine` and `cargo fmt --all` before reporting; cite the passing test output in testEvidence and list the exact commands you ran in commandsRun.

Done when:
- Plan and Mission each expose a serde-default `command_grants: Vec<String>` (JSON key `commandGrants`).
- The PlanApproved reducer copies command_grants from the plan into the mission; a reducer unit test proves this.
- plan_schema() advertises an optional commandGrants string-array property and plan_schema_matches_plan_shape still passes.
- A JSON plan/mission omitting commandGrants deserializes with an empty vec, proven by a test named command_grants_backcompat_defaults_empty.
- The literal token `commandGrants` appears in crates/engine/prompts/orchestrator.md.
- cargo test -p kranz-engine passes and the crate builds.

### 1.2 Fold plan-level grants into worker AND validator allowlists in permissions.rs

Depends on the previous feature (Mission now has `command_grants: Vec<String>`). Make plan-level grants flow into both the worker and the validator permission profiles, so a command the plan grants is runnable by the worker AND re-runnable by the validators verifying it. This is the fix for the m-d341a7 asymmetry where the worker ran `gc lint` but the scrutiny validator was denied it.

Background on `crates/engine/src/permissions.rs`: `for_role(role, cfg, validator_commands)` builds a `PermissionProfile`. The Worker branch grants bare `Bash` (acceptEdits). The Validator branches grant `Read/Glob/Grep` + `GIT_INSPECT` + `command_allow_patterns()` for each `validator_commands` entry ∪ `cfg.allow_validator_commands`. `command_allow_patterns("gc lint")` already yields `Bash(gc lint*)`, which matches `gc lint --help` — so granting a base command also fixes the `<cmd> --help` denial class from the brief.

Changes:
1. `crates/engine/src/permissions.rs`: add a new parameter `grants: &[String]` to `for_role`. For EACH grant, push `command_allow_patterns(grant)` into the allowed set for the Worker branch (alongside the existing `Bash`) AND for both validator branches (in addition to `validator_commands`). Keep the existing `dedup_preserving_order` behaviour so duplicates collapse. Do NOT remove the worker's bare `Bash` allow and do NOT change worker permission_mode. The Orchestrator branch ignores grants.
2. Thread `mission.command_grants` through the worker spec builder: `build_worker_spec` (~618 in `runner.rs`) and its callers `run_worker` (~502), `run_worker_in` (~540), `run_worker_in_buffered` (~584) each gain a `grants: &[String]` param passed down to the `permissions::for_role(role, cfg, &[], grants)` call (worker's validator_commands stays `&[]`). Update the orchestrator call site (`runner::run_worker(...)` around orchestrator.rs:1308) to pass `&self.state.mission.command_grants`.
3. `run_validator` (~706 in `runner.rs`): add a `grants: &[String]` param and pass it into `permissions::for_role(kind, cfg, &contract_commands, grants)`. Update the orchestrator call site (`validation_round`, ~2087) to pass `&self.state.mission.command_grants`.
4. The orchestrator's own read-only session (orchestrator.rs ~2819 `permissions::for_role(Role::Orchestrator, &cfg, &[])`) passes `&[]` for grants — orchestrator needs no grants.
5. Update `permissions.rs` doc comments to describe the grants parameter.

Test to add in `permissions.rs` (unit test, no backend needed) named `grants_reach_worker_and_validator`: with `MissionConfig::default()` and `grants = vec!["gc lint".to_string()]`, build the Worker profile and a Validator profile (ValidatorScrutiny) via `for_role(..., &grants)`, then `permissions::apply` each onto a fresh `SessionSpec` (or inspect the profiles directly) and assert BOTH `allowed_tools` contain `"Bash(gc lint*)"`. Since `Bash(gc lint*)` matches `gc lint --help`, add an assertion/comment making the `--help` coverage explicit (e.g. assert `command_allow_patterns("gc lint").contains(&"Bash(gc lint*)".to_string())`).

Run `cargo test -p kranz-engine` and `cargo fmt --all`; cite passing output in testEvidence and list your commands in commandsRun.

Done when:
- for_role takes a grants slice and applies command_allow_patterns(grant) to both the worker profile and both validator profiles.
- mission.command_grants is threaded through the worker spec builders and run_validator to the for_role calls; the orchestrator spawn passes an empty grants slice.
- Workers retain their bare Bash allow and acceptEdits mode (grants are additive, not a tightening).
- A test named grants_reach_worker_and_validator proves a granted command's allow pattern (Bash(gc lint*), which covers gc lint --help) is present in both the worker and validator allowed_tools.
- cargo test -p kranz-engine grants_reach_worker_and_validator passes and the workspace builds.

### 1.3 Add commandsRun to WorkerReport, its JSON schema, and the worker prompt

Give the worker a structured place to declare the verification commands it actually ran, so those can become first-class validator allowlist entries (the next feature harvests them). Today `WorkerReport` only has free-text `testEvidence`.

Changes:
1. `crates/engine/src/types.rs`: add `#[serde(default)] pub commands_run: Vec<String>` to the `WorkerReport` struct (~250). JSON key is `commandsRun` (struct uses camelCase). Grep `WorkerReport {` across `crates/` and update any struct literals (tests) so the crate compiles.
2. `crates/engine/src/runner.rs` `worker_report_schema()` (~443): add `"commandsRun": { "type": "array", "items": { "type": "string" } }` to the schema properties (the object is closed with `additionalProperties: false`, so the model can only emit fields the schema lists — this is required for workers to populate it).
3. `crates/engine/prompts/worker.md`: instruct the worker to populate `commandsRun` with the exact shell commands it executed to verify its work (test/build/lint invocations, e.g. `cargo test -p kranz-engine foo`, a project CLI like `gc lint`) so validators can re-run them to independently confirm the claim. The literal token `commandsRun` must appear in this file.

Test to add named `commands_run_backcompat_defaults_empty`: deserialize a `WorkerReport` from a JSON string that omits `commandsRun` (include only `result` and `summary`) and assert `commands_run.is_empty()`.

Run `cargo test -p kranz-engine` and `cargo fmt --all`; cite passing output in testEvidence and list your commands in commandsRun.

Done when:
- WorkerReport exposes a serde-default `commands_run: Vec<String>` (JSON key commandsRun).
- worker_report_schema() advertises the commandsRun string-array property.
- crates/engine/prompts/worker.md instructs the worker to fill commandsRun and contains the literal token commandsRun.
- A worker report JSON omitting commandsRun deserializes with an empty vec, proven by a test named commands_run_backcompat_defaults_empty.
- cargo test -p kranz-engine passes and the crate builds.

### 1.4 Harvest worker-executed commands into each milestone's validator allowlists

Depends on the two prior features (validators now receive a grants slice via run_validator; WorkerReport now has commands_run). Close the loop: when validating a milestone, the validators must be allowed to re-run the commands the milestone's workers actually executed and cited in their reports.

Changes:
1. Add a pure helper in `crates/engine/src/orchestrator.rs` (or a small module it can call), e.g. `fn worker_commands_for_milestone(state: &MissionState, milestone: &Milestone) -> Vec<String>`: iterate `milestone.features` -> `feature.worker_runs` -> `state.runs[run_id].report` -> `report.commands_run`, collecting commands in first-seen order with duplicates removed. Skip runs with no report. Make it `pub(crate)`/testable.
2. `crates/engine/src/runner.rs` `run_validator`: add a `worker_commands: &[String]` parameter. Merge these into the command set handed to permissions: pass `contract_commands` ∪ `worker_commands` as the `validator_commands` argument to `permissions::for_role` (so each gets `command_allow_patterns`). ALSO fold `worker_commands` into the validator prompt's runnable-commands bullet list (the `allowed_commands` built around lines 757-761) so the validator knows it may run them.
3. `crates/engine/src/orchestrator.rs` `validation_round` (~2057): compute `worker_commands_for_milestone(&self.state, &milestone)` and pass it into each `runner::run_validator(...)` call (~2087) as the new argument, alongside the grants argument added in the earlier feature.

Tests to add:
- `worker_commands_for_milestone` collects and de-duplicates commands from the milestone's completed features' worker reports (build a small MissionState fixture with a milestone, a feature whose worker_runs reference a WorkerRun whose report.commands_run = ["gc lint", "gc lint"], assert the helper returns ["gc lint"]).
- `validator_allowlist_includes_contract_and_worker_commands`: given a contract with a command assertion `cargo test` and worker_commands `["gc lint"]`, assemble the validator profile (call the same code path run_validator uses to build for_role's validator_commands — i.e. contract ∪ worker_commands, then `permissions::for_role(ValidatorScrutiny, cfg, &combined, &[])`) and assert `allowed_tools` contains BOTH `Bash(cargo test*)` and `Bash(gc lint*)`. (The mission contract runs `cargo test -p kranz-engine validator_allowlist_includes`.)

Run `cargo test -p kranz-engine` and `cargo fmt --all`; cite passing output in testEvidence and list your commands in commandsRun.

Done when:
- A tested helper gathers de-duplicated commands_run from a milestone's completed features' worker reports.
- run_validator accepts worker_commands and includes them (via command_allow_patterns) in the validator allowlist and in the validator prompt's runnable-commands list.
- validation_round computes the milestone's worker commands and passes them into run_validator.
- A test named validator_allowlist_includes_contract_and_worker_commands proves the validator allowlist contains both a contract command pattern and a worker-executed command pattern.
- cargo test -p kranz-engine validator_allowlist_includes passes and the workspace builds.

