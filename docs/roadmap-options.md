# Roadmap Options

This is the deterministic source for `/kranz roadmap`: strategic options that
are worth remembering but are not always immediate backlog tickets. Keep each
option under one of the category headings below.

Format:

```text
- **Title** - one-line summary.
  Why: why this option matters.
  Trigger: what makes it next.
  Source: docs/path.md; .kranz/tickets/slug.md
  Ticket: slug or none
```

## Now

- **Cursor CLI / Grok 4.5 backend implementation** - implement the completed probe's direct-parser route as a single-shot, validator-first `backend_cursor`.
  Why: Cursor now overlaps unattended agent work, but kranz's defensible layer is the mission/audit/consent harness; importing Cursor as a backend turns that pressure into model/runtime leverage.
  Trigger: The authenticated stream-json fixture and route decision are complete; build the parser/backend, live-soak validator use, then expose it in the backend picker.
  Source: docs/scoping/cursor-cli-backend.md; docs/scoping/cursor-probe-evidence/fixture-stream-json.jsonl
  Ticket: none

- **Local-inference trace flywheel** - record model identity + quant + validation outcome per completion and export a validation-passed, regenerable fine-tuning dataset — on frontier traces first.
  Why: It sharpens the audit/record pillar and yields a standalone dataset asset with zero local-inference dependency; the highest-leverage, lowest-risk, most mission-independent slice of the local-inference workstream.
  Trigger: Pick up now — it needs no GPU and gates nothing; do it before the local-executor tier so the executor eval and validator miss-rate have provenance to measure against.
  Source: docs/scoping/local-inference-executor-tier.md
  Ticket: local-inference-trace-provenance-flywheel

- **Roadmap surface v1** - keep `/kranz roadmap` backed by this tracked file so strategic options remain visible from Slack.
  Why: The backlog shows captured work, and `/kranz todo` shows immediate human actions; this file keeps conditional futures from collapsing into either.
  Trigger: Maintain whenever roadmap/scoping docs add or retire a strategic option.
  Source: docs/roadmap.md; docs/operator-gates.md; docs/roadmap-options.md
  Ticket: none

## Human-gated

- **Gas City Stage 1 live-city validation** - validate the existing pack against a disposable live City before building more integration polish.
  Why: The pack is stub/live-spike proven, but agent registration, supervisor behavior, and event-trigger reality still need one careful human-run pass.
  Trigger: A live `gc` city is available and the operator is willing to spend setup/teardown time plus any live mayor-session cost.
  Source: docs/gascity-citizenship.md
  Ticket: none

- **M6 live cloud deploy** - prove hosted kranz end-to-end after scoped push and deploy docs.
  Why: Cloud missions are scoped but still need a deliberately gated live run before claiming the milestone is operational.
  Trigger: Operator chooses a host and accepts the live deploy/network surface.
  Source: docs/roadmap.md; docs/deploy.md
  Ticket: none

## Scoping-ready

- **Repo knowledge store slices 2-3** - slice 1 (the `docs/knowledge/` vault + `research.md` artifacts) shipped 2026-07-08; next is slice 2 (ranked/capped knowledge injection into planning and M2 revision) then slice 3 (`kranz knowledge refresh` drift checks).
  Why: Slice 1 made the knowledge browsable and captured; slice 2 is where it starts paying off — the planner stops rediscovering the repo every draft.
  Trigger: Start when the next capability lane beats cleanup on leverage; slice 3 waits until enough notes exist to drift.
  Source: docs/scoping/repo-knowledge-store.md
  Ticket: none

- **Local-inference executor tier** - route bounded, well-specified execution-class work to a local OpenAI-compatible endpoint (backend_local, HTTP-in-engine), with deterministic tier routing and two-fail escalation to frontier; validator stays frontier until its local miss-rate is measured.
  Why: Kranz runs parallel batch workers continuously; local executors lift the per-token ceiling on how many run at once — but only past quota saturation. Per the workstream's own counter-evidence, a mixed setup loses to solo Opus while frontier calls are free at the margin.
  Trigger: A stretch of missions where frontier QUOTA — not local-model competence — is the binding constraint. Until then only the trace flywheel (in Now) is worth building. backend_local also reuses the shipped per-role backend machinery, so the harness entry cost is low.
  Source: docs/scoping/local-inference-executor-tier.md
  Ticket: local-inference-backend-local; local-inference-router-escalation; local-inference-validator-guarded; local-inference-cost-accounting

- **Gas City event dispatch and mission events** - after Stage 1, build the two stub-verifiable pack improvements.
  Why: Event dispatch lowers bead latency, and `kranz.mission.*` events make kranz a better City citizen without adopting beads as the core work model.
  Trigger: Stage 1 live-city validation has run at least once.
  Source: docs/gascity-citizenship.md
  Ticket: docs briefs 1 and 3

## Parked/demo

- **Even Realities demo feature** - keep as a high-delight demo lane, not the next strategic foundation.
  Why: It will be fun to show, but Gas City citizenship and ecosystem fit are higher-leverage right now.
  Trigger: Revisit when core reliability and ecosystem work are calmer.
  Source: docs/roadmap.md
  Ticket: none

## Out of kranz lane

- **Agentic IDE / desktop coding environment** - belongs to sgian, not kranz.
  Why: Kranz should stay a mission/audit/gate harness; terminal/editor/voice surfaces would blur the product boundary.
  Trigger: Track in sgian's roadmap, not here.
  Source: docs/roadmap.md; docs/scoping/repo-knowledge-store.md
  Ticket: none
