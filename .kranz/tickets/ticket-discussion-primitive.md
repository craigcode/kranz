---
title: "Ticket discussion primitive (D-BW-3, adopted from beads)"
priority: 3
schedule: once
---

# Ticket discussion primitive

Decision source: `docs/scoping/beads-workstore.md` D-BW-3 (accepted
2026-07-29): adopt beads' flat comment model natively — ticket as its own
ticket when needed. (The aspirational `thread_ts` capture stays a separate
Slack-side concern; do NOT build it here.)

## Problem

Tickets currently carry exactly two channels of meaning: the frontmatter
(structured) and the body (authored once, edited rarely). Everything else —
why a priority changed, what a review found, what an operator decided
mid-flight — lives in the event log (mission-scoped, not ticket-scoped) or
nowhere. Beads' flat `{author, text, created_at}` comment model is the
right-sized primitive: notes ON the ticket, visible to drafters, agents,
and reviewers.

## Design (locked)

1. **Storage:** `.kranz/tickets/<slug>.notes.jsonl` — one JSON object per
   line: `{ts, author, text}`. Additive-only, gitignored? NO — notes are
   part of the ticket's committed record (same class as the ticket .md
   itself; update AGENTS.md's tracked-vs-runtime list accordingly).
2. **CLI:** `kranz ticket note <slug> <text...>` appends (author =
   `KRANZ_NOTE_AUTHOR` env, else `operator`); `kranz ticket notes <slug>`
   prints chronologically. No edit/delete (append-only, mirroring the
   event log's honesty posture).
3. **Validation:** slug through the same `is_safe_id` path as ticket
   reads; note file created on first append; atomic append (open
   O_APPEND, single write, fsync — the event_log idiom).
4. **Surfacing:** `kranz draft <slug>` includes the notes in the drafter's
   context (they are precisely the "why" a plan needs); cap at the most
   recent 50 notes to bound prompt size.

## Test gate

- `cargo test --workspace ticket_note 2>&1 | grep -qE 'test result: ok. [1-9]'`
  covering: append creates the file, chronological print, author
  precedence (env > default), bad slug refused, no edit/delete path
  exists, draft context includes notes and caps at 50.
- Workspace gates green (bare exit codes, never piped).
