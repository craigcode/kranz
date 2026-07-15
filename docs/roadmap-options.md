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

- **Headless CLI backends: Cursor (Grok 4.5) + ChatGPT CLI (gpt-5.6-sol)** - implement the completed probe's direct-parser route as a single-shot, validator-first `backend_cursor`, and build a sibling ChatGPT-CLI backend for gpt-5.6-sol in the same pass.
  Why: Cursor now overlaps unattended agent work, but kranz's defensible layer is the mission/audit/consent harness; importing headless coding CLIs as backends turns that pressure into model/runtime leverage. Cursor and the ChatGPT CLI are the same "absorb a CLI as a backend" pattern, so build them together and share the probe → parser → picker path.
  Trigger: Cursor's authenticated stream-json fixture and route decision are complete; build the parser/backend, live-soak validator use, then expose it in the backend picker. The ChatGPT-CLI/gpt-5.6-sol backend needs its own probe first (auth, print-mode event structure, model/cost capture) — verify those, then reuse the Cursor parser scaffold. `gpt-5.6-sol` is Craig's stated target model; confirm the exact CLI + model id at probe time (post-cutoff, not vouched here).
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

- **Repo knowledge store slice 2** - ranked/capped knowledge injection into planning and M2 revision (4 KiB budget, stale excluded, separate from lessons).
  Why: Slice 1 made the vault browsable; slice 2 is where planning stops rediscovering the repo every draft.
  Trigger: Next capability lane when leverage beats cleanup.
  Source: docs/scoping/repo-knowledge-store.md D-C
  Ticket: repo-knowledge-ranked-brief-injection

- **Post-complete PR handoff, no auto-push** - `gh pr create` only when the mission branch already exists remotely; otherwise show the human push command.
  Why: Review plumbing without violating "kranz never pushes" locally.
  Trigger: Pipeline merge/review polish or team GitHub review becomes common.
  Source: AgentSystemLabs/mission-control CreatePullRequestButton pattern
  Ticket: post-complete-pr-handoff-no-push

- **Backend readiness preflight before drain** - probe binary/auth/model/sandbox; park on hard failures; warn+proceed on unknown quota.
  Why: Do not discover missing auth only after claiming a mission.
  Trigger: Unattended multi-backend drain, or Cursor/ChatGPT backend rollout.
  Source: existing auth/env preflight; AgentSystemLabs/mission-control provider-usage
  Ticket: backend-readiness-quota-preflight

- **Local workspace/sandbox visibility** - show isolation mode, worktree cwd, and sandbox tier in dashboard/report (no remote provider inventing).
  Why: Operators should not infer the runtime environment from branch names.
  Trigger: Anytime; unblocks clearer M6 UI later.
  Source: docs/scoping/worker-sandboxing.md
  Ticket: workspace-sandbox-visibility

- **M8 multi-root host design** - config, tokens, queues, Slack routing for one serve / many repos — design only.
  Why: The project picker cannot honestly scope tokens until the host model exists.
  Trigger: Before one-serve-many-repos or one-Slack-bridge-many-repos UI.
  Source: docs/roadmap.md M8
  Ticket: m8-multi-root-host-design

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

## Later / gated

- **Structured human-question events** - replayable ask/answer events feeding one pending-decision projection; grants/NeedsContext stay their own channels.
  Why: Precise "whose move" without a third inbox.
  Trigger: Next Slack/dashboard steering pass.
  Source: AgentSystemLabs/mission-control AskUserQuestion surface
  Ticket: structured-human-question-events

- **Agent hook status signals** - non-authoritative ephemeral projection from CLI lifecycle hooks; no primary-checkout hook rewrites.
  Why: Waiting-state sensing for interactive CLI backends.
  Trigger: With Cursor/ChatGPT backend work (blocked-by route decision).
  Source: AgentSystemLabs/mission-control docs/agent-status-detection.md
  Ticket: agent-hooks-status-signals

- **Multi-repo project picker** - pins/groups/search/activity counts across configured repos.
  Why: M8 orientation layer once the host can front many roots.
  Trigger: After m8-multi-root-host-design is accepted.
  Source: docs/roadmap.md M8; AgentSystemLabs/mission-control README/SPEC
  Ticket: multi-repo-project-picker

- **Workspace provider pin at approval (M6)** - pin provider/template/image at approve; surface readiness/previews/takeover.
  Why: Cloud workspace is the unit M6 provisions; local visibility came first.
  Trigger: When a real workspace provider implementation exists.
  Source: docs/roadmap.md M6; docs/scoping/worker-sandboxing.md Tier 3
  Ticket: workspace-provider-pin-at-approval

- **Repo knowledge store slice 3** - `kranz knowledge refresh` drift checks.
  Why: Freshness enforcement once enough notes exist to drift.
  Trigger: After slice 2 and a non-trivial vault.
  Source: docs/scoping/repo-knowledge-store.md
  Ticket: none yet

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
