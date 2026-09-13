# `kranz otel` — OpenTelemetry export sidecar

`kranz otel` is an opt-in, read-only sidecar that tails a repo's mission
event logs and exports OpenTelemetry spans (one trace per mission, with
milestone and run child spans) to an OTLP endpoint. It is purely read-side:
it consumes `kranz_engine::event_log`/`paths` read APIs only and makes zero
changes to the engine, the durability path, or `events.jsonl`. It runs until
Ctrl-C.

## Running it

```
kranz otel --endpoint <URL> [--from-start] [--repo <PATH>] [--mission <ID>]
```

- `--endpoint <URL>` (required) — an OTLP **HTTP/protobuf** traces endpoint,
  e.g. `http://localhost:4318/v1/traces`. HTTP/protobuf was chosen over gRPC
  so the exporter has no gRPC/TLS/codegen dependency — any collector with the
  standard OTLP HTTP receiver (port 4318) works out of the box.
- `--repo <PATH>` — global flag shared with the rest of the CLI; target
  repository root (defaults to the current directory).
- `--mission <ID>` — global flag shared with the rest of the CLI; scopes
  export to a single mission. Without it, `kranz otel` discovers and tails
  every mission in the repo.
- `--from-start` — see [Replay semantics](#replay-semantics) below.

Example against a local `otel-collector` (default OTLP HTTP receiver on
`4318`, logging spans to its own stdout):

```
docker run -p 4318:4318 -p 4317:4317 \
  otel/opentelemetry-collector:latest

kranz otel --endpoint http://localhost:4318/v1/traces --from-start
```

## Id mapping

Ids are derived deterministically from mission/event data, never generated
randomly, so re-running `kranz otel --from-start` after a mission finishes
re-emits byte-identical trace and span ids — backends that dedupe by id
converge instead of accumulating duplicate spans per run.

- **Trace id** = first 16 bytes of `sha256(mission_id)`.
- **Span id** = first 8 bytes of `sha256("{mission_id}:{open_seq}")`, where
  `open_seq` is the `seq` of the event that *opens* the span:
  `mission.created` for the mission root, `milestone.started` for a
  milestone span, `worker.spawned` for a run span.

A span is only built, and only exported, once its closing event has also
been observed (`milestone.completed`/`milestone.blocked` for a milestone,
`worker.completed` for a run, `mission.completed`/`mission.failed`/
`mission.abandoned` for the root) — mirroring OTel's export-on-span-end
model.

### Span hierarchy and parenting

```
mission <mission_id>              (root span)
├── milestone <milestone_id>: ... (child of root)
│   └── <role> <run_id>           (child of its milestone)
└── <role> <run_id>               (child of root, when unscoped to a milestone)
```

A run span's parent is resolved in this order:
1. If the closing `worker.completed` event's run has a `milestone_id`, parent
   to that milestone's span id.
2. Else if it has a `feature_id` of the form `f-<m>-<f>`, parent to milestone
   `ms-<m>` (derived by stripping the `f-` prefix and taking the first
   `-`-separated segment).
3. Else parent to the mission root span.

If none of the above resolves (e.g. a live tail that started mid-mission and
never observed `mission.created`), the span is exported with no parent
rather than being dropped or panicking.

### Attributes

| Attribute | Present on | Meaning |
|---|---|---|
| `kranz.mission.id` | root | Mission id |
| `kranz.mission.goal` | root | Mission goal string |
| `kranz.mission.status` | root | `complete` \| `failed` \| `abandoned` |
| `kranz.milestone.id` | milestone, run (only when the run was spawned with an explicit milestone_id — e.g. validator runs) | Milestone id (`ms-<n>`) |
| `kranz.milestone.title` | milestone | Milestone title |
| `kranz.milestone.status` | milestone | `complete` \| `blocked` |
| `kranz.milestone.fix_cycles` | milestone | Count of fix-feature cycles entered while validating |
| `kranz.run.id` | run | Run id |
| `kranz.role` | run | `orchestrator` \| `worker` \| `validator-scrutiny` \| `validator-functional` |
| `kranz.model` | run | Model name used for the run |
| `kranz.run.result` | run | `pass` \| `fail` \| `partial` |
| `kranz.feature.id` | run (if set) | Feature id (`f-<m>-<f>`) |
| `kranz.cost.usd` | root, run (if cost known) | USD cost |
| `kranz.tokens.input` / `.output` / `.cache_read` / `.cache_write` | root, run | Token usage |

The root span's `kranz.cost.usd` and token attributes are totals summed
across every `worker.completed` event folded for that mission.

### Status mapping

| Source | OTel span status |
|---|---|
| Run result `Pass` | `Ok` |
| Run result `Fail` | `Error` (description `"fail"`) |
| Run result `Partial` | `Error` (description `"partial"`) |
| Milestone `milestone.completed` | `Ok` |
| Milestone `milestone.blocked` | `Error` (description = block reason) |
| Mission `mission.completed` | `Ok` |
| Mission `mission.failed` | `Error` (description = failure reason) |
| Mission `mission.abandoned` | `Error` (description = abandon reason) |

(`Blocked`/`Failed`/`Abandoned` here are the mission/milestone terminal
states, not `RunResult` variants — `RunResult` only has `Pass`/`Fail`/
`Partial`.)

## Replay semantics

- **`--from-start`**: on first sighting of a mission, replays its entire
  event log, builds every span whose open *and* close events are both
  already in the log, and exports them — using the events' own `ts` fields
  for span start/end, never wall-clock `now()`. It then continues tailing
  new events live. Use this to backfill a finished (or partially finished)
  mission's history into a collector, or to re-run against a mission that
  completed after a previous `kranz otel` session exited.
- **Default (no `--from-start`)**: on first sighting of a mission, the
  cursor is seeded at the log's current head with no replay — history
  before that point is not exported. From then on, only spans whose
  **opening and closing events both occur during the tail** are built and
  exported. Consequently, a span whose opening event predates the tail
  (e.g. the mission root, if `mission.created` was logged before `kranz
  otel` started) is never exported in this mode, even once its closing event
  arrives.
- In both modes, spans are exported exactly once (a per-mission in-memory
  set of already-exported span ids is checked on every poll tick), and a
  still-running mission's root span is never exported until the mission
  reaches a terminal event — re-run with `--from-start` after the mission
  finishes to emit it.
