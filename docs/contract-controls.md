# Critical assertion controls

A command assertion can carry an optional `negativeControl` object. The author
provides approved checking inputs, a valid implementation, and a deliberately
defective implementation. Approval and final validation run the **same assertion
command** against both implementations and retain fresh evidence at the exact
source revision. The dashboard shows each path and expandable file contents for
operator review.

```json
{
  "checkerFiles": [{"path": "checks/auth.sh", "content": "...approved checker..."}],
  "validFiles": [{"path": "authorization.sh", "content": "...valid implementation..."}],
  "defectiveFiles": [{"path": "authorization.sh", "content": "...chosen defect..."}],
  "expectedFailure": "invalid-credential-authorized",
  "timeoutSeconds": 60
}
```

`checkerFiles` must exactly match files in the inspected checkout and in the
pinned revision. Include the checker and relevant helpers, not just its name.
The valid and defective groups replace matching paths in fresh detached
worktrees; they cannot replace a declared checking input. A source with tracked
changes or hidden index flags is inconclusive. An unavailable future checker is
also inconclusive, even when its intended behavior is clear from the plan.

Each group contains 1–16 UTF-8 files, at most 64 KiB each and 512 KiB total across
the three groups. Paths are relative, unique, and cannot traverse symlinks or
name Git/mission metadata. At most eight assertions may declare controls. Each
case has a 1–180 second command deadline (default 60); the evaluation has a
300-second launch/execution budget. Once that budget expires, no further case
starts. In-flight Git setup and cleanup retain their own bounded deadlines, so
the budget is not a strict wall-clock deadline including cleanup.
Only one control evaluation executes per process at a time. Concurrent requests
receive inconclusive busy evidence without queueing more commands. Cancelling
an awaiting final-validation task prevents subsequent control launches; an
already running case keeps its command deadline and cleans up its worktree
before releasing capacity.

## Checker result protocol

The checker writes a JSON receipt to the fresh file named by
`KRANZ_CONTROL_RESULT`. Ordinary assertion execution does not set this variable;
an adapter should write the receipt only when the variable is present, while
preserving its ordinary exit status. `KRANZ_CONTROL_SCRATCH` names private writable
scratch. HOME, temporary files, Cargo target output, and result receipts stay in
scratch; the checkout and approved checking inputs are read-only.

Successful behavioral checks:

```json
{"checksRun":3,"outcome":"passed"}
```

Rejection of the chosen authorization defect:

```json
{"checksRun":2,"outcome":"failed","failureId":"invalid-credential-authorized"}
```

The complete authorization example in
[`contract_controls_tests.rs`](../crates/engine/src/contract_controls_tests.rs)
tries a missing credential, a wrong nonempty credential, and a correct
credential, and checks whether the mutation happened. A weakened checker that
omits the wrong nonempty credential accepts the defective implementation and
produces **not rejected** evidence.

| Evidence | Meaning |
| --- | --- |
| Verified | Valid case exits zero with positive `checksRun` and `passed`; defective case exits nonzero with positive `checksRun`, `failed`, and the expected `failureId`. |
| Not rejected | Both cases exit zero and report positive behavioral checks with `passed`. The selected defect escaped the check. |
| Inconclusive | Missing or malformed receipt, zero checks, build/setup error, different failure, timeout, stale checking input, unavailable containment, exhausted budget, or unavailable evidence file. |

Output text and an arbitrary nonzero exit do not prove rejection. Receipts are
bounded regular files, opened without following symlinks. They are claims made
by the approved checker; this mechanism does not independently prove a checker
is truthful or that the fixtures cover every implementation of the requirement.

## Containment and evidence

Controls always require native macOS Seatbelt or Linux bubblewrap containment,
including when worker enforcement is off. Worker `fs+net` restrictions remain in
effect. Extra writable roots are removed for controls. Only scratch is writable;
the real checkout, detached checkouts, shared Git metadata, and authority files
remain protected. Unsupported platforms and container configurations currently
produce inconclusive evidence rather than running uncontained. There is no
degradation opt-in for controls.

Each evaluation writes a unique scrubbed JSON artifact under the mission's
`runs/` directory. It contains assertion/checker/control SHA-256 identities,
source revision, platform, containment posture, environment **names**, bounded
output, elapsed times, case receipts, and the combined outcome. The existing
`gate.result` records link it through a `file:` artifact reference. Plan Markdown
and gate diagnostics show advisory results; evidence bundles export referenced
artifacts and mark missing files unresolved. Final validation always reruns the
controls at its current revision; it never reuses an approval receipt.

Controls retain the existing advisory policy. Inconclusive and not-rejected
results are non-passing gate evidence, but do not independently block approval
or completion. Existing assertions, gates, and validation findings still decide
those transitions. Replanning cannot replace an approved assertion's controls;
new controls require a new assertion identity under the existing additive
contract rules.

This implements the negative-controls portion of
[`contract-readback-and-negative-controls`](../.kranz/tickets/contract-readback-and-negative-controls.md).
Independent model read-back and a recorded operator comparison remain open in
that ticket; no model read-back is implied by a verified control pair.
