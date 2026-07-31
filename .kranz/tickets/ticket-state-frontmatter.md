---
title: "Committed ticket state: notes must fold into ticket status, or .status sidecars must commit"
priority: 2
schedule: once
---

# Committed ticket state

Found by the 6th-pass review: the `.kranz/tickets/*.status` sidecars are
gitignored runtime. On a fresh clone they vanish, so a ticket marked done
(or superseded) becomes NEW again — the state never actually persisted
anywhere durable. The notes primitive IS committed, but nothing reads
notes back into the ticket state machine.

## Problem

Ticket lifecycle state has no committed home:
- `.kranz/tickets/<slug>.status` (state + note) is gitignored — ephemeral
  per clone, so done/superseded is lost on every fresh checkout.
- `<slug>.notes.jsonl` IS committed, but the ready/list/queue evaluation
  does not fold it — a "SUPERSEDED" note is documentation, not state.

So any operator state beyond the frontmatter evaporates across clones,
and (observed 2026-07-31) superseded-by-note tickets would silently
re-enter the ready path on a fresh machine.

## Design (locked)

1. **State in the frontmatter**: an optional additive `state:` key in the
   ticket .md itself (`open` default; `done`, `superseded`, `wontfix`
   recognized, with an optional `state-note`). The ready/queue evaluation
   treats `done`/`superseded`/`wontfix` as terminal (excluded, like
   terminal pipeline states). This is the single source of truth — it
   survives clones by construction.
2. **Sidecar demoted to cache**: `.status` files become a write-through
   cache of the frontmatter state (engine writes both on state change;
   frontmatter wins on conflict — never a silent divergence; a mismatch
   logs and uses the frontmatter).
3. **Notes stay discussion**: `<slug>.notes.jsonl` remains the
   append-only discussion channel — it does NOT become a second state
   channel (no parsing of note text for state; that way lies grep-driven
   state machines).
4. **Migration**: a one-time fold of existing `.status` sidecars into
   frontmatter `state:` (engine command or first-read merge, documented).

## Test gate

- `cargo test --workspace ticket_state_frontmatter 2>&1 | grep -qE 'test result: ok. [1-9]'`
  — frontmatter `state: superseded` excludes from ready/queue; absent key
  defaults open; a `.status`/frontmatter conflict resolves to the
  frontmatter with a logged note; migration folds an existing sidecar.
- Workspace gates green, bare exit codes, never piped.
