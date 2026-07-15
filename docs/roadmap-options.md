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

- **Agent hook status signals** - use Claude/Codex/Cursor lifecycle hooks as a
  non-authoritative observability lane for "running", "needs input",
  "interrupted", and "turn finished".
  Why: Mission Control's hook approach is the cleanest way to detect waiting
  states outside the model loop. Kranz should keep reducer/event outcomes
  authoritative, but hooks can make Slack/dashboard status much more truthful,
  especially for new CLI backends.
  Trigger: Build with Cursor/ChatGPT backend work, or sooner if Slack/dashboard
  "whose move is it?" inaccuracies recur.
  Source: AgentSystemLabs/mission-control docs/agent-status-detection.md
  Ticket: agent-hooks-status-signals

- **Structured human-question events** - represent agent questions and answers
  as replayable mission events rendered by dashboard and Slack.
  Why: Blocking work should converge to a precise human decision, not a prose
  blob buried in a transcript. Mission Control's AskUserQuestion contract is a
  good shape to borrow without copying PTY key injection.
  Trigger: Pick up with the next Slack/dashboard steering pass, or when a
  backend exposes structured questions.
  Source: AgentSystemLabs/mission-control AskUserQuestion surface
  Ticket: structured-human-question-events

- **Multi-repo project picker** - give M8 a home surface with pinned repos,
  groups, search, and activity counts across tickets and missions.
  Why: Mission Control's project grid solves the orientation problem that
  appears once one serve or one Slack bridge fronts multiple repos. Kranz needs
  repo selection and work-state overview, not terminal launching.
  Trigger: Start before one-serve-many-repos or one-Slack-bridge-many-repos
  ships.
  Source: docs/roadmap.md M8; AgentSystemLabs/mission-control README/SPEC
  Ticket: multi-repo-project-picker

- **Workspace and sandbox visibility** - show the effective execution
  workspace, provider, readiness, preview links, and takeover details as
  mission artifacts.
  Why: M6 needs a complete runnable workspace as the unit of execution.
  Mission Control's scope switcher reinforces the operator value of making
  local/worktree/remote context explicit.
  Trigger: Build alongside the next M6 workspace-provider slice or before the
  live cloud deploy.
  Source: docs/roadmap.md M6; docs/scoping/worker-sandboxing.md
  Ticket: workspace-sandbox-visibility

- **Repo knowledge store slices 2-3** - slice 1 (the `docs/knowledge/` vault + `research.md` artifacts) shipped 2026-07-08; next is slice 2 (ranked/capped knowledge injection into planning and M2 revision) then slice 3 (`kranz knowledge refresh` drift checks).
  Why: Slice 1 made the knowledge browsable and captured; slice 2 is where it starts paying off — the planner stops rediscovering the repo every draft.
  Trigger: Start when the next capability lane beats cleanup on leverage; slice 3 waits until enough notes exist to drift.
  Source: docs/scoping/repo-knowledge-store.md
  Ticket: repo-knowledge-ranked-brief-injection

- **Backend readiness and quota preflight** - before queue drain, show whether
  the selected backends can actually run: binary, auth, model, version, quota
  where available, sandbox compatibility, and model floor.
  Why: Mission Control's provider-usage panel is broader than kranz needs, but
  the readiness lesson is sharp: do not discover missing auth or quota only
  after a mission is claimed.
  Trigger: Pick up before unattended queue drain becomes routine across
  multiple backends, or with the Cursor/ChatGPT backend rollout.
  Source: AgentSystemLabs/mission-control docs/provider-usage.md
  Ticket: backend-readiness-quota-preflight

- **Post-complete PR handoff, no auto-push** - help an operator create a
  GitHub PR for a completed mission branch only when the branch already exists
  remotely; otherwise show the human-run push command.
  Why: Mission Control's PR affordance is worth adapting, but kranz's local
  invariant stands: it does not push. PR creation is review plumbing, not a
  delivery shortcut.
  Trigger: Build with pipeline merge/review polish or when team GitHub review
  becomes the common handoff for delivered missions.
  Source: AgentSystemLabs/mission-control CreatePullRequestButton pattern
  Ticket: post-complete-pr-handoff-no-push

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
