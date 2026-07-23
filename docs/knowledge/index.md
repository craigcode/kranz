---
title: Kranz knowledge vault — index
owner: mixed
freshness: live
last_verified: 2026-07-08
verified_against:
  - docs/scoping/repo-knowledge-store.md
  - AGENTS.md
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

### operations/
How kranz is run — CLI, `kranz serve`, backends, config. _(To populate: no seed
note in slice 1; add as operational facts accumulate.)_

### Cross-cutting
- [lessons.md](lessons.md) — curated index into `.kranz/lessons/`.
- [glossary.md](glossary.md) — project vocabulary.

## How this vault is used

- **Now (slice 1):** browse and review. Committed Markdown, diffable in the same
  flow as plans and reports. A per-mission `research.md` evidence artifact is
  written beside `plan.md`.
- **Next (slice 2):** a capped, ranked "Knowledge from this repo" block is
  injected into planning and mid-mission revision — never every worker turn.
- **Later (slice 3):** `kranz knowledge refresh` runs each note's drift checks,
  flags notes whose `verified_against` paths changed, and proposes edits.

Freshness legend: **live** = verified/invariant · **check-on-touch** = trusted
until a `verified_against` path changes · **stale** = browseable, not
auto-injected.
