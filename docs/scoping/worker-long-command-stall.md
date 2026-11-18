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
| The engine sets the agent CLI's foreground command limit from config | How the agent executes commands | Built (operator decision, 2026-11-17) |
| The session tool policy allows waiting on a command the worker started | Widens the worker tool policy | Not built |

The second row was the operator's choice: classify these runs separately
from ordinary partial runs. The credit is limited to one run per feature so
that the extra spend is bounded at one worker session per feature.

The third row is the only candidate that lets the command finish inside the
session. It is an execution change, so it was held until the operator decided
it, and it is one setting passed to the agent CLI, not a new mechanism in the
engine. The credit in the second row does not prevent a stall: in the mission
above, respawned workers stalled the same way again.

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

- Config `agentCommandTimeoutSecs` sets how long a shell command may stay in
  the foreground in a worker or validator session. Absent means 1800 seconds;
  `0` leaves the agent CLI's own limit in place. The engine passes it to the
  session as `BASH_DEFAULT_TIMEOUT_MS` and `BASH_MAX_TIMEOUT_MS`, the Claude
  CLI's documented variables for the default and the maximum foreground
  limit (the default is 120000, the "120s timeout" in the notice above).
  Both carry the same value, so the configured limit is also the ceiling.
  Other backends ignore the variables.

Checked on 2026-11-17 with Claude CLI 2.1.281, in a single-shot session with
both variables set to 600000: a command that sleeps for 140 seconds stayed in
the foreground and its output came back as the tool result, with no
background notice. No control run without the variables was made; the
two-minute behaviour is the one recorded in the mission log above.

## Limits

- The record says a command left the foreground and the run did not pass. It
  does not prove the command was still running when the session ended.
- The model can still name a shorter limit for a single command. That
  command is moved to the background at the shorter limit, as before.
- Nothing in the engine watches a worker or validator session for silence,
  so while a long command runs the event log shows the tool call and then
  nothing until it returns.
- If a future agent CLI stops honouring the variables, sessions fall back to
  the two-minute limit. The stall record is what would show it.
- A worker can still start a command in the background itself. The limit
  does not change that, and such a run is still recorded as a stall.
- A command that hangs now holds its session for up to the configured limit
  before the agent CLI moves it on.
- A run that moved a command to the background and then failed for another
  reason is recorded the same way. The one-per-feature limit bounds what that
  costs.
