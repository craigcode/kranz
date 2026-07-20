# Mission plan — m-0f1abd

**Goal:** Record model provenance (id, quant, weight-hash) on every run and add a derived, event-log-regenerable export that yields a fine-tuning-ready dataset of validation-PASSED worker traces in instruction-pair form, shipped frontier-first.

Branch `kranz/mission-m-0f1abd` (from `main`). Approved plan of record; the machine-readable twin is [plan.json](plan.json). Live status: `kranz status` or the dashboard.

## Cost estimate

Estimated **$7.53 – $37.63** (expected ~$16.39). Rough estimate — live usage is authoritative; based on 41 completed mission(s).

## Considered alternatives

**Chosen approach:** Add provenance (quant, weight_hash) additively onto the existing worker.spawned/WorkerRun where model already lives; derive each trace's validation outcome from the milestone/feature fold rather than storing it; build the export as a pure event-log function mirroring render_mission_report; ship frontier-first (quant "n/a", no weight-hash) and prove the local weight-hash path with a fixture rather than a real backend.

Rejected shapes:
- **Stamp the validation outcome as a field on worker.completed / WorkerRun.** — Temporally impossible — a worker completes before its milestone is validated — and it creates a second source of truth that drifts from the event log; the fold already carries the outcome.
- **Introduce a dedicated new run.provenance event instead of extending worker.spawned.** — Adds an event type and a reducer arm and splits provenance from the model field it belongs with, with no back-compat advantage over serde-default fields on the existing event.
- **Hydrate the real committed git diff as the trace response for richer fine-tuning data.** — Makes the export depend on git working-tree state, breaking the 'regenerable purely from the log' invariant that report.md establishes; deferred to a later ticket.

## Validation contract

Defined before any feature; gates mission completion.

- **[a1]** The worker-spawn/run schema additively carries model provenance (quant and weight-hash) alongside the existing model id, and a log/state written before these fields still deserializes and folds (old logs default quant to "n/a" and weight-hash to absent).
  `cargo test --workspace provenance 2>&1 | grep -qE 'test result: ok\. [1-9]'`
- **[a2]** A frontier turn records quant "n/a" and no weight-hash, while a local turn (fixture) records a populated content weight-hash of the weight file plus its quant, and both fold through onto the run record intact.
  `cargo test --workspace weight_hash 2>&1 | grep -qE 'test result: ok\. [1-9]'`
- **[a3]** The trace export includes only validation-PASSED worker traces: a worker run whose feature and milestone reached Complete is exported, while a run on a failed or skipped feature is excluded.
  `cargo test --workspace passed_only 2>&1 | grep -qE 'test result: ok\. [1-9]'`
- **[a4]** The export is a pure function of the event log with no persisted second source of truth: regenerating it from the same log yields byte-identical output across repeated calls.
  `cargo test --workspace regenerable 2>&1 | grep -qE 'test result: ok\. [1-9]'`
- **[a5]** Each exported trace is a well-formed instruction-pair record (instruction, response, and model provenance) serialized as one valid JSON object per JSONL line.
  `cargo test --workspace instruction_pair 2>&1 | grep -qE 'test result: ok\. [1-9]'`
- **[a6]** The `kranz export-traces` CLI path emits the validation-passed instruction-pair JSONL for a mission by loading and folding its event log on demand (regenerable), and its output is stable across consecutive invocations over an unchanged log.
  `cargo test --workspace export_traces 2>&1 | grep -qE 'test result: ok\. [1-9]'`
- **[a7]** The whole workspace's test suite passes (no regression introduced by the schema or export changes).
  `cargo test --workspace 2>&1 | grep -qE 'test result: ok\. [1-9]'`
- **[a8]** The workspace is clippy-clean across all targets with warnings denied.
  `cargo clippy --workspace --all-targets -- -D warnings`
- **[a9]** All code is rustfmt-formatted (CI gates `cargo fmt --all --check`).
  `cargo fmt --all --check`

## Milestone 1 — M1 — Model provenance recorded on every run/turn (frontier-first, additive)

### 1.1 Additive provenance schema on the spawn event + run record, folded, back-compat preserved

Add model-provenance fields to the worker-spawn event and the folded run record, strictly additively per AGENTS.md rule 6 (contract files `crates/engine/src/events.rs` and `types.rs` are additive-only: `#[serde(default)]`, never break old logs). Add two fields alongside the existing `model` on `EventKind::WorkerSpawned` (events.rs) and mirror them onto `WorkerRun` (types.rs): `quant: String` and `weight_hash: Option<String>`. `model` stays exactly as today (the frontier model id/alias). Because serde's default for `String` is "" (not "n/a"), give `quant` a custom serde default function returning "n/a" (e.g. `#[serde(default = "default_quant")]` with `fn default_quant() -> String { "n/a".into() }`) so a spawn/run JSON written before this field folds to quant == "n/a"; `weight_hash` uses `#[serde(default, skip_serializing_if = "Option::is_none")]`. Thread the new fields through the reducer's `WorkerSpawned` arm (crates/engine/src/reducer.rs, ~line 192) so the folded `WorkerRun` carries them. Adding fields to the `WorkerSpawned` enum variant forces every construction site to compile — update ALL of them (production emit sites in orchestrator.rs and runner.rs, and any `WorkerSpawned { .. }` constructed in test modules across the engine/cli/server crates) to set `quant: "n/a".into(), weight_hash: None`, which is the correct frontier value. Do NOT store any validation outcome on the event or run here — that is derived in M2 (a worker completes before its milestone is validated, so the outcome does not exist at spawn/completion time). Add unit tests mirroring the existing back-compat precedents (`plan_approved_base_sha_backcompat`, `finding_class_backcompat_defaults_empty` in events.rs/types.rs): a spawn-event JSON omitting quant/weightHash deserializes with quant=="n/a" and weight_hash==None; a spawn with quant "q4_k_m" and weight_hash Some("<hex>") round-trips through serde and through `reducer::fold` onto the run. Name at least one test with the substring `provenance` and one with `weight_hash`. Run the full gate (cargo test --workspace, clippy --workspace --all-targets -D warnings, fmt --all --check) before reporting.

Done when:
- A `worker.spawned` event JSON with no quant/weightHash keys deserializes with quant == "n/a" and weight_hash == None (test name contains `provenance`).
- A spawn carrying quant "q4_k_m" and a populated weight_hash round-trips through serde and through reducer::fold so the resulting WorkerRun carries both (test name contains `weight_hash`).
- No existing field on WorkerSpawned/WorkerRun is removed, renamed, or made non-defaulting; the whole workspace compiles and `cargo test --workspace provenance` reports ≥1 passing test.
- cargo clippy --workspace --all-targets -- -D warnings and cargo fmt --all --check both pass.

### 1.2 Frontier emit-site correctness + local weight-hash fixture proof

With the schema in place, prove the two provenance regimes. (1) Frontier: assert that in a completed mission driven by the existing CLI backends (Claude/Codex/Droid — all frontier), every `worker.spawned` records quant == "n/a" and no weight-hash. Use the existing mock-backend mission test harness (see crates/engine/tests/mission_test.rs and the mission-driving tests in orchestrator.rs) to run a small mission to completion and scan its events. (2) Local: add a test-only fixture/helper that constructs a *local* spawn event as `backend_local` would once it lands — `quant: "q4_k_m"`, `weight_hash: Some(<64-hex content hash of a stubbed GGUF>)` — and assert it folds onto the run with the weight-hash intact, proving the schema satisfies the sovereignty-pinning requirement (a bare model name like qwen3-coder-30b-q4 is unauditable; the content hash pins the exact weights). Do NOT build a real local backend or any HTTP inference path — a fixture/stub is the deliverable per the ticket. Name the frontier test with substring `provenance` (or `frontier`) and the local test with substring `weight_hash`. Full gate before reporting.

Done when:
- Every `worker.spawned` in a completed mock-backend mission records quant == "n/a" and weight_hash == None (frontier regime).
- A local-turn fixture folds to a run whose weight_hash is Some(<hash>) and whose quant names the quantisation (test name contains `weight_hash`).
- No production local backend is added; the change is test/fixture-only beyond M1's schema.
- cargo test --workspace passes; clippy and fmt gates pass.


## Milestone 2 — M2 — Derived, regenerable validated-trace export (instruction-pair dataset)

### 2.1 trace_export engine module: pure passed-only derivation + instruction-pair JSONL

Add a new engine module `crates/engine/src/trace_export.rs` (register in crates/engine/src/lib.rs) that mirrors the derive-and-regenerable pattern of `render_mission_report` (orchestrator.rs:6111, called from try_write_mission_report which reads the event log + folded state and never stores a second source of truth). Define `InstructionPair` (serde camelCase) with fields: instruction, response, model, quant, weightHash (Option, skip_if None), missionId, featureId, runId. Provide `pub fn export_validated_traces(state: &MissionState, events: &[Event]) -> Vec<InstructionPair>` and `pub fn to_jsonl(pairs: &[InstructionPair]) -> String` (one compact serde_json line per pair, trailing newline per line, deterministic field order via the struct). Selection rule — validation-PASSED, DERIVED not stored: include a run iff `run.role == Role::Worker` AND its feature's status is `Complete` AND that feature's milestone status is `Complete` (the milestone passed validation with no surviving findings). Exclude non-worker roles, and runs on Failed/Skipped features. instruction = the feature's spec followed by its validation criteria (the task as given to the worker); response = the run's `WorkerReport` rendered from the log only (summary + test_evidence + commits list) — event-log-only, NO git-diff hydration (that is deferred to a later ticket to preserve pure regenerability). Provenance (model/quant/weightHash) comes from the WorkerRun. Iterate runs deterministically (state.runs is a BTreeMap; keep a stable order). Add tests: a fixture MissionState/event log with one milestone Complete containing a passed feature (with a worker run + report) and one FAILED feature (with its own worker run) — assert the export contains exactly the passed feature's trace and excludes the failed one (test name `passed_only`); assert `to_jsonl` output is byte-identical across two calls on the same input (test name `regenerable`); assert each emitted line parses as one JSON object carrying instruction+response+model+quant and the weight-hash when present (test name `instruction_pair`). Full gate before reporting.

Done when:
- Given a fixture log with a Complete milestone holding one passed feature and one failed feature, export_validated_traces returns the passed feature's worker trace and omits the failed feature's (test name contains `passed_only`).
- to_jsonl over the same input is byte-identical across repeated calls and the module writes no persisted dataset file (test name contains `regenerable`).
- Each JSONL line is a single valid JSON object with instruction, response, model, quant, and weightHash-when-local (test name contains `instruction_pair`).
- cargo test --workspace passed_only, regenerable, and instruction_pair each report ≥1 passing test; clippy and fmt gates pass.

### 2.2 kranz export-traces CLI path (regenerable on demand)

Add a `kranz export-traces` subcommand in crates/cli (define the arg in cli.rs, dispatch in commands.rs/main.rs following the existing subcommand pattern — study a nearby read-only subcommand such as tail/ready for the shape). Accept `<mission-id>` (single mission) and `--all` (every mission under .kranz/missions), plus optional `--out <path>` (default: stdout). Implementation: for each target mission, load its event log via `EventLog::read_events(paths.events_file())`, fold with `reducer::fold`, call `trace_export::export_validated_traces`, and emit `trace_export::to_jsonl`. It must read only the existing event log and regenerate on demand — never write a tracked/committed second copy (mirrors report.md being regenerable from the log). Add a CLI integration test (crates/cli/tests) that points the subcommand at a fixture mission directory and asserts: the emitted JSONL contains only validation-passed instruction pairs, and two consecutive invocations over the unchanged log produce identical bytes. Name a test with substring `export_traces`. Full gate before reporting.

Done when:
- `kranz export-traces <mission-id>` prints JSONL containing only that mission's validation-passed instruction pairs, loaded and folded from the event log at invocation time.
- Two consecutive invocations over an unchanged log produce byte-identical output, and no tracked dataset file is created (regenerable).
- `--all` aggregates passed traces across missions under .kranz/missions without error when a mission log is missing/unreadable (skipped, not fatal).
- cargo test --workspace export_traces reports ≥1 passing test; clippy and fmt gates pass.
