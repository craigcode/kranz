# Bounded server read work

Synchronous filesystem reads, JSON parsing, event-log validation/folding, Git probes, readiness reads, and cross-mission aggregation now run through a bounded blocking-task pool. Each mounted repository has eight read slots; its scoped route and unscoped compatibility alias share the same slots. `/api/repos` has a separate eight-slot catalog pool. Artifact reads retain the existing process-wide eight-slot pool and 8 MiB file limit; plan/transcript parsing happens inside that task too.

The limiter is a private router extension. `ServerState` retains its public fields and can still be constructed by library callers. Cloned routers and WebSocket sessions share their repository limiter. There is no additional outcome cache or alternate source of mission truth.

When a pool is full, a new REST read returns HTTP **503** with a JSON `error` explaining that repository readers are busy and the caller should retry shortly. There is no unbounded admission queue and no automatic replay of mutations. `/api/health` does not use a read slot. Existing read-token, Host, and WebSocket Origin checks stay outside the work admission path and retain their behavior.

A permit moves into the blocking closure. Cancelling the awaiting request does not free capacity while its task continues to consume CPU or I/O; capacity is released on completion, error, or panic. This limits concurrent work, not the size of an event log. Mission logs retain full validation and existing integrity guarantees, without truncation or silently incomplete views.

WebSocket setup offloads filesystem checks and the initial authenticated fold. Live polling still validates the entire log, including the prefix before the client's cursor. If its repository pool is busy at a poll, the session keeps the last validated state/cursor and retries on a later tick. If setup cannot acquire capacity, the upgrade returns 503; if capacity disappears between upgrade and initial fold, the socket closes and the client can reconnect. Corrupt logs close the session. Refold errors still fail closed.

Coverage includes mission listings/state/events/workspace/standards/revision/diff/PR-handoff/readiness/hook-status, outcomes and metrics, ticket reads, queue readiness, pending-plan reads, and repository summaries. Mutating routes retain their existing scheduling and acknowledgement semantics. Artifact limits continue to refuse oversized and nonregular files.

Validation includes single-runtime-thread responsiveness, bounded admission and cancellation, recovery after failure/panic, real concurrent reads of an authenticated log larger than 1 MiB, refusal of a corrupt prefix even when `since` lies beyond it, and a live WebSocket retaining its cursor through overload before detecting a corrupted earlier event.
