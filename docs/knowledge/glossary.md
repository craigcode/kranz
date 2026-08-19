---
title: Glossary
owner: mixed
freshness: check-on-touch
last_verified: 2026-08-18
verified_against:
  - crates/engine/src/types.rs
  - crates/engine/src/orchestrator.rs
  - AGENTS.md
---

# Glossary

Project vocabulary. Terms link to the note that explains them in depth.

- **Mission** — one unit of work driven through the
  [pipeline](architecture/mission-pipeline.md): draft → review plan → queue →
  run → deliver → gated merge → land. Identified `m-<hex>`.
- **Ticket** — a backlog item at `.kranz/tickets/<slug>.md`. Its additive
  `state:` frontmatter is lifecycle authority; the gitignored `.status`
  sidecar is a write-through compatibility cache. The ticket is planning input
  for a mission.
- **Plan** — the approved unit of intent: `plan.json` (durable, structured) plus
  a `plan.md` twin, committed on the mission branch. Contains milestones,
  features, and the validation contract.
- **Milestone / Feature** — plan structure. A milestone groups features; a
  worker implements one feature at a time.
- **Validation contract** — per-plan assertions the mission must satisfy. Each
  is a `Command` (a build/test/lint invocation) or an `AgentJudgement`
  (re-evaluated by a validator/orchestrator). See
  [gates](validation/gates.md).
- **Orchestrator** — the planning-and-judging agent role: drafts plans, proposes
  revisions, and renders final-gate verdicts. Runs as one streaming session.
- **Worker** — the coding agent role; one run per feature, in its own git
  worktree ([worktree isolation](decisions/inviolable-invariants.md)).
- **Validator** — the checking roles: **scrutiny** (adversarial review) and
  **functional** (runs the contract's command gates). Configurable floors apply
  to autonomous runs.
- **base_sha** — the base branch tip pinned at approval. All contract/final-gate
  diffs are taken against it; it is never re-resolved later.
- **Event log** — `events.jsonl`, the append-only single-writer source of truth.
  `fold(events)` == `MissionState`; `state.json` is only a cache.
- **Deliver vs Land** — a mission is **Delivered** when it Completes but its
  branch is unmerged, and **Landed** once merged.
- **Drain** — running the per-repo execution queue (`kranz work`); serialized
  per repo.
- **Gate** — a deterministic pass/fail check: the full-workspace
  test/clippy/fmt suite, the merge pre-gate, the empty-deliverable net, and the
  secret scan. See [gates](validation/gates.md).
- **Empty-deliverable net** — a mission with zero non-meta feature commits vs
  its pinned `base_sha` FAILs rather than falsely Completing.
- **Anti-vacuity** — the contract rule that a filtered test run matching zero
  tests is a vacuous pass, not a real one
  (`grep -qE 'test result: ok\. [1-9]'`).
- **Lessons** — append-only cross-mission memory in `.kranz/lessons/`, injected
  tightly-capped into planning. See [lessons.md](lessons.md).
- **Backend** — the dispatch target a role runs on: `claude`, `codex`, `droid`,
  `kimi`, `cursor`, an OpenAI-compatible `local` endpoint, or a worker-only
  `acp` agent executable. Selection and sandbox support are validated per role.
- **`kranz serve`** — the REST/WS mission host. It defaults to loopback and can
  compose an operator-configured catalog of repositories for the dashboard and
  the single Slack bridge.
- **Respawn / fix-cycle / fix-feature** — a respawn re-runs a failed worker; a
  fix-cycle is a validation round that produced findings; a fix-feature is a
  feature created to resolve a finding.
