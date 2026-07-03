# Design: mission backlog (tickets) + Slack bridge

Status: design. Target milestone: M2.75 (after M1/M2; multiplies with M6).

## The idea

Kranz today is synchronous: you sit down, converse, approve, watch. The
backlog turns it into scheduled work: **tickets** are missions-in-waiting
with prepared context; plans are **drafted asynchronously** and parked for
review as `plan.md`; approved missions enter a **per-repo execution queue**;
Slack carries notifications and approvals to wherever the humans are.

```
ticket.md ──draft──▶ plan.md ready ──review/approve──▶ queued ──▶ running ──▶ report.md
   ▲                     │  ▲                                        │
   └── "needs context" ◀─┘  └──── Slack: review link / approve ──────┴─▶ Slack: blocked / complete
```

## Tickets are markdown, in the repo

`.kranz/tickets/<slug>.md` — arrives by PR, discussed in code review,
merged = triaged. Frontmatter + body:

```markdown
---
title: Rate-limit the notes API
priority: 2            # 1 high … 3 low
repo-refs: [src/api/, tokenstore.py]
schedule: once         # once | nightly | weekly (recurring re-instantiates)
maxBudgetUsd: 15
---

## Goal
<one paragraph — becomes the mission goal>

## Context
<why now; links; prior art; the ticket author's knowledge dump>

## Scoping answers   ← pre-answers the questions orchestrators always ask
- Test command: python3 -m unittest discover
- Conventions: stdlib only, no new deps
- Out of scope: auth changes, storage format

## Acceptance hints
- requests beyond N/min per token get 429
- existing endpoints unaffected (all current tests pass)
```

## Pipeline stages (mapping to existing machinery)

1. **Draft** — a dispatcher (`kranz drift`… no: `kranz draft <ticket>` or the
   serve host on a timer) creates the mission and runs the planning
   conversation NON-interactively: seed + ticket body as the first message,
   then `request_plan`. `Ready` → plan.json/plan.md/index.md committed as
   usual, mission parked in `review` (= today's Planning-with-proposed-plan);
   `NotReady` → ticket flagged **needs-context** with the orchestrator's
   verbatim questions appended to the ticket file (and Slack-pinged). Nothing
   new in the engine — this is `planning_turn`/`request_plan` driven by a
   scheduler instead of a keyboard. Draft cost is bounded (orchestrator
   budget cap applies; no workers spawn).
2. **Review** — a human reads plan.md (git, web UI, or the Slack message
   rendering its summary) and approves via the M2.5 endpoint, CLI, or a
   Slack button. Edits = reply with guidance (a planning turn) → re-draft.
3. **Queue** — approval enqueues rather than starts: `.kranz/queue/` ordered
   by priority then age. **Per-repo serialization is mandatory** (missions
   share the working tree; branches are per-mission but checkouts are not).
   The serve host runs the queue: one mission at a time per repo, N repos in
   parallel if hosting several.
4. **Run / block / complete** — existing lifecycle. Blocked is the key Slack
   moment (§4.5: convergence to a human): the message carries the reason and
   accepts a threaded reply as `kranz msg` guidance. Completion posts the
   report.md summary + branch link.

## Slack bridge

A `kranz-slack` bridge (crate or `kranz serve --slack`) using **Socket
Mode** — outbound websocket, no public URL, no change to the localhost
trust model. Bot token + app token in `~/.kranz/config.json` (never in the
repo).

Outbound (v1, highest value):
- plan ready for review → summary + `plan.md` excerpt + [approve] [ask] buttons
- ticket needs-context → the orchestrator's questions, threaded
- mission blocked → reason + "reply here to unblock"
- mission complete/failed → report summary, cost, branch name
- guardrail denials & waivers → optional noisy channel

Inbound (v1.5):
- [approve & queue] button → M2.5 approve endpoint + queue insert
- threaded replies on a mission's thread → control-inbox `msg`
- `/kranz ticket <title>` slash command → opens a ticket file PR from the
  thread's content (the thread IS the context dump)

One thread per mission keeps the mapping trivial (thread_ts ↔ mission id in
the mission dir).

## What's genuinely new vs reused

New: ticket schema + parser, non-interactive draft driver, queue +
scheduler (incl. recurring tickets), Slack bridge, `review`/`queued`
surfacing in status/dashboard/picker.
Reused untouched: planning engine (incl. NotReady), approval + plan.md +
index.md, M2.5 host/endpoints/token, control inbox, event feed, report.md.

## Done when

A ticket merged to `.kranz/tickets/` gets a drafted plan and a Slack review
message without anyone running a command; approving from Slack queues it;
the mission runs when the repo is free; a blocked milestone gets unblocked
entirely from a Slack thread; an underspecified ticket bounces back with
the orchestrator's specific questions; two approved missions on one repo
never run concurrently.
