# Mission plan — m-8b7db0

**Goal:** Add an opt-in `kranz otel --endpoint <otlp>` sidecar that tails mission event logs and exports OpenTelemetry spans (mission trace root, milestone and run child spans, cost/token/status attributes), entirely read-side with zero engine changes.

Branch `kranz/mission-m-8b7db0` (from `main`). Approved plan of record; the machine-readable twin is [plan.json](plan.json). Live status: `kranz status` or the dashboard.

## Validation contract

Defined before any feature; gates mission completion.

- **[a1]** A folded complete mission maps to exactly one root span whose 16-byte trace id derives from the mission id and which spans mission.created to the terminal event (start/end times taken from those events' timestamps). 
  `cargo test -p kranz mission_maps_to_trace_root 2>&1 | grep -qE 'test result: ok\. [1-9][0-9]* passed'`
- **[a2]** A worker.spawned/worker.completed pair maps to one run span carrying the run's cost (kranz.cost.usd) and all four token counts (kranz.tokens.input/output/cache_read/cache_write) as attributes, plus role and model. 
  `cargo test -p kranz run_span_carries_cost_and_token_attributes 2>&1 | grep -qE 'test result: ok\. [1-9][0-9]* passed'`
- **[a3]** Terminal statuses map to OTel span status: run Pass->Ok, Fail/Partial->Error; mission Completed->Ok, Failed/Abandoned->Error; milestone Completed->Ok, Blocked->Error. 
  `cargo test -p kranz terminal_status_maps_to_span_status 2>&1 | grep -qE 'test result: ok\. [1-9][0-9]* passed'`
- **[a4]** Span and trace ids are derived deterministically from mission id + opening-event seq, so mapping the same event log twice yields byte-identical trace/span ids (idempotent for deduping backends). 
  `cargo test -p kranz ids_are_deterministic_and_idempotent 2>&1 | grep -qE 'test result: ok\. [1-9][0-9]* passed'`
- **[a5]** Spans nest correctly: milestone spans parent to the mission root; run spans parent to their milestone (via milestoneId, or featureId f-<m>-<f> -> ms-<m>) and to the root when they have neither. 
  `cargo test -p kranz spans_parent_correctly 2>&1 | grep -qE 'test result: ok\. [1-9][0-9]* passed'`
- **[a6]** Replay derives span start/end times from event timestamps, never wall-clock now(): mapping a fixed historical log produces spans with the exact timestamps recorded in the events. 
  `cargo test -p kranz replay_uses_event_timestamps 2>&1 | grep -qE 'test result: ok\. [1-9][0-9]* passed'`
- **[a7]** The kranz (cli) crate test suite passes in full, including the OpenTelemetry mapping tests. 
  `cargo test -p kranz`
- **[a8]** `kranz otel --from-start --endpoint <url>` actually exports OTLP spans over HTTP: a local mock OTLP receiver stood up by the test captures at least one non-empty ExportTraceServiceRequest. 
  `cargo test -p kranz --test otel_export 2>&1 | grep -qE 'test result: ok\. [1-9][0-9]* passed'`
- **[a9]** The whole workspace is clippy-clean with warnings denied. 
  `cargo clippy --workspace --all-targets -- -D warnings`
- **[a10]** No file under crates/engine changed relative to the pinned pre-mission base commit (the exporter is entirely read-side). 
  `test -z "$(git diff --name-only "$KRANZ_BASE_SHA" -- crates/engine)"`
- **[a11]** docs/otel.md exists and documents endpoint configuration, the id mapping, and replay semantics. 
  `test -f docs/otel.md && grep -qi endpoint docs/otel.md && grep -qi 'trace\|span id\|id mapping' docs/otel.md && grep -qi replay docs/otel.md`
- **[a12]** docs/otel.md is accurate and complete: the documented id-derivation formula, attribute names, status mapping, and live-vs-replay/incomplete-span semantics match the actual implementation. *(agent judgement)*

## Milestone 1 — Pure event-to-span mapping (deterministic, attributed, fully unit-tested)

### 1.1 Neutral span model + deterministic id derivation

Create a NEW module in the `kranz` cli crate (package `kranz`, lib name `kranz_cli`, sources under crates/cli/src). Add `pub mod otel;` in crates/cli/src/lib.rs and create crates/cli/src/otel/mod.rs plus crates/cli/src/otel/map.rs (or a single crates/cli/src/otel.rs with a `map` submodule — either is fine, but keep the pure mapping logic in its own submodule so it is testable without any OTLP/network dependency).

Do NOT touch anything under crates/engine (hard constraint). You only CONSUME engine read-side APIs: `kranz_engine::events::{Event, EventKind}`, `kranz_engine::types::{Role, RunResult, MissionStatus, MilestoneStatus, TokenUsage}`. Study crates/engine/src/events.rs and crates/engine/src/types.rs for exact shapes (they are already in this repo).

Deliver two things in this feature:

1. A neutral, transport-agnostic span record type, e.g.:
   ```
   pub struct MissionSpan {
     pub trace_id: [u8; 16],
     pub span_id: [u8; 8],
     pub parent_span_id: Option<[u8; 8]>,
     pub name: String,
     pub start: chrono::DateTime<chrono::Utc>,
     pub end: chrono::DateTime<chrono::Utc>,
     pub attributes: Vec<(String, AttrValue)>, // AttrValue = String|I64|F64 (a small enum)
     pub status: SpanStatus, // enum { Ok, Error(String), Unset }
   }
   ```
   Keep it dependency-free (no opentelemetry types yet — that comes in a later feature). Attributes ordering should be deterministic.

2. Deterministic id derivation, e.g. `pub fn trace_id(mission_id: &str) -> [u8;16]` = first 16 bytes of sha256(mission_id); `pub fn span_id(mission_id: &str, open_seq: u64) -> [u8;8]` = first 8 bytes of sha256(format!("{mission_id}:{open_seq}")). Use the `sha2` crate (already a workspace dependency — add `sha2.workspace = true` to crates/cli/Cargo.toml [dependencies]; do NOT add it to the engine). `open_seq` is the seq of the span's OPENING event (mission.created for the root, milestone.started for a milestone, worker.spawned for a run).

Unit tests (this is what the contract greps for by name — use these EXACT test fn names):
- `ids_are_deterministic_and_idempotent`: same (mission_id, seq) yields identical bytes across calls; trace_id is 16 bytes and span_id is 8 bytes; two different missions get different trace ids; two different seqs in one mission get different span ids.

No engine edits, no new heavy deps in this feature (only `sha2`, already vendored).

Done when:
- A `MissionSpan` (or equivalently named) neutral span record type exists in the cli crate's otel module with trace_id [u8;16], span_id [u8;8], optional parent_span_id, name, start/end DateTime<Utc>, deterministically-ordered attributes, and a status enum — with no opentelemetry dependency.
- `trace_id(mission_id)` returns the first 16 bytes of sha256(mission_id) and `span_id(mission_id, seq)` the first 8 bytes of sha256("{mission_id}:{seq}"), both pure and total.
- A unit test named exactly `ids_are_deterministic_and_idempotent` passes and asserts: byte-length 16/8, call-idempotency, distinct trace ids for distinct missions, distinct span ids for distinct seqs.

### 1.2 Fold events into closed spans with attributes, status and parenting

Building on the neutral span model + id derivation from the previous feature (same crates/cli/src/otel module), implement the pure fold that turns a mission's events into finished spans:

`pub fn map_mission(events: &[kranz_engine::events::Event]) -> Vec<MissionSpan>`

Semantics (a span is emitted ONLY when its closing event is present — mirror OTel, which exports on span end):
- ROOT span: opens at `mission.created` (seq of that event), closes at the terminal event (`mission.completed` | `mission.failed` | `mission.abandoned`). If the log has no terminal event (still-running mission), DO NOT emit the root span. name e.g. `"mission <id>"`. Attributes: kranz.mission.id, kranz.mission.goal, kranz.mission.status (final), and mission totals kranz.cost.usd + kranz.tokens.{input,output,cache_read,cache_write} (sum across runs, or fold via the reducer). Status: Completed->Ok, Failed->Error(reason), Abandoned->Error(reason).
- MILESTONE spans: open at `milestone.started` (has milestoneId, startSha), close at `milestone.completed`. If never completed, do not emit. name e.g. `"milestone <milestoneId>: <title>"` (title from the folded state / plan). Parent = root span id. Attributes: kranz.milestone.id, kranz.milestone.title, kranz.milestone.status, kranz.milestone.fix_cycles. Status: Complete->Ok, Blocked->Error. (A milestone.blocked without a later completion leaves the span unclosed -> not emitted; that is acceptable.)
- RUN spans: open at `worker.spawned` (runId, role, model, optional featureId, optional milestoneId), close at `worker.completed` (result, tokens, costUsd). This covers ALL roles including the orchestrator (runId like `orch-1`). name e.g. `"<role> <runId>"`. Attributes: kranz.run.id, kranz.role (kebab like the enum), kranz.model, kranz.run.result, kranz.cost.usd (if present), kranz.tokens.{input,output,cache_read,cache_write}, plus kranz.feature.id / kranz.milestone.id when present. Status: Pass->Ok, Fail->Error, Partial->Error. Parent: the run's milestone span when it has a milestoneId; else derive the milestone from featureId `f-<m>-<f>` -> `ms-<m>` and parent to that milestone span; else (no milestone/feature, e.g. orchestrator) parent = root span id.
- All start/end times come from the corresponding events' `ts` fields (chrono DateTime<Utc>) — NEVER now(). This is the replay-fidelity guarantee.

You may fold the log with `kranz_engine::reducer::fold` to recover milestone titles / feature->milestone structure, or track opening events in a single pass — your choice, but keep it pure and deterministic. Attribute vectors must be built in a stable order so tests are stable.

Unit tests (use these EXACT fn names — the contract greps them):
- `mission_maps_to_trace_root`: a complete synthetic log yields exactly one root span, trace_id == trace_id(mission_id), start==created ts, end==terminal ts.
- `run_span_carries_cost_and_token_attributes`: a spawned+completed run yields a span with kranz.cost.usd and all four kranz.tokens.* attributes matching the event payload, plus role and model.
- `terminal_status_maps_to_span_status`: covers run Pass->Ok, Fail->Error, Partial->Error; mission Completed->Ok, Failed->Error, Abandoned->Error; milestone Complete->Ok, Blocked->Error.
- `spans_parent_correctly`: a run with featureId f-2-1 parents to the ms-2 milestone span; a validator run with milestoneId ms-1 parents to ms-1; an orchestrator run parents to the root; every milestone parents to the root.
- `replay_uses_event_timestamps`: build a log with fixed, non-now timestamps and assert every emitted span's start/end equal the event timestamps exactly.

Build small synthetic Event vectors in-test (construct EventKind variants directly). No engine edits.

Done when:
- `map_mission(&[Event]) -> Vec<MissionSpan>` emits a root span only when a terminal mission event is present, and its trace id and start/end timestamps come from mission.created and the terminal event.
- Milestone spans (started->completed) and run spans (worker.spawned->worker.completed, all roles) are emitted only when closed, with the documented kranz.* attributes and start/end from event timestamps.
- Run/milestone parenting resolves as specified (milestoneId direct, featureId f-<m>-<f> -> ms-<m>, orchestrator/none -> root; milestones -> root).
- Span status maps Pass/Complete/Completed->Ok and Fail/Partial/Blocked/Failed/Abandoned->Error.
- Unit tests named exactly mission_maps_to_trace_root, run_span_carries_cost_and_token_attributes, terminal_status_maps_to_span_status, spans_parent_correctly, and replay_uses_event_timestamps all pass.


## Milestone 2 — kranz otel subcommand streams spans to an OTLP HTTP endpoint

### 2.1 OTLP HTTP exporter adapter + dependencies

Add the OpenTelemetry dependencies and a THIN adapter that turns the neutral `MissionSpan` records (from the mapping module) into OTel SpanData and exports them over OTLP HTTP/protobuf. Keep this separate from the pure mapping module so the mapping tests stay dependency-free.

Dependencies — add to the workspace [workspace.dependencies] in the root Cargo.toml and reference them with `.workspace = true` in crates/cli/Cargo.toml ONLY (never crates/engine):
- `opentelemetry` (API types: TraceId, SpanId, KeyValue, Value, trace::Status, trace::SpanKind).
- `opentelemetry_sdk` (trace::SpanData / export::trace::SpanData, Resource, and the SpanExporter trait).
- `opentelemetry-otlp` configured for OTLP HTTP/protobuf over reqwest+rustls, with default features DISABLED so it does NOT pull in grpc-tonic. Select the minimal feature set for the version cargo resolves (typically something like `http-proto` plus a reqwest/rustls client feature). After wiring, run `cargo tree -p kranz -i tonic` (or `cargo tree | grep -i tonic`) and CONFIRM tonic/grpc is absent from the tree; the mission requires justifying each dep and staying minimal.
In the commit message, list each new crate and one line on why it is needed and why grpc was avoided.

Adapter (e.g. crates/cli/src/otel/emit.rs):
- `fn to_span_data(span: &MissionSpan, resource: ...) -> opentelemetry_sdk::...::SpanData` mapping trace_id/span_id via `TraceId::from_bytes`/`SpanId::from_bytes`, parent via SpanId (or SpanId::INVALID when None), start/end DateTime<Utc> -> SystemTime, attributes -> Vec<KeyValue> (String/i64/f64), and SpanStatus -> opentelemetry Status (Ok / Error{description}). Set span kind Internal.
- An exporter constructor `fn build_exporter(endpoint: &str) -> Result<impl SpanExporter>` using opentelemetry-otlp's HTTP span exporter pointed at the given endpoint (an http(s) URL such as http://localhost:4318/v1/traces).
- An async `export_spans(exporter, spans: Vec<MissionSpan>)` that batches and exports; log and continue on export error (a sidecar must never panic the tail loop).

Tests:
- A unit test asserting to_span_data preserves ids (from_bytes round-trip), timestamps, and status without a network.
- An INTEGRATION test at crates/cli/tests/otel_export.rs named e.g. `from_start_exports_spans_to_endpoint` (contract runs `cargo test -p kranz --test otel_export`): stand up a minimal local HTTP server on 127.0.0.1:0 (you may use the `axum`/`tokio` stack already available to the workspace, or a raw tokio TcpListener — bind to port 0 and read the actual port) that accepts POSTs to /v1/traces and records that at least one request with a non-empty body arrived; construct a small in-memory set of MissionSpans (or a synthetic events.jsonl) and run the export path against `http://127.0.0.1:<port>/v1/traces`; assert the receiver captured >= 1 export request. Keep it hermetic and fast; no external services.

No engine edits.

Done when:
- opentelemetry, opentelemetry_sdk, and opentelemetry-otlp are added to the workspace and referenced only by the cli crate; opentelemetry-otlp is configured for HTTP/protobuf with default-features disabled and no grpc-tonic in the dependency tree (verified via cargo tree).
- An adapter converts MissionSpan -> OTel SpanData preserving the 16/8-byte trace/span ids, parent, event timestamps, attributes, and Ok/Error status, verified by a no-network unit test.
- An integration test `crates/cli/tests/otel_export.rs` runnable via `cargo test -p kranz --test otel_export` stands up a local mock OTLP HTTP receiver, runs the export path against it, and asserts at least one non-empty export request was received.
- The commit message enumerates and justifies each new dependency and records that gRPC/tonic was deliberately avoided.

### 2.2 `kranz otel` CLI command and tail-fold-export loop

Add the user-facing subcommand and its runtime loop, mirroring the existing read-side patterns: crates/cli/src/tail.rs (`tail_events`) and crates/slack/src/bridge.rs (`run_bridge` — poll mission dirs, incremental fold, self-healing). Read both before starting; reuse `kranz_engine::event_log::EventLog::{read_events, read_events_after}`, `kranz_engine::paths::MissionPaths::{list_missions, new, events_file}`.

1. clap surface: add a `Otel` variant to `Command` in crates/cli/src/cli.rs with:
   - `--endpoint <URL>` (required): the OTLP HTTP traces endpoint (e.g. http://localhost:4318/v1/traces).
   - `--from-start` (bool): replay each mission's full log (spans built from event timestamps) and then continue following live. Default (flag absent): seed each mission cursor at its current head seq (like bridge.rs first-sighting) and only export spans whose opening AND closing events arrive during the tail.
   The command honors the existing global `--repo` (defaults to cwd) and `--mission` (scope to a single mission id; absent = all missions under the repo).

2. Dispatch: wire `Command::Otel { .. }` in crates/cli/src/commands.rs to a new `crate::otel::run_otel(repo, mission_scope, endpoint, from_start).await`.

3. Runtime loop (crates/cli/src/otel.rs or otel/run.rs): build the OTLP exporter once (from the emit adapter feature). On an interval (reuse a ~300-500ms poll like tail.rs/bridge.rs), for each in-scope mission: incrementally read new events, maintain a per-mission fold/cursor, and whenever a span's CLOSING event (worker.completed / milestone.completed / mission terminal) is seen, build that span via the mapping module and export it. For `--from-start`, on first sighting of a mission fold the whole log and export all already-closed spans (using event timestamps), then continue live. Be self-healing like the bridge (on a fold/read error, log at debug/warn and re-fold next tick; never wedge the loop). Run until Ctrl-C (tokio::signal::ctrl_c) — print a short startup line to stderr naming the endpoint and scope, like the other long-running commands.

Keep the span-construction logic in the pure mapping module; this feature is orchestration + I/O only. You may add a focused unit/async test for cursor advancement (first-sighting seeds at head in live mode; --from-start replays closed spans) but the end-to-end export is already covered by the otel_export integration test.

No engine edits. Ensure `cargo test -p kranz` and `cargo clippy --workspace --all-targets -- -D warnings` are clean.

Done when:
- `kranz otel --endpoint <url>` is a real subcommand (clap) with `--from-start`, honoring global --repo/--mission; `kranz otel --help` shows the flags.
- The loop polls mission dirs, folds incrementally (reusing EventLog + reducer read APIs), and exports each span when its closing event is observed; live mode seeds cursors at head, --from-start replays already-closed spans from event timestamps then follows.
- The command runs until Ctrl-C, is self-healing on read/fold errors (never panics the loop), and touches no file under crates/engine.
- cargo test -p kranz passes and cargo clippy --workspace --all-targets -- -D warnings is clean with the new command wired in.


## Milestone 3 — Documentation

### 3.1 docs/otel.md — endpoint config, id mapping, replay semantics

Write docs/otel.md documenting the OpenTelemetry exporter as actually implemented (read the finished otel module and CLI command to keep it accurate — the agent-judgement assertion checks the docs match the code).

Cover at minimum:
- Overview: `kranz otel` is an opt-in, read-only sidecar that tails mission event logs and exports spans; it makes zero changes to the engine/durability path.
- Endpoint configuration: the `--endpoint <URL>` flag, that it is an OTLP HTTP/protobuf traces endpoint (e.g. http://localhost:4318/v1/traces), the HTTP-not-gRPC choice, and `--repo`/`--mission` scoping. Include a runnable example (e.g. pointing at a local otel-collector).
- Id mapping: the exact derivation — trace id = first 16 bytes of sha256(missionId); span id = first 8 bytes of sha256("{missionId}:{openSeq}") where openSeq is the opening event's seq (mission.created / milestone.started / worker.spawned); why this makes re-runs idempotent for deduping backends. Document the span hierarchy (mission root -> milestone -> run) and parent resolution (milestoneId, featureId f-<m>-<f> -> ms-<m>, else root), the attribute names (kranz.mission.*, kranz.milestone.*, kranz.run.*, kranz.cost.usd, kranz.tokens.*), and the status mapping (Pass/Complete->Ok, Fail/Partial/Blocked/Failed/Abandoned->Error).
- Replay semantics: `--from-start` replays complete missions using event timestamps (never now()) then follows live; default seeds at head and only exports spans whose open and close both occur during the tail; spans export only when closed, so a still-running mission's root span is not exported until it terminates (re-run with --from-start after completion to emit it).

Keep it concise and correct. Do not modify code in this feature except, if needed, a doc link from README or docs/roadmap.md (optional). No engine edits.

Done when:
- docs/otel.md exists and documents endpoint configuration (the --endpoint flag, OTLP HTTP URL form, --repo/--mission scoping).
- docs/otel.md documents the deterministic id mapping (sha256-based trace/span id formulas and openSeq), the span hierarchy/parenting, attribute names, and status mapping.
- docs/otel.md documents replay semantics (--from-start uses event timestamps and follows live; default seeds at head; spans export only when closed).
- The documented formulas, attribute names, and semantics match the implemented code (accurate, not aspirational).

