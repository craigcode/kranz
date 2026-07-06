# Tickets — using the backlog

How to capture work as tickets, turn them into drafted plans, and execute
them on demand. This is the usage companion to the design doc
(docs/backlog-and-slack.md §tickets); commands verified against the CLI.

## Where tickets live

One ticket = one plain markdown file in the repo at `.kranz/tickets/<slug>.md`:

```markdown
---
title: Persist the serve mutation token to a 0600 file
priority: 2
schedule: once
---

## Goal
One self-sufficient paragraph: the outcome, stated imperatively.

## Context
What a fresh worker needs: file paths, incidents, constraints, prior lessons.

## Scoping answers
Left empty at creation. If a draft comes back NEEDS-CONTEXT, the
orchestrator's questions are appended to the ticket — answer them here
and re-draft.

## Acceptance hints
Concrete testable outcomes. Any named test uses a passed-count guard —
`grep -qE 'result: ok\. [1-9][0-9]* passed'` — never a bare test-name
filter (a zero-match filter exits 0).
```

Tickets are ordinary versioned files: they diff, merge, and review like
code. Pipeline state (NEW → REVIEW → QUEUED → done) lives in a sidecar
state file, not the markdown — editing a ticket never corrupts its state.
Slugs are validated (ascii alphanumerics, `-`, `_`, `.`; no traversal).

A ticket can also declare `blocked-by: [slug, ...]` in its frontmatter to
depend on other tickets — see §Dependencies (`blocked-by`) below.

## Listing the backlog

```sh
kranz ticket list          # slug, priority, state, title
kranz ticket show <slug>   # one ticket in full, incl. needs-context questions
kranz queue                # the execution queue (approved, awaiting a run)
```

Slack: `/kranz work` reports the queue read-only; the App Home tab shows
the overview. The backlog is also browsable, draftable, and approvable
from the dashboard's backlog panel (`#/backlog`, linked from the mission
picker) — it lists the same slug/priority/state/title/blocked-by columns,
drills into a ticket for its goal/context/needs-context, and drives
draft/approve with the same blocked-by-aware gate described below.

## The pipeline: draft → review → approve → drain

```sh
kranz ticket new my-fix --title "..." --goal "..."   # scaffold (refuses overwrite)
kranz draft my-fix        # orchestrator drafts a plan, parks plan.md for review
kranz ticket approve my-fix   # commit approval, enqueue, mark QUEUED
kranz work                # drain the queue: one mission at a time, crash-safe
```

What each step really does:

- **`kranz draft <slug>`** is non-interactive: the orchestrator is seeded
  with the whole ticket, produces a plan, and parks a committed `plan.md`
  for review — no execution, no repo changes. An underspecified ticket
  comes back **NEEDS-CONTEXT** with the orchestrator's actual questions
  appended to the ticket file; answer under `## Scoping answers` and
  re-draft. Spend is bounded by the orchestrator budget cap.
- **`kranz draft <slug> --yes`** collapses draft → approve → queue into
  one command. Only for tickets you trust completely: you are skipping the
  one human gate between a ticket and paid execution, and estimates for
  unusual mission shapes can miss badly (m-d341a7: $163.64 actual vs
  $18.35 expected — a doc-heavy shape the calibration corpus didn't model).
- **`kranz ticket approve <slug>`** requires the REVIEW state (a parked
  drafted plan) and moves it to QUEUED with a queue entry.
- **`kranz work`** drains the per-repo queue serially — missions own the
  working tree, so one at a time. `--once` processes exactly one front
  entry and exits (exit 0 if the repo is busy). Crash recovery is
  automatic: claims are atomic file renames, and dead dispatchers' claims
  are recovered on the next run.

## Dependencies (`blocked-by`)

A ticket can name other tickets it depends on:

```markdown
---
title: Wire the new client into the dashboard
priority: 2
blocked-by: [add-client-lib, add-client-tests]
---
```

A blocker is **satisfied only when its mission reaches `Complete`** —
approved, queued, running, or failed all still count as blocked. There is
no partial credit: satisfaction is authoritative on mission status alone,
never on ticket/queue pipeline state.

- **`kranz ticket approve <slug>`** refuses an approval with an unsatisfied
  blocker, naming it honestly:

  ```
  cannot approve <slug>: blocked by <blocker>, <blocker> (its mission is not Complete)
  ```

- **`kranz ticket approve <slug> --force`** overrides unsatisfied blockers
  (approve anyway, at your own risk) — but never a cycle (below); a cycle
  is refused unconditionally.
- **Cycle detection** runs at approve time regardless of `--force`: the
  `blocked-by` graph is walked from `slug`, and a reachable cycle refuses
  the approval with the path spelled out:

  ```
  blocked-by cycle: a -> b -> a
  ```

- **`kranz work` re-checks blockers at work time**, not just at approve
  time: batch-approving a night's worth of tickets can queue a dependent
  right alongside a blocker that later fails mid-drain. When the
  dispatcher is about to run a queued entry and finds a `blocked-by` ticket
  that reached `Failed`, it skips the entry — retiring its claim, marking
  the ticket `Failed` with a note naming the blocker, and printing:

  ```
  warning: skipping ticket '<slug>' — blocked-by '<blocker>' failed
  ```

  This is a one-shot skip, not a retry loop — re-driving a mission whose
  dependency failed can never succeed on its own.

The CLI and the REST `POST /api/tickets/:slug/approve` route share this
exact gate (`kranz_engine::deps::approve_ticket`), so the two surfaces can
never drift on what "approvable" means.

## The backlog over REST

`kranz serve` exposes the same backlog as a REST surface, as an
alternative to the CLI pipeline above:

```
GET  /api/tickets              # list: slug, priority, state, title, blockedBy
GET  /api/tickets/:slug        # full ticket + needsContext
POST /api/tickets/:slug/draft  # kick off a draft (202, long-running)
POST /api/tickets/:slug/approve  # {"force": bool} -> approve into the queue
```

`draft` is asynchronous: it returns `202 {"missionId":"m-…"}` immediately
and the draft turns run as a background task. Watch progress over that
mission's `GET /api/missions/:id/ws` feed, and poll `GET /api/tickets/:slug`
for the terminal outcome (Review vs NEEDS-CONTEXT). Both `POST` routes
require the mutation token like every other `POST` (docs/protocol.md
§Authority); `approve` returns the same `409` shapes described above for
an unsatisfied blocker or a cycle. See docs/protocol.md §Tickets for the
full request/response reference.

## Batch pattern

```sh
kranz draft fix-a
kranz draft fix-b
# read both parked plans — check the estimates before committing spend
kranz ticket approve fix-a
kranz ticket approve fix-b
kranz work        # leave it in a spare terminal; exits when the queue is dry
```

Queue order is `(priority, insertion order)` from the frontmatter
`priority` field. Batch-approving a night's worth and letting `kranz work`
grind is the intended workflow; reviewing each drafted plan's estimate
first is the one gate worth keeping human.

## Operational notes

- With `kranz serve` running, a drafted mission stays attached to serve
  (holding the mission lock) until approval releases it — the approve
  paths hand off automatically. Expect `kranz status` to show the lock
  moving between serve and the `kranz work` dispatcher.
- Ticket-approve is spend-adjacent everywhere: in Slack it is gated on the
  user allowlist like `/kranz new`.
- Mission outcomes flow back: exit 0 completes the ticket, a blocked
  mission (exit 2) surfaces for guidance, an underspecified plan (exit 3)
  bounces the ticket back with questions.
