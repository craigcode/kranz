# Mission plan — m-c9c915

**Goal:** Fix F1 (drain() must not discard acknowledged buffered deltas on a mid-loop write failure) and F2 (idle missions must age-flush buffered deltas) in crates/engine/src/event_log.rs, with regression tests proving both failure modes and all existing tests kept green.

Branch `kranz/mission-m-c9c915` (from `main`). Approved plan of record; the machine-readable twin is [plan.json](plan.json). Live status: `kranz status` or the dashboard.

## Validation contract

Defined before any feature; gates mission completion.

- **[a1]** When drain_buffer hits a write error on the k-th buffered delta, deltas 1..k-1 are written to the file, and deltas k..N remain in the in-memory buffer (never silently discarded) and are recoverable by a subsequent successful drain. 
  `cargo test -p kranz-engine drain_retains_unwritten_deltas_on_write_failure`
- **[a2]** A buffered stream delta whose age has exceeded the throttle is flushed to events.jsonl by an age-based flush call, with no intervening append(), flush(), or drop of the log (an idle mission ages out its buffer). 
  `cargo test -p kranz-engine flush_if_due_drains_idle_buffer_by_age`
- **[a3]** buffer_age() reports None for an empty buffer and Some(age-of-oldest) otherwise, and flush_if_due() drains only when the oldest buffered delta's age is at least the throttle (a young buffer stays buffered). 
  `cargo test -p kranz-engine buffer_age`
- **[a4]** The full engine test suite (all pre-existing event_log and orchestrator tests plus the new regression tests) passes. 
  `cargo test -p kranz-engine`
- **[a5]** The workspace builds and is clippy-clean with no new warnings. 
  `cargo clippy -p kranz-engine --all-targets -- -D warnings`
- **[a6]** On a drain/flush failure the Drop path reports HOW MANY buffered deltas were retained/unwritten (not merely that flushing failed), and the orchestrator's poll loop invokes the age-based flush so idle missions flush in production, not only in the unit test. *(agent judgement)*

## Milestone 1 — F1 — buffered deltas survive a mid-drain write failure

### 1.1 Prefix-based drain that retains unwritten deltas on a write error

FILE: crates/engine/src/event_log.rs (Rust, part of the kranz-engine crate). CONTEXT: EventLog is an append-only JSONL event log. Stream deltas (worker.message) are buffered in memory in `self.buffer: Vec<BufferedLine>` and later drained to `self.file: std::fs::File`. The current `drain_buffer` (around lines 256-261) is:

    fn drain_buffer(&mut self) -> Result<()> {
        for buffered in self.buffer.drain(..) {
            self.file.write_all(buffered.line.as_bytes())?;
        }
        Ok(())
    }

BUG (finding F1 in docs/reviews/event-log-review.md): `Vec::drain(..)` sets the buffer length to 0 immediately and yields items lazily; if `write_all` fails on the k-th item via `?`, the Drain guard's Drop discards every not-yet-yielded item k+1..N. Those deltas were already returned Ok(event) to their producers by earlier append() calls, so they are silently lost with no way to recover or enumerate them. drain_buffer has three callers: append() (~line 238), flush() (~line 251), and Drop::drop (~line 345).

REQUIRED FIX: Rewrite the drain so that on a write error the not-yet-written lines STAY in `self.buffer` (in order) for a later retry, and only the lines actually written are removed. Write from the FRONT: for each front element, write_all it; only on success remove it from the buffer; on the first Err, return that Err leaving the remaining (including the one that failed) still in `self.buffer`. Extract a writer-agnostic helper so this logic can be unit-tested with an injected failing writer — e.g. a free fn or associated fn `fn drain_lines<W: std::io::Write>(sink: &mut W, buffer: &mut Vec<BufferedLine>) -> std::io::Result<()>` (or Result<()> using the crate error) that drains from the front and leaves unwritten lines in place. `drain_buffer` then calls it as `drain_lines(&mut self.file, &mut self.buffer)`. Do NOT change the public API of EventLog (append/flush/acquire signatures stay identical). Note fsync (sync_data) is File-specific and stays in append() exactly as now — only the buffer-writing loop is abstracted.

DROP DIAGNOSTIC: Update Drop::drop (and/or the drop-time flush) so that when the drain fails it logs HOW MANY buffered deltas were retained/unwritten (e.g. `tracing::warn!(retained = n, ...)`), not just the error. The count must be observable from the buffer length after the failed drain.

TESTS: Add a unit test in the existing `#[cfg(test)] mod tests` block at the bottom of event_log.rs (it has access to private items like BufferedLine and drain_lines). Name the primary test `drain_retains_unwritten_deltas_on_write_failure`. Construct a mock `impl std::io::Write` that succeeds for the first k writes and returns Err(io::Error) afterward (a small struct counting writes). Build a buffer of N>k BufferedLine entries and call the drain helper; assert: (1) the helper returned Err, (2) exactly k lines were written to a captured sink in order, (3) the buffer still contains the remaining N-k lines in original order. Add a second assertion (same or sibling test) that calling the helper AGAIN with a now-working writer writes the retained lines successfully (recoverability). CONSTRAINTS: keep all existing tests in crates/engine/tests/event_log_test.rs and the in-file unit tests green; run `cargo test -p kranz-engine` and `cargo clippy -p kranz-engine --all-targets -- -D warnings` before finishing and include their output as evidence.

Done when:
- A unit test injects a writer that fails on the k-th write; it asserts lines 1..k-1 were written in order, the drain returned Err, and lines k..N remain in the buffer in original order.
- A subsequent drain with a working writer writes the retained lines successfully, proving the lost deltas are recoverable.
- Drop (or its drop-time flush) logs the count of retained/unwritten deltas on a drain failure, not merely that flushing failed.
- EventLog's public API is unchanged and all existing event_log tests remain green (cargo test -p kranz-engine).


## Milestone 2 — F2 — idle missions age-flush buffered deltas

### 2.1 Age-based flush API on EventLog with an idle-flush regression test

FILE: crates/engine/src/event_log.rs (Rust, kranz-engine crate). CONTEXT: EventLog buffers worker.message stream deltas in `self.buffer: Vec<BufferedLine>` where `BufferedLine { buffered_at: std::time::Instant, line: String }`, with a `self.throttle: Duration`. Today the only age check lives inside append() (~lines 231-236): a delta is buffered, and the buffer is drained only if `self.buffer.first().buffered_at.elapsed() >= self.throttle` — but this runs ONLY as a side effect of a LATER append() call. Finding F2 (docs/reviews/event-log-review.md): if a burst of deltas is followed by silence (idle mission waiting on an approval gate, worker stopped), the buffered lines sit unwritten indefinitely, bounded only by the next event or drop — contradicting the header's claim that deltas are 'drained by age (throttle)'.

REQUIRED ADDITIONS (public methods on EventLog):
1. `pub fn buffer_age(&self) -> Option<Duration>` — None when the buffer is empty, else the elapsed time since the OLDEST buffered delta (`self.buffer.first()`).
2. `pub fn flush_if_due(&mut self) -> Result<bool>` — if the buffer is non-empty AND `buffer_age() >= self.throttle`, drain the buffer to the file (reuse the existing drain path / drain_buffer; no fsync required, matching flush()) and return Ok(true); otherwise do nothing and return Ok(false). This gives callers (and truly-idle missions, once wired) a wall-clock-driven flush that does not depend on another append().

Update the module header doc (lines ~5-9) and append()'s doc if needed so the 'drained by age' wording is accurate: age draining now happens either on the next append() OR via flush_if_due().

TESTS (add to crates/engine/tests/event_log_test.rs, which uses the public API; a NEVER = 3600s throttle helper and a Duration::from_millis short throttle pattern already exist there — mirror `throttle_flushes_buffer_by_age`):
- `flush_if_due_drains_idle_buffer_by_age`: acquire with a short throttle (e.g. 30ms), append ONE delta, assert it is still buffered (read_events shows 0), sleep past the throttle (e.g. 60ms), then call `flush_if_due()` with NO intervening append/flush/drop; assert it returned true AND read_events now shows the delta on disk. This is the core F2 proof: an idle buffer ages out without another event.
- `buffer_age_reports_oldest_and_none_when_empty`: assert buffer_age() is None on a fresh log, Some(_) after appending a delta, and that flush_if_due() on a YOUNG buffer (long throttle) returns false and leaves the delta buffered.

CONSTRAINTS: do not alter existing method signatures; keep all existing tests green. Run `cargo test -p kranz-engine` and `cargo clippy -p kranz-engine --all-targets -- -D warnings` and include output as evidence.

Done when:
- flush_if_due_drains_idle_buffer_by_age: after appending one delta, sleeping past a short throttle, and calling flush_if_due() with no other call, the delta is present on disk and the method returned true.
- buffer_age() returns None for an empty buffer and Some(oldest-elapsed) after a delta is buffered.
- flush_if_due() on a buffer younger than the throttle returns false and leaves the delta buffered.
- All pre-existing event_log tests remain green.

### 2.2 Drive age-flush from the orchestrator poll loop

FILE: crates/engine/src/orchestrator.rs (Rust, kranz-engine crate). PREREQUISITE: EventLog now exposes `pub fn flush_if_due(&mut self) -> Result<bool>` (added in the sibling feature) which drains buffered stream deltas iff the oldest has exceeded the throttle. CONTEXT: The orchestrator owns the single `EventLog` as `self.log` (field around orchestrator.rs:246) and already calls `self.log.flush()` in several places (e.g. ~lines 551, 1839, 2341). It runs async loops with periodic polling — notably a run/pause monitoring loop using `PAUSE_POLL = Duration::from_millis(300)` (const ~line 71) and other `loop { ... tokio::time::sleep(...) }` sites (~lines 974, 1187, 2610). GOAL (finding F2 remediation, option: caller polls and flushes): ensure that while a mission is idle but the engine event loop is still ticking, buffered deltas are aged out to disk within roughly the throttle, rather than sitting until the next lifecycle event or drop.

TASK: Identify the orchestrator's primary wait/poll point(s) where the engine is otherwise idle (waiting on approval, waiting for a run, pause polling) and call `self.log.flush_if_due()?` (or log-and-continue on Err, matching how nearby best-effort flushes are handled) on each tick so an idle buffer is drained by age. Prefer the smallest, clearest change: add the call to the existing periodic poll tick(s); do NOT introduce a new background thread or timer. Keep the call cheap — flush_if_due is a no-op when nothing is due.

VERIFICATION: Explain in the WorkerReport exactly which loop/tick you wired it into and why that is the idle path. If an existing orchestrator integration test can be extended to assert that an idle buffered delta reaches disk within ~throttle, add it; otherwise state plainly that the behavior is covered by the event_log unit test plus manual reasoning about the wired loop, and ensure existing orchestrator tests stay green.

CONSTRAINTS: do not change EventLog. Run `cargo test -p kranz-engine` and `cargo clippy -p kranz-engine --all-targets -- -D warnings`; include output as evidence.

Done when:
- The orchestrator calls flush_if_due() on its periodic idle/poll tick so a buffered delta is aged out to disk without requiring a new lifecycle event.
- The WorkerReport names the specific loop/tick the call was wired into and justifies it as the idle path.
- No new background thread or timer is introduced; the change reuses an existing poll point.
- All existing orchestrator and engine tests remain green (cargo test -p kranz-engine).

