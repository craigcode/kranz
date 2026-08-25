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

- **Roadmap surface v1** - keep `/kranz roadmap` backed by this tracked file so strategic options remain visible from Slack.
  Why: The backlog shows captured work, and `/kranz todo` shows immediate human actions; this file keeps conditional futures from collapsing into either.
  Trigger: Maintain whenever roadmap/scoping docs add or retire a strategic option.
  Source: docs/roadmap.md; docs/operator-gates.md; docs/roadmap-options.md
  Ticket: none

- **ChatGPT CLI backend (gpt-5.6-sol)** - probe, then a dispatch adapter reusing the Cursor parser/picker path.
  Why: Cursor's half of the old "absorb a CLI" line shipped. This is the remaining sibling, under the retained `AgentBackend` seam.
  Trigger: After a written probe (auth, print-mode events, cost/model). Do not implement from a remembered model id.
  Source: docs/scoping/cursor-cli-backend.md (pattern only)
  Ticket: chatgpt-cli-backend

- **Gas City pack → native queue (re-scope first)** - point `packaging/gascity/` at `.kranz/queue/` + `kranz work` instead of `KRANZ_SPOOL`.
  Why: D5 leftover. The native queue holds planned mission ids; the spool holds briefs for `kranz exec`. Not a drop-in.
  Trigger: After tomorrow's course, and after the ticket's written re-scope. Do not cut over on a demo night.
  Source: docs/gascity-citizenship.md D5
  Ticket: gascity-native-queue-swap

## Human-gated

- **Gas City Stage 1 live-city validation** - validate the existing pack against a disposable live City before building more integration polish.
  Why: The pack is stub-proven and demo-ready; supervisor+worker reality still needs one careful human-run pass.
  Trigger: A live `gc` city is available and the operator is willing to spend setup/teardown time plus any live mayor-session cost. Course runbook: docs/gascity-demo.md.
  Source: docs/gascity-citizenship.md
  Ticket: none

- **M6 live cloud deploy** - prove hosted kranz end-to-end after scoped push and deploy docs.
  Why: Cloud missions are scoped but still need a deliberately gated live run before claiming the milestone is operational.
  Trigger: Operator chooses a host and accepts the live deploy/network surface.
  Source: docs/roadmap.md; docs/deploy.md
  Ticket: m6-railway-live-deployment

## Later / gated

- **Gas City pack publish remainder** - human `gc pack registry` / `gc pack release` after Stage 1 receipt and a second consumer.
  Why: CI lint and publish metadata landed; publishing with no consumer only exports the Stage 1 gap.
  Trigger: A second operator wants to `gc pack fetch` the pack, and Stage 1 has a receipt.
  Source: docs/gascity-citizenship.md Stage 4
  Ticket: gascity-pack-publish

- **Gas City fleets** - `gc sling` / cross-machine only when a heterogeneous City fleet or a real `AgentBackend` remote runtime exists.
  Why: Speculative. In-harness heterogeneous dispatch already shipped and is not a City fleet.
  Trigger: A city actually routes kranz plus at least one other agent type, or a cross-machine runtime is offered.
  Source: docs/gascity-citizenship.md Stage 5
  Ticket: gascity-fleets

- **Local-inference executor tier** - route bounded execution-class work to a local OpenAI-compatible endpoint; validator stays frontier until miss-rate is measured.
  Why: The trace flywheel and backend_local slices shipped. Mixed local+frontier still loses to solo Opus while frontier calls are free at the margin.
  Trigger: A stretch of missions where frontier QUOTA — not local-model competence — is the binding constraint.
  Source: docs/scoping/local-inference-executor-tier.md
  Ticket: local-inference-backend-local (done); revisit only on the quota trigger

- **Repo knowledge store slice 3 UI** - dashboard/Slack stale-note surface after the CLI report exists.
  Why: Scoping put the operator surface in slice 3; the detector should land first.
  Trigger: After `repo-knowledge-refresh-drift` ships.
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

- **In-harness skill capture** - proposing or installing consumer skill files.
  Why: Frozen prompt-optimization. Lessons, knowledge, packs, Flight Rules, and corpus export already cover repeated knowledge as evidence.
  Trigger: A named consumer asks for a one-way export into a skill *format* with a human install step they own. Do not file that ticket until then.
  Source: docs/knowledge/decisions/skill-capture-boundary.md
  Ticket: m5-skill-capture-positioning-decision (done, wontfix)

## Retired from this file (shipped)

These were listed as Now / Scoping-ready / Later and have landed. Kept here
one cycle so `/kranz roadmap` readers are not surprised by the deletion.

- Headless Cursor (Grok 4.5) backend — `backend-cursor-direct-parser` and probe tickets done.
- Local-inference trace flywheel — `local-inference-trace-provenance-flywheel` done.
- Repo knowledge slice 2 — `repo-knowledge-ranked-brief-injection` done.
- Repo knowledge slice 3 CLI report — `repo-knowledge-refresh-drift` done.
- Post-complete PR handoff — `post-complete-pr-handoff-no-push` done.
- Backend readiness preflight — `backend-readiness-quota-preflight` done.
- Local workspace/sandbox visibility — `workspace-sandbox-visibility` done.
- M8 multi-root host design + project picker — both done.
- M7 Linux hostile live proof — real bubblewrap receipt committed; `m7-linux-hostile-live-proof` done.
- M7 container per-host egress — Docker internal-network boundary and native Ubuntu receipt committed; `m7-container-per-host-egress-boundary` done.
- M7 Windows production receipt — operator-controlled Windows 11 hostile and
  retained-overhead receipt committed; `m7-windows-containment-parity` done.
- Gas City event dispatch + `kranz.mission.*` emits — `revive-gascity-demo` done.
- Structured human-question events, agent hook status signals — both done.
- Workspace provider pin at approval — done.
