---
title: Skill capture stays outside the harness
owner: operator
freshness: check-on-touch
last_verified: 2026-09-22
verified_against:
  - docs/knowledge/decisions/positioning-governance-evidence-layer.md
  - docs/roadmap.md
  - crates/engine/src/knowledge.rs
  - crates/engine/src/lessons.rs
  - .kranz/tickets/m5-skill-capture-positioning-decision.md
  - .kranz/tickets/training-corpus-export.md
---

Rechecked 2026-09-22 (UTC) after the gate review packet shipped and its
roadmap checkbox was closed. The packet and baseline/candidate comparison
remain read-only evidence surfaces; neither changes the execution or skill
capture boundary.

## What this records

A positioning decision about M5's remaining "skill capture" bullet, made
2026-08-17. Work that contradicts it should be flagged against this ADR,
not reconciled quietly.

Rechecked 2026-09-16 (UTC) against the
[review/evidence follow-ups](../../roadmap.md#scheduled-follow-up--review-efficiency-and-evidence-2026-09-16)
and the positioning decision. Their reporting and measurement scopes neither
generate agent skills nor introduce automatic prompt optimization; the boundary
below is unchanged.

**Kranz does not propose, generate, or install agent skill files.**
Turning repeated mission lessons into Claude/Codex/Cursor skills is
prompt-optimization work. It is **wontfix** inside the harness.

## Decision

**wontfix** for in-harness skill capture.

The three options the ticket named:

| Option | Verdict |
|---|---|
| Build a skill-proposal loop in kranz | Rejected. Purpose is to make an agent write better code. Frozen. |
| Adapter-export of reviewed artifacts into a skill *format* | Not retained. No consumer has asked. Named as a revisit, not a contract. |
| Wontfix | **Accepted.** |

## Reasoning

The positioning ADR's freeze is the boundary, applied at the point of
temptation: new in-harness primitives whose purpose is to make an agent
write better code are not built. A skill file is exactly that primitive —
it is prompt material the consumer CLI loads to change how the next
session writes code.

Kranz already has these evidence homes for repeated knowledge. None of
them is a skill directory:

| Home | What it is |
|---|---|
| `.kranz/lessons/` | Append-only mission scars; byte-capped planning injection |
| `docs/knowledge/` | Reviewed, freshness-tagged standing facts |
| Packs (KRZ-313) | Private domain knowledge behind the pack contract |
| Flight Rules (M5.5) | SHOULD/MUST governance with pin, waiver, and evidence |
| `kranz export-corpus` | Provenance-tagged traces for *backend* fine-tune, not skills |

An additional channel that writes `.claude/skills`, Codex skill sinks, or
equivalent runtime dirs would be context-management and would cross the
ticket's own constraint (never write those dirs without a separate
explicit human approval). The honest form of that constraint is: do not
own the channel.

Human-approved export of an already-reviewed note into a skill *format*
would be an adapter, not a product. It is not scoped here. If a consumer
asks, the adapter must: stay one-way, carry source mission/note
provenance, redact secrets, and refuse to write the consumer runtime
directory — the human copies or a separate approved install step does.

## What this does not change

- Lessons, knowledge injection, packs, Flight Rules, and corpus export
  stay. They are evidence and governance, not skill capture. Being evidence is
  a claim kranz has to hold up: a lesson's provenance check now compares the
  working-tree bytes (`lessons::read_lesson_from_worktree`) against the blob in
  the commit it verified, so a lesson whose content was overwritten after the
  engine committed it is dropped rather than injected (the 2026-09-01
  adversarial audit, H14).
- Dispatch adapters (`AgentBackend`) stay. They translate; they do not
  author prompt libraries.
- Consumer CLIs may keep their own skills. Kranz does not manage them.

## Consequences

- M5's skill-capture bullet is decided. The milestone no longer names
  unowned implementation work.
- Do not file a skill-export implementation ticket until a named
  consumer asks.
- A change that "just writes a skill file from a lesson" is a freeze
  violation, not a small feature.

## Revisit triggers

- A real consumer asks for a one-way, provenance-preserving export into
  a skill *format*, with a human install step they already own.
- A dispatch target's hook/permission surface becomes too weak to
  project gates onto (the positioning ADR's existing revisit) — that
  still does not license skill generation; it would force a different
  adapter conversation.
