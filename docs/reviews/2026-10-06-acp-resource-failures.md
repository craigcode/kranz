# ACP resource failures: R2 review

Base: public `44356285`. R2 extends the test-only resource revision from R1.
The [parent ticket](../../.kranz/tickets/acp-resource-budget-qualification.md)
remains open for R3 production qualification and R4/R5 terminal work. The
[scope and additive contractChangeRequest](../scoping/acp-resource-budgets.md#r2-implementation-and-contractchangerequest)
record the operator's 2026-10-06 approval and the exact authority boundary.

## Findings resolved

Automatic container removal could erase OOM state before the host inspected it.
The resource revision retains stopped state until explicit owned removal; v1's
argv and original supervisor stay unchanged. Owner death still expires the guest
lease, but can leave an exited namespace plus its recovery ledger. A dedicated
proof verifies stopped descendants and performs explicit owner-bound recovery.

Worker stderr and exit codes cannot distinguish memory exhaustion, a handled
process limit, a normal exit or a timeout. R2 uses matched Docker inspection and
a protected supervisor record. The supervisor opens and unlinks its observation
file before peer spawn, stays non-dumpable, and closes that descriptor in every
child. The host reads its retained inode, not a pathname the peer can recreate.
A checksum detects torn writes; byte caps bound parsing. Host namespace/image,
supervisor hash, owner, observation interval, counters and configured ceilings
travel together. Missing or ambiguous evidence stays explicit. An OOM record
does not distinguish cgroup pressure from host pressure, and the message does
not claim that the configured memory ceiling was necessarily the cause.

A failure during initialization previously returned before the normal worker
completion record. A bounded ACP profile now returns an already-closed failed
session after cleanup, allowing the runner to retain its observations. Resource
observations are a separate backend channel, not fields copied from peer JSON.
The optional completion and folded-run fields preserve old serialization.

The new revision adds a supervisor-enforced session deadline and per-tmpfs cap.
The session deadline includes handshake and permission waits; neither lease
expiry nor exit 124 alone is a wall-clock classification. The tmpfs cap covers
shared memory and explicit tmpfs mounts, with Docker's managed 64 MiB `/dev`
within the admitted minimum. It is not a quota for host bind-mounted storage.

## Five-axis review

Self-review; this is not an independent agent review.

- Correctness: observed OOM/deadline failures cannot become passing worker
  attempts. CPU throttling and a handled process-limit hit do not manufacture a
  failure. Concurrent hits remain unattributed. Cleanup failure remains a failed
  session, and `namespaceCleanupConfirmed` refers only to namespace removal.
- Readability: configured ceilings, observations and classification are separate
  fields. Messages name the observed resource, configured value and operator key.
- Architecture: use the existing backend, runner, completion event and reducer;
  no new event type, scheduler, retry mechanism or authority surface. Boxed event
  and folded-state evidence avoid inflating the persisted-event enum in memory.
- Security: worker text cannot choose the typed classification; namespace ID and
  ownership must match; replacement files do not replace the engine's descriptor.
  Trusted supervisor bytes and startup flags change only on the test revision.
  Credentials still travel only through existing private channels. Production v2
  and released terminal capabilities remain unavailable.
- Performance: the test revision samples bounded counters every 100 ms and adds
  bounded kill/inspect calls before removal. The existing v1 execution path does
  not gain sampling or inspection. R3 must measure this overhead with real work.

## Validation

The R2 Docker proofs require each test by name in the existing Linux containment
lane and reject skip markers. They cover memory/process/time exhaustion,
initialization timeout, forged records, exit-code ambiguity, CPU-throttled useful
work, tmpfs exhaustion handled by a successful worker, completion-event replay,
abort, Drop and owner death. Portable tests cover classification, counter resets,
namespace mismatch, inode replacement, old logs, override ranges and authority.

Local execution uses the existing pinned Python image in the Docker VM, with
synthetic peers and no provider credentials or paid agent session. Final local
check logs and PR checks are the validation receipts; the review does not qualify
production adapter values or promise native Linux proof before CI completes.
