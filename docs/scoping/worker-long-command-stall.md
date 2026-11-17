# Worker sessions that end while their own command is still running

Design note for ticket `worker-long-command-stall`. It records the cause as
read from an event log, the operator's decision, and what was built.

## Cause, confirmed from an event log

The ticket's account came from orchestrator notes. It is now confirmed from
the `worker.message` events of one mission run on 2026-11-16 with the Claude
backend. That mission started 26 worker sessions. 21 ended without a passing
result, and in 10 of those the stream shows the same sequence:

1. The worker ran a long command in the foreground, in every case the
   workspace test suite or a large part of it.
2. The agent CLI answered with its notice that the command "did not complete
   within its 120s timeout and was moved to the background", or the worker
   started the command in the background itself.
3. The worker spent the rest of the session waiting: text turns saying it
   would wait, `sleep` calls the CLI blocked, attempts to schedule a wake-up,
   and polling loops.
4. The session ended with a partial report or with no report at all. The
   command's result never reached it.

Five features in that mission failed with "respawn budget exhausted". Two of
the five passing sessions also had a command moved to the background, so the
notice alone does not mean a session was lost.

Two things the event log does not show:

- Whether the backgrounded command later finished. The agent CLI's completion
  notice is not an event the engine receives.
- Which limit ended each session. A session that ends with no report after
  about the same number of events each time is consistent with a turn limit,
  and that is an inference, not a recorded fact.

## D-A: is a remedy in scope? (operator decision)

The positioning decision freezes new execution primitives
(`docs/knowledge/decisions/positioning-governance-evidence-layer.md`). Three
candidates were weighed against it.

| Candidate | What it changes | Decision |
|---|---|---|
| Record the stall in the event log | Evidence only | Built |
| A stalled run does not spend a respawn | Respawn accounting | Built, limited to one per feature (operator decision, 2026-11-16) |
| The engine sets the agent CLI's foreground command limit from worker config | How the agent executes commands | Not built. Open |
| The session tool policy allows waiting on a command the worker started | Widens the worker tool policy | Not built |

The second row was the operator's choice: classify these runs separately
from ordinary partial runs. The credit is limited to one run per feature so
that the extra spend is bounded at one worker session per feature.

The third row is the only candidate that would let the command finish inside
the session. It is left open because it is an execution change and because
the setting that controls the limit has not been checked against the agent
CLI's documentation or a live run. The credit does not prevent a stall: in
the mission above, respawned workers stalled the same way again.

The fourth row is not pursued. It widens what a worker session may do, and
the wait calls in the log were refused by the agent CLI's own rules for the
session mode, not by a kranz deny pattern.

## What was built

- `RunOutcome.backgrounded_commands` (`crates/engine/src/runner.rs`): the
  shell commands the agent CLI reported as moved to, or started in, the
  background. The notice is matched at the start of a tool result, and the
  command is taken from the tool call before it, the same correlation the
  denied-command capture uses. Commands are scrubbed and bounded. Backends
  with no such notice record nothing.
- After a worker run that did not pass and has at least one such command,
  the engine records an `orchestrator.decision` event. Its summary says the
  run ended with its command moved to the background and whether the run was
  credited. Its detail names the run, the feature, the result and the
  commands. No event schema changed.
- The first such run for a feature is not counted against `maxRespawns`.
  Later ones are counted as before. The credit is held in the engine process,
  like the credit for grant re-runs: a restart drops it, which fails closed.

## Limits

- The record says a command left the foreground and the run did not pass. It
  does not prove the command was still running when the session ended.
- A run that moved a command to the background and then failed for another
  reason is recorded the same way. The one-per-feature limit bounds what that
  costs.
