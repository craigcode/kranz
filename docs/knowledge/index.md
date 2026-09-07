---
title: Kranz knowledge vault — index
owner: mixed
freshness: live
last_verified: 2026-09-07
verified_against:
  - docs/scoping/repo-knowledge-store.md
  - AGENTS.md
  - docs/knowledge/decisions/skill-capture-boundary.md
  - .kranz/tickets/repo-knowledge-refresh-drift.md
---

# Kranz knowledge vault

The canonical, reviewed knowledge store for kranz: what the system is, how it is
run, how it is gated, and the invariants that must never break. Both humans and
agents read this. New here? Start with [CONVENTIONS.md](CONVENTIONS.md) for the
note format and freshness model, and [AGENTS.md](../../AGENTS.md) for the
day-to-day working rules.

## Map

### architecture/
How kranz is built.
- [Mission pipeline](architecture/mission-pipeline.md) — the lifecycle
  (draft → review → queue → drain → deliver → gated merge → land) and the
  event-sourced core (append-only log, single-writer, fold == state).

### validation/
The gates a mission must pass.
- [Validation and merge gates](validation/gates.md) — the full-workspace gate
  recipes, the anti-vacuity contract, the empty-deliverable safety net, the
  merge pre-gate, and secret scanning.
- [The two readiness axes](validation/readiness-axes.md) — the AMM projection
  (mapped, never adopted) and contract/consent health: why the second axis
  exists and the mapping rules.

### surfaces/
Operator surfaces.
- [Slack command surface](surfaces/slack-commands.md) — the `/kranz` commands,
  the read-only vs spend/mutation auth split, and the mrkdwn-escaping rule.

### decisions/
Load-bearing invariants and design history.
- [Inviolable invariants](decisions/inviolable-invariants.md) — never-push,
  pinned base_sha, append-only log, empty-deliverable, worktree isolation.
- [Positioning — governance and evidence layer](decisions/positioning-governance-evidence-layer.md)
  — kranz dispatches, gates, records, and proves; it does not write code.
- [Skill capture stays outside the harness](decisions/skill-capture-boundary.md)
  — wontfix; no in-harness skill proposal or install.

### operations/
How kranz is run — CLI, `kranz serve`, backends, config. _(To populate: no seed
note in slice 1; add as operational facts accumulate.)_

### Cross-cutting
- [lessons.md](lessons.md) — curated index into `.kranz/lessons/`.
- [glossary.md](glossary.md) — project vocabulary.

## How this vault is used

- **Now (slices 1–3):** browse and review. Committed Markdown, diffable in the
  same flow as plans and reports. A per-mission `research.md` evidence
  artifact is written beside `plan.md`. A capped, ranked "Knowledge from this
  repo" block is injected into planning and mid-mission revision — never
  every worker turn. Stale and unverified notes are excluded.
- **Slice 3 (CLI):** `kranz knowledge-refresh` reports notes whose
  `verified_against` paths are missing or have commits after `last_verified`,
  and fails closed when metadata, citations, or Git/filesystem probes cannot
  establish freshness. It does not rewrite notes. Dashboard/Slack surface is
  still later.
  Ticket: [`repo-knowledge-refresh-drift`](../../.kranz/tickets/repo-knowledge-refresh-drift.md).

Freshness legend: **live** = verified/invariant · **check-on-touch** = trusted
until a `verified_against` path changes · **stale** = browseable, not
auto-injected.
