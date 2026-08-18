# Knowledge vault conventions

This directory (`docs/knowledge/`) is kranz's **canonical repo knowledge
store**: reviewed, committed, source-linked Markdown that both humans and agents
read. It is the durable layer above `.kranz/lessons/` (append-only mission
scars) and `.kranz/tickets/` (the backlog). Design of record:
[docs/scoping/repo-knowledge-store.md](../scoping/repo-knowledge-store.md).

The rule that makes it trustworthy: **a stale or hallucinated repo fact is worse
than no note** — it can steer planning, validation, and workers toward obsolete
behavior. So every note carries provenance and freshness, and every factual
claim must be grounded in the code as it exists now.

## Note format

Every note is a Markdown file beginning with a YAML front-matter block:

```yaml
---
title: Short title
owner: human | agent | mixed
freshness: live | check-on-touch | stale
last_verified: 2026-07-08
verified_against:
  - crates/engine/src/orchestrator.rs
  - cargo test -p kranz-engine lessons
---
```

- **`owner`** — who maintains it. `agent` notes were generated and are expected
  to be regenerated; `human` notes are hand-authored intent; `mixed` is both.
- **`freshness`**
  - `live` — verified in the last knowledge refresh, or tied to a stable
    invariant (e.g. "kranz never pushes").
  - `check-on-touch` — trusted until one of its `verified_against` paths
    changes; the default for notes describing code.
  - `stale` — still browseable, but **excluded from automatic prompt injection**
    (slice 2) unless explicitly requested.
- **`last_verified`** — the date the claims were last checked against the code.
- **`verified_against`** — the real repo paths and/or commands the note's claims
  depend on. These drive the drift checks in slice 3, so every entry must exist.

## Body

- Use ordinary Markdown links for cross-references (e.g.
  `[gates](../validation/gates.md)`). Wikilinks are allowed only as an optional
  duplicate alias, never as the sole link — they do not travel across tools.
- Keep notes concise and high-signal: what a planner or agent needs to not
  rediscover the subsystem from scratch. Prefer a short accurate note over a
  long speculative one.
- Ground claims. If a fact cannot be confirmed against the code, leave it out.

## Categories

| Directory | Holds |
|---|---|
| `architecture/` | how the system is built — the pipeline, the event-sourced core, module maps |
| `operations/` | how it is run — CLI, `kranz serve`, backends, config |
| `validation/` | the gates, safety nets, and validation contract recipes |
| `surfaces/` | operator surfaces — Slack, dashboard, REST |
| `decisions/` | load-bearing invariants and design history ("lore") |
| `lessons.md` | curated index into `.kranz/lessons/` (see below) |
| `glossary.md` | project vocabulary |

## What this is *not* (yet)

Slice 1 established the vault and its conventions, plus a per-mission
`research.md` evidence artifact beside `plan.md`. Slice 2 injects a ranked,
≤4 KiB "Knowledge from this repo" block into **planning and revised-planning**
seeds only (stale/unverified notes excluded; separate from the lessons budget).
Slice 3 (`kranz knowledge refresh` drift checks) is ticketed as
[`repo-knowledge-refresh-drift`](../../.kranz/tickets/repo-knowledge-refresh-drift.md).
See the scoping doc's build slicing.

## Relationship to lessons

`.kranz/lessons/` stays the append-only, tightly-capped mission-capture lane —
high-signal operator scars. This vault **summarizes and links** to lessons via
[lessons.md](lessons.md); it does not replace or absorb them.
