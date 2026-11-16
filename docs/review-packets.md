# Human review packets

Use a packet to inspect the approved scope, actual change, checks, independent
findings and remaining human decision together:

```sh
kranz review-packet <mission-id>
kranz review-packet <mission-id> --json
```

The dashboard's **Human review packet** panel uses the same engine projection.
Choose **Read review packet**, then refresh before acting. A new event hides the
old packet; an edit outside the event log requires a manual refresh. The packet
names its mission, observation time, last event, gate attempt and result event.
Answer through the existing plan, grant, permission, question or merge controls.
There is no packet approval action and no new authority or persisted state.

The approved plan comes from `plan.approved` / `plan.revised`, not a mutable
`plan.json`. Candidate identity includes the pinned base, HEAD, selected source
inventory and exclusions. The committed diff and working-tree status are shown
separately. Paths outside the current approved touch set are visible, including
dirty and untracked paths. An empty touch set keeps the existing advisory-off
meaning; the packet does not invent a scope policy.

Each external evaluation retains its existing stage, mechanical/judgment kind
and blocking/advisory policy. Check rows reference retained receipts, including
the observed exit code, assertion count, checked content and environment in
JSON. Findings keep their artifact/line anchors. Events and retained paths are
references into the existing log and evidence bundle, not a second export.

| Status | Meaning |
| --- | --- |
| `current-pass` | The recorded pass still binds to the observed source, workspace, approval and policy; retained evidence verifies, required receipts pass without zero assertions, and the attempt remains live. This is not consent. |
| `historical` | A pass cannot be used as current evidence: source/bindings changed, the attempt expired, closed, was consumed or superseded, or the live subject is unavailable. |
| `failed` | A failed result, process error or unsuccessful required receipt. |
| `escalated` | The evaluator asked for human review. |
| `unavailable` | A result or required retained evidence is missing, changed, unreadable or outside the read budget. |

The packet compares each stage/gate with its preceding attempt and lists source
paths changed after a retained review snapshot. Changes to a file's executable
bit count too. Environment identities describe the recorded execution; this
read-only view does not rerun a command or qualify today's host environment.
Existing stage consumption still rechecks authority and freshness before work
can proceed. An obsolete packet cannot authorize a new request.

Native gate outcomes and older logs without source-bound receipts remain
**recorded results**, with current binding explicitly unavailable. Missing
worktrees are not recreated. Findings are never silently marked resolved, and
recorded waivers remain explicit exceptions whose applicability must be
rechecked. The view does not infer a waiver or a successful independent review.

Opt-in [baseline/candidate observations](contract-controls.md#baseline-and-candidate-observations)
have a separate section. It shows approved expectations, original source
identities, any checker overlay, environment labels and actual receipts.
Retained bytes must match their sealed digest. Source and environment
configuration matching is reported separately from the recorded outcome;
mutable toolchain, cache and service state remains unqualified. These advisory
observations never become a current gate pass or authorize reuse.

## JSON shape and compatibility

`kranz review-packet <mission-id> --json` is meant to be read by other tools.

- `schemaVersion` is `1`. Within a version, fields are only added. A removed,
  renamed or re-typed field raises the number, so check it before parsing.
- `missionStatus` is the mission's status as folded from the log through
  `throughSeq`.
- `nativeChecks` lists every recorded native gate result as data: `seq`,
  `gate`, `surface` (`approval` or `final-gate`), `kind`, `verdict` (`pass`
  or `fail`), `artefactRef`, `artefactDetail` and `artefactAvailable`. Read
  this list for gate outcomes. The same results also appear in `history` as
  prose for people; those titles are not a stable interface.
- A native check is a recorded result, not a current pass. `evaluations`
  carries the source-bound external evaluations and is empty for a mission
  that used none.

Without `--json` the command prints the same packet as Markdown, which is the
simpler input for a pull request description.

## Human view and validator boundary

`GET /api/missions/:id/review-packet` returns `{packet, markdown}`. It requires
the existing `x-kranz-token` read or mutation capability even on otherwise
anonymous loopback servers. The dashboard uses its existing token prompt when
needed. Repository-scoped URLs use the same route under `/api/repos/:repoId`.

For a report plus a fresh packet, request
`GET /api/missions/:id/report.md?review=true` with that capability. The ordinary
report endpoint remains unchanged. The combined view is generated in memory:
it is **not written into the committed report**, mission state, gate manifest
or validator snapshot. Keep manually saved human packets outside the source
tree supplied to an agent.

The projection never reads worker transcripts. External validators still receive
only S5's selected criteria, source, receipts and independent findings. They
receive neither human-view URLs nor read credentials. Tests inspect the built
input bytes and run a contained checker that attempts to read marked human
audit files and connect to a listening host endpoint. API tests also exercise
the always-authenticated route and prove that reads do not mutate the report,
log or control inbox.

Artifact reads reject symlinks, non-regular files, hard links and digest/length
mismatches. A packet reads at most 128 MiB of retained artifacts, spending the
budget on external evaluations first and revision pairs second, newest first
within each group. It observes at most 32 distinct stage baselines and 32 pair
baselines. Evidence outside those bounds
stays unavailable; it never becomes a passing claim. Use `kranz provenance` and
`kranz evidence-bundle` for the wider audit chain and retained bytes.
