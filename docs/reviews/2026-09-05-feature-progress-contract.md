# Contract change: durable sequential feature receipts

`contractChangeRequest`: additive event/state change under the approved
release audit. The live rehearsal retained a first-attempt checkpoint in Git,
but a successful verification retry emitted an empty `feature.completed`
receipt. A failed retry could also make retained work appear eligible for
implicit fix-feature supersession.

The engine now writes `feature.progress` with `featureId`, `baseSha`, and
`commits`. It pins the active feature's baseline before starting its first
worker, then records cumulative engine-observed receipts after workers and
checkpoints. Receipts retain the existing `SHA subject` representation.
The folded state adds `featureBaseShas`, defaulting to an empty map and omitted
when empty. The log remains authoritative; the cached state is not trusted.

A baseline cannot change during a feature. Previously recorded commits must
remain ancestors of HEAD; removed work fails closed. Terminal receipts merge
by SHA only for features carrying the new baseline. A failed feature with any
recorded work cannot be replaced as a commitless proposal. An actual commitless
supersession clears the predecessor's baseline before the successor starts.

Existing event variants and fields are unchanged. Logs without the new event
retain their previous fold and serialized state. On upgrading an already active
legacy feature, the engine pins the current HEAD and preserves existing
receipts; it cannot reconstruct receipts that an older engine already lost.
No historical log is rewritten or re-signed. Older binaries cannot read the
new event variant, so downgrade requires finishing or retaining those missions
for the newer binary. This release already introduces versioned event seals.

Regression evidence covers partial-attempt checkpoints followed by successful
and failed no-write retries, process restart after a checkpoint in checkout
and worktree modes, baseline immutability, terminal progress refusal,
supersession guards, receipt deduplication, and legacy state compatibility.
The original retry regression failed before the change with zero receipts
for a commit still present in Git.
