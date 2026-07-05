# What is kranz — a quickstart overview

kranz is a local mission-control harness for Claude Code. You describe an
outcome in plain language; kranz turns it into a **mission** — a planned,
human-approved, validated, fully-audited run of headless `claude` sessions
against your repo — that you can start from a terminal, a browser, Slack, or
a pair of glasses, and then walk away from.

The honest one-liner: **kranz turns "run claude on this task" into a durable,
auditable, human-gated state machine.** The CLI does the thinking; kranz owns
when it's allowed to think, what it's allowed to touch, who checks its work,
and what happened, forever, in a log you can replay.

## What it does

- **Plan interactively, cheaply.** `kranz plan "goal"` opens a conversation
  with an orchestrator agent that shapes the goal into a structured plan:
  milestones, contracts, and the validation commands that will prove each one.
  Nothing touches your repo during planning.
- **Gate on human approval.** A plan does nothing until you approve it. That
  approval is the one privileged moment: it pins the git base SHA, commits the
  plan into the mission's event log, and starts (or queues) the run.
- **Execute with separated powers.** A worker agent implements one milestone
  at a time on a mission branch; a separate validator agent — a fresh session
  that never saw the worker's reasoning — re-runs the validation commands and
  checks the diff against the pinned base. The thing that did the work never
  grades itself.
- **Survive anything.** Crash the process, kill the laptop, hit a usage limit:
  every event was already appended to the mission's log, so `kranz run`
  resumes exactly where it stopped. Locks are stolen only from provably dead
  holders.
- **Report everywhere.** `kranz serve` exposes the same missions over a web
  dashboard, a Slack bridge (plan, approve, and steer from a thread), and a
  minimal REST/SSE API that even a G2 glasses app can drive.
- **Queue and drain.** Missions serialize per repo (they own the working
  tree). Approvals can enqueue; `kranz work` drains the queue one mission at a
  time, crash-safely.

## Quickstart

Build and install from this repo:

```sh
cargo install --path crates/cli   # installs the `kranz` binary
```

Run your first mission from any git repo:

```sh
cd your-repo
kranz plan "Add a --json flag to the report command, with tests"
# …interactive planning conversation; approve the plan when it's right…
kranz run        # executes the mission loop (resumable if interrupted)
kranz status     # mission tree, milestones, cost — read-only, any time
```

Watch and manage it from a browser (and Slack/glasses, if configured):

```sh
kranz serve      # dashboard + REST/SSE on http://127.0.0.1:4560
```

Other entry points, when you need them:

```sh
kranz ticket new <slug>     # backlog: capture work before it's a mission
kranz exec plan.json        # fully headless: plan in, exit code out (CI)
kranz work                  # drain the per-repo execution queue
kranz otel --endpoint <url> # export missions as OpenTelemetry traces
kranz config                # inspect/edit config, including mid-mission
```

Exit codes are part of the protocol: `0` complete, `1` failed, `2` blocked
(needs a human), `3` underspecified (needs more planning).

## How it does it

Under the hood, kranz is three things: an **event-sourced state machine**, a
**process supervisor for headless `claude` CLI sessions**, and a **git
choreographer**. Every surface — dashboard, Slack, glasses — is a thin client
over those.

### The event log is the only truth

Every mission is a directory (`.kranz/missions/m-xxxxxx/`) whose heart is
`events.jsonl` — an append-only log of typed events (`mission.created`,
`plan.approved`, `worker.spawned`, `milestone.completed`, `run.finished`, …).
Nothing else is authoritative: current state is always computed by folding the
log through a pure reducer (`crates/engine/src/reducer.rs`). `state.json` is
just a cached snapshot; if it's stale or corrupt, the fold rebuilds it. That's
why crashes are cheap — there's no "half-written state" problem, only "log
ends earlier than you hoped."

Writes are guarded by a single-writer file lock (`events.jsonl.lock` holding
pid + identity token + timestamp). Liveness is probed, not assumed: a lock
whose holder is provably dead is stolen automatically.

### Missions are spawned `claude` processes with roles

When a mission needs an agent turn, the engine builds a `SessionSpec` and the
backend (`crates/engine/src/backend_claude.rs`) spawns the installed `claude`
binary headless:

```
claude -p --output-format stream-json --verbose \
  --model … --effort … --session-id … \
  --append-system-prompt <role prompt> \
  --permission-mode … --allowedTools … [--json-schema …]
```

It reads the stream-json event stream off stdout line by line, translating CLI
events into mission events as they happen (cost and token accounting come from
the same stream). The child is made a process-group leader (Unix) or put in a
kill-on-close Job Object (Windows), so an abort kills the whole tool tree —
test runners and builds die with the agent instead of orphaning. Conversation
continuity across turns is just `--resume <session-id>`; kranz never stores
conversation state itself, only the session id in the event log.

Three roles use this same mechanism with different system prompts and tool
policies:

- **Orchestrator** — the planning brain. `kranz plan` converses with it;
  requesting a plan forces it to emit a structured plan (milestones,
  contracts, validation commands) against a JSON schema.
- **Worker** — executes one milestone at a time in the repo, with the plan's
  contract in its prompt and a schema-validated report required at the end.
- **Validator** — a separate fresh session that checks the worker's claims:
  runs the validation commands and diffs against the pinned base SHA. This is
  the scrutiny layer; skipping it (`skipScrutiny`) requires an explicit
  `--allow-unvalidated`.

### Plans are contracts, and approval is the gate

Planning is interactive and cheap — under `kranz serve`, the orchestrator
session sits parked in a registry (released after 30 idle minutes) so Slack
replies and plan requests reuse its full context. Nothing touches the repo
until plan approval, which pins `KRANZ_BASE_SHA` at that moment — contracts
diff against a fixed point, not a moving `main` — and either starts the run or
enqueues it.

The queue (`crates/engine/src/queue.rs`) exists because missions serialize per
repo: a running mission owns the working tree. Entries are one JSON file each,
named `priority-seq-missionId` so plain filename sort *is* the schedule;
claiming is an atomic rename to `.claimed.<pid>`, which is what makes
dispatcher crashes recoverable.

### The run loop

`run()` in `crates/engine/src/orchestrator.rs` drives: check out the mission
branch → for each milestone, spawn a worker → parse its schema'd report →
spawn a validator → on validator pass, record the milestone and continue; on
fail, feed the findings back for a retry. On completion the mission branch
merges back to main; conflicts get a synthesized-resolution pass. A final
lesson-capture turn distills what tripped the mission into `.kranz/lessons/`,
seeded into future missions' prompts.

### Everything else is a client

`kranz serve` folds the same logs and exposes REST + SSE. The web dashboard,
the Slack bridge (Socket Mode, buttons calling the same approve/abandon
endpoints), and the G2 glasses app are all just renderers of
`GET /api/missions` and posters to `POST /api/*` (mutation-token gated). None
of them hold mission state; kill any of them and the missions don't notice.

## Where to go next

- `docs/design.md` — the full architecture and its invariants
- `docs/protocol.md` — the REST/SSE surface
- `docs/backlog-and-slack.md` — tickets, the queue, and the Slack bridge
- `docs/gascity.md` — running kranz missions from Gas City beads
- `docs/otel.md` — exporting missions as OpenTelemetry traces
