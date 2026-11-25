# Parking a validator for a grant only when a denial stopped it

Design note for ticket `validator-park-only-when-denial-stopped-it`. It
records the cause as read from an event log, two operator decisions, and
what was built.

## Cause, confirmed from an event log

Found 2026-11-14 in missions m-373b57 and m-7886d9. In m-7886d9, run
`266c60e5`, the scrutiny validator ran `grep ...; echo "exit:$?"`. The
`echo` part of that compound command was denied: denials appear at seq 470
and seq 507. After the seq 507 denial the validator made about 33 more tool
calls (about a hundred events in total), at one point launching a
background `Agent` sub-agent, and its last tool call was that `Agent` call
at seq 572. `worker.completed` recorded a partial result at seq 574, and
`grant.requested` followed at seq 575.

`maybe_park_for_grant` (`crates/engine/src/orchestrator.rs`) offered the
first denied shell command of the session whenever the validator outcome
was untrusted — in this run, the `grep` line from seq 470, not the `echo`
line that actually ended the session. The engine parked on that grant, the
grant request timed out after an hour, and the mission blocked. Operator
guidance to the validator (one command per call, no background agents) made
the same validation pass with no grant at all, which confirms the denial
itself was not what sank the run — the 33 calls and the sub-agent after it
were.

## D-A: when a denial stopped the session (operator decision)

The rule: a denial stopped the session if no tool call happened after the
last denied tool result. Results of calls already issued before that
denial, and text or result events with no tool call, do not count against
it.

Rejected alternatives:

- **The denial is the last tool result.** This misfires on a parallel tool
  batch: a batch can return a denied result alongside other results in the
  same turn, with no further calls in that turn, even though the session
  goes on to make more calls afterward in a later turn. Counting results
  instead of calls-after would also wrongly call a denial "stopping" when
  only its sibling results, not a later call, happened to land after it.
- **The session ends within N events of the denial.** Event counts between
  a denial and the end of a session differ by backend and by how chatty a
  given validator run is, so any N would be arbitrary. The m-7886d9 run
  took about a hundred events after its stopping point to end; a smaller,
  equally untrusted run could end in ten.

## D-B: which command is offered (operator decision)

The grant offers the stopping denial's command — the one whose denied tool
result was the session's last tool call — not the first denied command of
the session. In m-7886d9 that means the grant, if any were due, would be
about `echo "exit:$?"`, not `grep ...`.

If the last denial carried no shell command (for example, a non-Bash tool
denial), the grant offers nothing: there is no command whose grant would
have let the session continue past that point.

## What was built

- `RunOutcome` (`crates/engine/src/runner.rs`) gains three fields:
  `tool_calls_after_last_denial` (count of tool calls made after the
  session's last denied tool result), `stopping_denied_command` (the shell
  command of that last denial, when the denial stopped the session and the
  denial had one), and `last_tool_call` (the name of the session's final
  tool call, for use in a block reason when no grant is offered).
- `maybe_park_for_grant` (`crates/engine/src/orchestrator.rs`) offers a
  grant only when `tool_calls_after_last_denial` is zero and
  `stopping_denied_command` is present. Otherwise it records an
  `orchestrator.decision` event whose summary says "not offering a grant
  for ..." and names how the session actually ended (no report, error,
  aborted, or failed; the last tool call; and the count of tool calls made
  after the last denial).
- In that case the engine uses the existing single validator retry instead
  of parking. If the retry is still untrusted, the milestone blocks with
  cause `untrusted-validator` and a reason naming how the session ended (no
  report / error / aborted / failed, the last tool call, and the tool calls
  made after the last denial), rather than a reason about a grant that was
  never going to fix it.
- No event or schema changed; `orchestrator.decision`'s existing summary
  and detail fields carry the new text.

## Not changed here

- The one-hour wait before a parked grant request is denied by default
  (`grantRequestTimeoutSecs`). Whether that wait should be shorter, or
  should notify someone before it expires, is a separate question.
- Validator prompt guidance (one command per call, no background agents).
  The m-7886d9 operator note shows this alone would have avoided the parked
  grant, but `prompts.rs` is frozen under
  `docs/knowledge/decisions/positioning-governance-evidence-layer.md`, so
  prompt changes are deferred.
- The worker deny-grant path (`maybe_park_for_worker_deny_grant`) still
  offers the first denied command of the session and has the same shape as
  the old validator path. It was not touched.
- Egress grants are unchanged: a proxy denial has no position in the event
  stream to compare against "last tool call," so the stopping-denial rule
  does not apply to it.
- Attribution inside a parallel tool batch can still name the wrong command
  of that batch. This is pre-existing and was confirmed by the operator as
  a separate, already-known gap, not something this change fixes.
