# Recorded outcome reasons

`kranz outcomes --window-days 30` and the dashboard Outcomes view explain
recorded interruptions to work. Add `--json` for the same engine projection
served by `GET /api/missions/outcomes?windowDays=30`. This is a read-only report:
it starts no worker, check, retry, credential flow or permission response.

The mission's current state remains the existing reducer's state. Earlier
observations remain visible after repair or completion. A completed mission is
not evidence that its change was released, deployed or cut over in production.

## Mapping version 1

The report classifies structured events, never words in an error message.

| Category | Recorded evidence |
| --- | --- |
| Environment prerequisite | An engine-owned block with the explicit `authentication` cause. Other setup failures remain unknown unless their producer records a supported structured cause. |
| Reported defect | A finding from a recorded native validator run; or a failed external judgment evaluation with findings. This records the checker's judgment, not an independently established root cause. |
| Human/policy boundary | Operator blocks, typed grant/security/independence blocks, the engine's out-of-contract finding, validator tamper events, permission/grant requests and denials, questions, proposed plan revisions, external escalation, or a stage resolution that blocks/requires a human. |
| Interrupted | An explicit pause or interrupt request. The request alone does not prove that a process was killed or explain why it was interrupted. |
| Cancelled | A recorded mission abandonment. |
| Unknown | Generic failed workers/features/missions, native command failures, failed mechanical gates, external evaluator errors, legacy or unsupported block causes, or a log the reducer cannot validate. A command's nonzero exit cannot distinguish a defect from missing setup. |

A finding can coexist with a policy block or an unknown execution failure in
one mission. Absence of a classified observation means no such cause was
recorded; it does not mean the work succeeded. A rejected policy action is never
relabeled as a credential problem to suggest a retry.

Each observation retains the event sequence, timestamp and type, available
milestone/feature/run or evaluation-attempt identity, stage, block context and
actual recorded actor. One-call decisions also name their permission request.
These references locate the detailed source in the mission event log or its
existing evidence export. Display details are scrubbed and limited to 4,096
characters; they are not a replacement transcript or a complete secret filter.

An actor may identify a local authority capability or policy rather than a
verified person. Legacy events without actor attribution remain null.

## Windows, counts and pending work

The CLI and API default to a 30-day activity window; the dashboard offers 7,
30 and 90 days. The selected cohort contains missions whose **latest recorded
event** falls within the inclusive window. The report retains the full recorded
history of those missions, including older attempts. It is an activity cohort,
not a count of failures that occurred during the window. Idle missions outside
the window are excluded even if they remain blocked.

Each task-class denominator counts selected missions once. Each category counts
a mission at most once, with a separate observation count. Categories overlap,
so their percentages must not be added. `mixedMissions` counts missions with
more than one recorded category. Missing task classes use `unclassified`.
Unreadable or missing logs are listed separately and excluded from denominators;
the report cannot determine their activity window. Existing cost, latency and
other metrics keep their existing scopes. `--all` retains its existing
cross-repository merged-cost report.

Requests and blocks are `unresolved` until a matching recorded workflow event
resolves or closes them. A replacement in the same pending slot marks the older
observation `superseded`, with the replacing event as its reference. Terminal mission events close outstanding rows; this
is not permission or proof of a repair. Permission deadlines can make a row
`expired`: the interactive report compares to its recorded `through` time,
while a log-only export can only compare to the latest event time. Replaying
identical events with an identical window yields identical data.

## Compatibility and contractChangeRequest

The additive report contract is owned by `outcomes.rs`: optional
`outcomeReasons` contains `mappingVersion: 1`, the selected window, mission
observations, task-class counts and unavailable log IDs. Consumers must handle
its absence in older reports. `MissionCostSummary` in an evidence bundle's
existing `cost.json` also gains optional `outcomeReasons`, using the same
single-mission fold without a report-time window. Existing fields, export files
and mission states retain their meaning. No persisted event schema changes.

The remaining event-producer gap is deliberately not guessed here. A future
contractChangeRequest for `command_exec`/contract-check or backend completion
producers should add an optional typed failure cause and evidence reference
when those producers can establish assertion rejection versus execution/setup
failure. Older events must default to unknown. That change needs producer-side
evidence and tests; reporting cannot establish the distinction from diagnostics.
