# Research — m-0f1abd

Evidence behind the approved plan (roadmap M1 / repo-knowledge-store slice 1). Candidate knowledge updates feed `docs/knowledge/`.

## Files & docs read

- crates/engine/src/events.rs
- crates/engine/src/types.rs
- crates/engine/src/reducer.rs
- crates/engine/src/cost.rs
- crates/engine/src/orchestrator.rs
- crates/engine/src/pr_handoff.rs
- AGENTS.md
- docs/scoping/local-inference-executor-tier.md
- .kranz/tickets/local-inference-trace-provenance-flywheel.md

## External sources

- KRZ-207 in docs/scoping/local-inference-executor-tier.md §4 and review addendum §3/§5
- .kranz/tickets/local-inference-trace-provenance-flywheel.md

## Facts

- events.rs and types.rs are contract files but additive-only: add #[serde(default)] fields, never break old logs; schema changes are deliberate. — `AGENTS.md rule 6; header comments at crates/engine/src/events.rs:1-13 and types.rs:1-8`
- Model identity is already recorded per run: worker.spawned carries `model: String`, folded onto WorkerRun.model. — `events.rs:134 (WorkerSpawned.model); reducer.rs:192-236 (WorkerSpawned fold into WorkerRun)`
- report.md is derived-and-regenerable via a pure function over folded state + the event log, with no persisted second source of truth — the pattern the export must mirror. — `orchestrator.rs:6111 render_mission_report(state, events, plan, ...); orchestrator.rs:4388 try_write_mission_report reads EventLog + fold and regenerates`
- A worker.completed is folded with result/tokens/cost but no validation information; validation outcome only appears later via milestone.completed/validation.finding — so it must be derived, not stored on the completion turn. — `reducer.rs:246-261 (WorkerCompleted arm) vs reducer.rs:335-337 (MilestoneCompleted) and events.rs:191-229`
- serde's String default is empty, so quant needs a custom default fn to fold old logs to "n/a"; Option fields default to None cleanly (existing precedent). — `types.rs base_sha/finding class use #[serde(default, skip_serializing_if=...)]; events.rs plan_approved_base_sha_backcompat test at events.rs:396`
- No trace-export/instruction-pair/dataset module exists in the engine today; the otel code in crates/cli is unrelated telemetry. — `grep for export|instruction.pair|fine.?tun|dataset across crates matched only otel/* and unrelated files`

## Ambiguities & stale docs

- Ticket says events should carry 'its validation outcome' on the completion turn; resolved by deriving the outcome from milestone/feature completion (report.md-mirror) because it is temporally unavailable at completion and storing it would create a drifting second source of truth.
- 'Instruction-pair' content source was unspecified; resolved to event-log-only (instruction = feature spec + criteria; response = the worker report) with real git-diff hydration deferred, to preserve pure regenerability.
- Whether a CLI surface is in scope; resolved to include a thin read-only `kranz export-traces` path since an export 'path' implies something invocable, kept regenerable-on-demand.

## Candidate knowledge updates

- Add a knowledge note: 'Validated-trace export is derived-and-regenerable from the event log (mirror render_mission_report); the per-trace validation outcome is derived from milestone/feature completion, never stored on worker.completed.'
- Add a knowledge note: 'Model provenance lives on worker.spawned/WorkerRun as model + quant + weight_hash; frontier turns record quant "n/a" and no weight-hash, local turns must record a content hash of the weight file + quant for auditable version-pinning.'
