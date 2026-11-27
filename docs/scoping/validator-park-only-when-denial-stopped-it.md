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
first denied command of the session whenever the validator outcome was
untrusted. In this run that was the whole `grep ...; echo "exit:$?"` compound
command from seq 470, even though the session had carried on for about 33
more tool calls after the seq 507 denial and ended for an unrelated reason:
it launched a background `Agent` sub-agent and ended its turn without a
report. The engine parked on that grant, the grant request timed out after
an hour, and the mission blocked. Operator guidance to the validator (one
command per call, no background agents) made the same validation pass with
no grant at all, which confirms the denial itself was not what sank the
run. The 33 calls and the sub-agent after it were.

## D-A: when a denial stopped the session (operator decision)

The rule: a denial stopped the session if no tool call happened after the
last denied tool result. Results of calls already issued before that
denial, and text or result events with no tool call, do not count against
it.

Rejected alternatives:

- **The denial is the last tool result.** In a parallel tool batch the
  agent issues all of its tool calls before any result comes back, so the
  results of calls issued alongside a denied one can arrive after the
  denied result even though those calls were made no later than it. A
  denial that truly stopped the session can then fail to be the last tool
  result, and this rule would miss it. This is why D-A counts tool calls
  made after the denial, and ignores the order of results.
- **The session ends within N events of the denial.** Event counts between
  a denial and the end of a session differ by backend and by how chatty a
  given validator run is, so any N would be arbitrary. The m-7886d9 run
  took about a hundred events after its stopping point to end; a smaller,
  equally untrusted run could end in ten.

## D-B: which command is offered (operator decision)

The grant offers the stopping denial's command (the one whose denied tool
result was the session's last tool call), not the first denied command of
the session, and the offered command is the whole command of that stopping
tool call as recorded, not a fragment of it. In m-7886d9, under D-A no
denial stopped the session: the last tool call was the background `Agent`
call at seq 572, not either denial. So no grant is offered; the engine
records a `not offering a grant for ...` decision, uses the single
validator retry, and if the retry is still untrusted it blocks the
milestone with a reason naming how the session ended.

If the last denial carried no shell command (for example, a non-Bash tool
denial), the grant offers nothing: there is no command whose grant would
have let the session continue past that point.

## What was built

- `RunOutcome` (`crates/engine/src/runner.rs`) gains three fields:
  `tool_calls_after_last_denial` (count of tool calls made after the
  session's last denied tool result; any tool's denial counts, not only a
  shell one; `None` when the session had no denied tool result),
  `stopping_denied_command` (the shell command of that last denial, when
  the denial stopped the session and the denial had one), and
  `last_tool_call` (the session's last tool call rendered as
  `<tool>: <summary>`, scrubbed and bounded to 200 characters, for use in a
  block reason when no grant is offered).
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
- The worker deny-grant path (`maybe_park_for_worker_deny_grant`) now also
  follows D-A/D-B (ticket `worker-deny-grant-only-when-denial-stopped-it`,
  operator decision D-X1). See "Worker path (D-X1)" below.
- Egress grants are unchanged: a proxy denial has no position in the event
  stream to compare against "last tool call," so the stopping-denial rule
  does not apply to it.
- Attribution inside a parallel tool batch can still name the wrong command
  of that batch. This is pre-existing and was confirmed by the operator as
  a separate, already-known gap, not something this change fixes.

## Worker path (D-X1)

`maybe_park_for_worker_deny_grant` offers the `WorkerDeny` grant only for the
deny rule matching `stopping_denied_command`: the same D-A/D-B test
`maybe_park_for_grant` uses, read from the same `RunOutcome` fields. There
are two cases where it does not offer a grant, each recorded as one
`orchestrator.decision`:

- The denial did not stop the session (a tool call followed it):
  `not offering a grant for `<first denied command>`: the denial did not
  stop the worker session; <how the session actually ended>`.
- The denial that did stop the session matches no liftable worker deny rule
  (a hook denial or a non-Bash tool denial): `not offering a grant for
  `<command>`: it matches no liftable worker deny rule`.

In both cases the run is not parked: it goes to the normal judgement and is
charged to `max_respawns` like any other non-passing run. Only a run that
IS parked for a `WorkerDeny` grant has its respawn credited via
`grant_respawns`, so it does not also consume the respawn budget.

The accepted caveat: a worker that retries a variant of a denied command
(for example, denied `git stash`, then later denied `git push`) makes a
tool call after the first denial before failing, so the grant offered is
for the LAST denied command's rule, whose matching rule may differ from the
first denial's rule.
