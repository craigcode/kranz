# Correctness & durability review: `crates/engine/src/event_log.rs`

Scope: `crates/engine/src/event_log.rs` (825 lines), the append-only JSONL event
log that is the mission's single source of truth. Reviewed in full, together
with `crates/engine/src/events.rs` (`Event`, `EventKind::is_stream_delta`),
`crates/engine/src/error.rs` (`EngineError::LogCorruption` /
`EngineError::LockHeld`), and `crates/engine/src/paths.rs` (`MissionPaths`) for
context. This is a read-only review — no source, test, or config file was
changed. This document has been through an independent adversarial
verification pass: every finding below was re-derived from the source
(reading the code before re-reading the prior claim) and checked against its
own stated trigger; none were refuted. See the per-finding **Verification**
notes and the [Rejected during verification](#rejected-during-verification)
section.

The module's core design is sound: lifecycle events are fsynced per append,
stream deltas are buffered and reconciled with file order on the next
lifecycle write, torn tails are detected and repaired before the append handle
opens, and lock stealing is protected on unix by a `flock`-serialized
guard with an inode-identity recheck that closes the classic
unlink/recreate race. The most significant finding is a real bug, not a
theoretical one: a write failure partway through draining the in-memory
stream-delta buffer silently discards buffered events that were never
attempted to write, even though their original `append()` calls already
returned `Ok`. The remaining findings are narrower races and documentation/
guarantee mismatches, mostly Low-to-Medium.

## Findings table

| ID | Severity | Location | Summary |
|----|----------|----------|---------|
| F1 | High | event_log.rs:256-261 | `Vec::drain` semantics silently discard un-written buffered deltas on a mid-loop write failure, even though those events were already acknowledged `Ok` to their callers. |
| F2 | Medium | event_log.rs:231-236 | The "drained by age" throttle only fires as a side effect of a *later* `append()` call; an idle mission after a delta burst can leave buffered deltas un-fsynced indefinitely, contradicting the header's throttle claim. |
| F3 | Low | event_log.rs:144-153 | Mission-id validation on `acquire` inspects only the first parsed event; a mixed-mission or empty pre-existing log passes unnoticed. |
| F4 | Low | event_log.rs:640-643 | On non-unix platforms `probe_liveness` can never return `Dead` for a foreign pid, so the LockForce table's "Dead holders are always stolen, regardless of tier" guarantee is unreachable there — undocumented at the table itself. |
| F5 | Low | event_log.rs:710-727 | The macOS identity-token probe shells out to `ps` on every liveness check, adding latency and an external-binary dependency to the lock-steal hot path. |

## F1 — Buffer drain loses events on a mid-loop write error (High)

**Location:** event_log.rs:256-261 (`drain_buffer`), called from event_log.rs:238 (lifecycle `append`), event_log.rs:251 (`flush`), and event_log.rs:345 (`Drop::drop`).

**Severity:** High

**Description:** `drain_buffer` is:

```rust
fn drain_buffer(&mut self) -> Result<()> {
    for buffered in self.buffer.drain(..) {
        self.file.write_all(buffered.line.as_bytes())?;
    }
    Ok(())
}
```

`Vec::drain(..)` removes the whole requested range from `self.buffer`
regardless of how much of the iterator is actually consumed: if the loop body
returns early (here, via the `?` on `write_all`), the `Drain` guard's `Drop`
impl still discards every remaining not-yet-yielded element when the loop
exits. Concretely: if `self.buffer` holds N buffered `worker.message` lines
and `write_all` fails on line *k* (`ErrorKind::Other`, disk full, `EIO`,
etc.), lines `1..k-1` are already durably `write_all`'d to the fd, line *k*
may be partially written, and lines `k+1..N` are removed from
`self.buffer` and dropped **without ever being written anywhere** — the
function returns `Err`, but the caller has no way to recover or even
enumerate what was lost.

**Impact:** Every one of `drain_buffer`'s three callers loses information
about *which* buffered events vanished, and two of the three callers make
the loss effectively invisible:
- `append()` (event_log.rs:238) propagates the error to its own caller, but
  the events that vanished are not the event `append()` was processing —
  they are *earlier* `worker.message` events whose own `append()` calls
  already returned `Ok(event)` to their original callers. Those callers
  believe their events are durably queued; they are gone.
- `Drop::drop` (event_log.rs:344-347) only `tracing::warn!`s the error and
  continues; there is no caller left to propagate to. This is the most
  likely place for the bug to bite silently, e.g. during graceful shutdown
  under transient disk pressure.
- `flush()` (event_log.rs:250-254) propagates normally, but by the time the
  caller sees the error the un-yielded lines are already gone from
  `self.buffer`, so retrying `flush()` cannot recover them.

This is strictly worse than the module's stated durability contract ("losing
buffered deltas on a crash is recoverable" — header, event_log.rs:8): here
there is no crash at all, just an I/O error on one write, yet an arbitrary
suffix of the buffer disappears with no trace in the log and no signal to
the events' original producers.

**Trigger:**
1. Client calls `append(WorkerMessage {..})` three times in a row while
   under the throttle window; all three land in `self.buffer` and each call
   returns `Ok(event)` to its caller (seq N, N+1, N+2).
2. The underlying filesystem hits `ENOSPC` (disk full) or the fd receives
   `EIO`.
3. A fourth call — either another lifecycle `append()` (event_log.rs:238),
   an explicit `flush()`, or the log being dropped — invokes
   `drain_buffer()`. The first buffered line's `write_all` succeeds; the
   second's fails.
4. `drain_buffer` returns `Err`; `self.buffer` is now empty. The third
   buffered event (seq N+2) — already acknowledged to its producer — is
   gone: never written to `events.jsonl`, not recoverable from memory, not
   named in any error.

**Suggested remediation:** Do not use a `for … in self.buffer.drain(..)`
loop for a fallible per-item operation. Iterate by index (or use
`retain`/`split_off`) so that on a write failure the not-yet-attempted lines
stay in `self.buffer` instead of being dropped by the `Drain` guard —
e.g. write from the front and only remove the prefix that was actually
written (`self.buffer.drain(..written_count)`), leaving the remainder to be
retried by the next `drain_buffer` call. Callers (especially `Drop`) should
also log *how many* lines were lost/retained, not just that flushing failed.

**Verification:** Re-derived from `Vec::drain`'s documented contract rather
than taking the claim on faith: `drain(..)` sets `self.buffer`'s length to 0
immediately and yields items lazily from the guard's internal range; if that
guard is dropped before full consumption (here, via the `?` on `write_all`
at event_log.rs:258 returning early), every not-yet-yielded item is dropped
with it — never written, never restored to `self.buffer`. Confirmed the `?`
is the only exit from the loop body and that all three call sites
(event_log.rs:238, 251, 345) are reachable with a non-empty buffer and no
path to recover the lost suffix. The trigger, mechanism, and line references
hold exactly as stated; this finding survives unchanged.

## F2 — Throttle-based drain has no independent time source (Medium)

**Location:** event_log.rs:231-236 (throttle check inside `append`), header contract at event_log.rs:5-9.

**Severity:** Medium

**Description:** The header states stream deltas are "drained by age
(throttle), by the next lifecycle append, by explicit
[`EventLog::flush`], or on drop" (event_log.rs:6-8), and `append`'s own doc
comment repeats "Stream deltas are buffered and drained once the oldest
buffered delta exceeds the throttle age" (event_log.rs:219-220). In practice
there is no timer, background task, or scheduled wake-up anywhere in this
module: the throttle check

```rust
if event.kind.is_stream_delta() {
    self.buffer.push(BufferedLine { buffered_at: Instant::now(), line });
    let oldest = self.buffer.first().expect("just pushed").buffered_at;
    if oldest.elapsed() >= self.throttle {
        self.drain_buffer()?;
    }
}
```

only runs as a side effect of a *subsequent* call to `append()`. If a burst
of `worker.message` deltas is followed by silence (e.g. the mission is
waiting on a human approval gate, or the worker producing deltas simply
stops for longer than `throttle` with no further events of any kind), the
buffered lines sit in `self.buffer` — unwritten to the fd at all, not just
un-fsynced — for an unbounded time, bounded only by whenever the *next*
event (of any kind) happens to arrive, or the log is dropped.

**Impact:** The throttle is not the age-based upper bound on exposure the
documentation describes; it is an opportunistic check with no independent
enforcement. This is not full data loss on its own (a graceful process exit
still flushes via `Drop`, and `Drop` is best-effort-reliable absent F1's
error path), but it means the "throttle" configuration knob does not bound
buffered-delta staleness the way an operator reading the header comment
would expect, and combined with a crash (not just a graceful exit) during an
idle period, the entire buffered backlog — however old — is lost, with the
loss window potentially far exceeding `throttle`.

**Trigger:**
1. Acquire an `EventLog` with `throttle = Duration::from_secs(2)`.
2. Append one `worker.message` delta (buffered, not yet due for drain).
3. Do not call `append`, `flush`, or drop the log for an hour (e.g. the
   engine is blocked waiting on an external event that never triggers
   another `append`).
4. The buffered line is still sitting in `self.buffer`, never written to
   the fd, despite `throttle` having elapsed 1798 times over.
5. If the process is killed (not dropped) at this point, the delta is lost
   with a staleness far beyond `throttle`, contradicting the age-bound
   framing in the header.

**Suggested remediation:** Either (a) document plainly that the throttle
only bounds staleness *relative to the next event of any kind*, not
wall-clock time in isolation, and that callers needing a hard bound must
call `flush()` on their own timer; or (b) give `EventLog` an actual
time-based flush path (e.g. have the caller that owns the event loop poll
`buffer_age()` and call `flush()`, or spawn a lightweight ticker that calls
`flush()`.) — a prose-only suggestion, no code should change as part of
this review.

**Verification:** Read the whole module for any timer, background thread, or
scheduled callback that could drain the buffer independently of a caller
invoking `append`/`flush`/drop — found none; `Instant::now()` /
`.elapsed()` (event_log.rs:232, 234) are only ever evaluated synchronously
inside `append`. The header and doc-comment wording cited
(event_log.rs:6-8, 219-220) matches the source verbatim, and the throttle
check at event_log.rs:231-236 is confirmed to run only on the next
`append()` call, never on a wall-clock schedule. Finding survives unchanged.

## F3 — Mission-id guard only checks the first log line (Low)

**Location:** event_log.rs:144-153 (mission_id check in `acquire`), interacts with event_log.rs:317-325 (per-line seq validation, which has no matching per-line mission_id check) and `Event::mission_id` in events.rs.

**Severity:** Low

**Description:** `acquire` validates the mission the log belongs to by
inspecting only the *first* successfully parsed event:

```rust
if let Some(first) = parsed.events.first() {
    if first.mission_id != mission_id {
        return Err(EngineError::InvalidState(...));
    }
}
```

No other line's `mission_id` is ever checked, either here or in
`parse_log`'s per-line validation (event_log.rs:317-325), which only checks
`seq` continuity. Two gaps follow: (1) if `parsed.events` is empty — an
existing but entirely empty (or fully-torn, zero-valid-lines) log file —
the check is skipped altogether, so a stale/misplaced empty `events.jsonl`
from an unrelated mission is silently adopted for the new `mission_id`
with no warning; (2) if the first line happens to match but a later line's
`mission_id` differs (only reachable via external tampering, a
directory/path mixup upstream, or a latent bug in whatever assigns
`events_path` per mission, since the single-writer invariant otherwise
guarantees all lines share one `mission_id`), the mismatch is never
detected — the log is accepted and used as this mission's history.

**Impact:** Under the module's own single-writer discipline this is
unreachable in ordinary operation (every line in a given log is written by
one `EventLog` bound to one `mission_id`). It is a genuine gap only as a
defense-in-depth measure against an already-anomalous precondition (path
reuse across missions, manual file edits, filesystem-level corruption
outside the torn-tail case already handled). If it does trigger, the
symptom is silent cross-mission history mixing rather than a loud
`InvalidState`/`LogCorruption` error.

**Trigger:**
1. Construct (e.g. via a bug elsewhere, or manually for testing) an
   `events.jsonl` whose first line has `mission_id: "m-a"` and a later line
   has `mission_id: "m-b"`.
2. Call `EventLog::acquire(paths, "m-a", ...)`.
3. `parse_log` parses all lines successfully (seq continuity has nothing to
   say about `mission_id`); the mission-id check only looks at line 1, which
   matches; the log is accepted and `read_events`/`read_events_after` will
   happily replay the "m-b" line as part of mission "m-a"'s history.

**Suggested remediation:** Extend the per-line validation in `parse_log`
(alongside the existing seq check at event_log.rs:317-325) to also assert
every event's `mission_id` matches the first (or an expected) value,
turning a mismatch anywhere in the file into the same
`EngineError::LogCorruption` used for seq discontinuities, rather than only
checking line 1 from `acquire`.

**Verification:** Confirmed `parse_log` (event_log.rs:281-332) validates
only `seq` continuity per line (event_log.rs:317-325) with no `mission_id`
comparison anywhere in the loop, and that `acquire`'s own check
(event_log.rs:144-153) reads only `parsed.events.first()`, which is `None`
(and thus skipped) for an empty-events log. Both gaps are real as stated.
Severity is appropriately Low: the single-writer discipline (one `EventLog`
per `mission_id` ever appends to a given path) makes the trigger reachable
only via an already-anomalous precondition, not ordinary operation. Finding
survives unchanged.

## F4 — "Dead" liveness verdict is unreachable on non-unix builds (Low)

**Location:** event_log.rs:640-643 (`probe_liveness`, `#[cfg(not(unix))]` branch), event_log.rs:26-30 (LockForce table), event_log.rs:731-734 (`process_identity_token` non-unix stub).

**Severity:** Low

**Description:** The `LockForce` table at the top of the module (lines
26-30) states as an unqualified property: "A holder that is provably DEAD is
always stolen (a stale lock from a crashed engine), regardless of tier."
`probe_liveness` (event_log.rs:618-644), though, can only ever produce
`Dead` in two ways: a unix `kill(pid, 0)` returning `ESRCH`
(event_log.rs:636), or the own-pid token-reuse check in `alive_or_reused`
(event_log.rs:657-678) — which itself requires `process_identity_token` to
return `Some` on both readings, and the non-unix stub
(event_log.rs:731-734) always returns `None`. On any non-unix target, the
`#[cfg(not(unix))]` arm of `probe_liveness` (event_log.rs:640-643)
unconditionally returns `Unknown` for every foreign pid, and the own-pid
path degrades to `Alive` (via `alive_or_reused`'s `None`-token fallback at
event_log.rs:658-660). So `Dead` is provably unreachable there. This is
individually documented at the function level ("Non-unix → Unknown" —
event_log.rs:617), but the module-level table that operators are most
likely to read in isolation states the Dead-tier guarantee with no
platform caveat.

**Impact:** Not a correctness bug — `Unknown` is the safe, documented
fallback and every path that treats `Unknown` conservatively still behaves
correctly (`LockForce::No` refuses, `IfNotLive`/`EvenIfLive` steal). The
practical effect is purely operational: on a non-unix target, a crashed
engine's lock is *never* auto-recovered by a bare retry — every
crash-recovery there requires an explicit `--force-lock`, contradicting a
naive reading of the table's "regardless of tier" wording.

**Trigger:**
1. On a non-unix build, start an engine, let it crash without releasing
   `events.jsonl.lock` (simulating e.g. `SIGKILL`/an unhandled panic that
   skips `Drop`, or a hard process kill).
2. Immediately retry `EventLog::acquire(..., LockForce::No)`.
3. `authorize_steal` → `probe_liveness` → non-unix branch → `Unknown`, so
   the `(Unknown, LockForce::No)` arm of the match (event_log.rs:429-436)
   returns `LockHeld`, even though the holder is, in fact, dead — the
   caller must know to pass `--force-lock` (or stronger) for recovery to
   proceed, despite the table's phrasing suggesting Dead-holder recovery is
   tier-independent.

**Suggested remediation:** Add a one-line platform caveat to the LockForce
table's introduction (or to the "Dead" row) noting that automatic Dead
detection is unix-only (`kill(pid, 0)` + the token-reuse screen); on other
platforms, only the token-reuse screen for a reused *own* pid can produce
Dead, so recovery from a foreign crashed holder always requires an explicit
force tier.

**Verification:** Confirmed the `#[cfg(not(unix))]` arm of `probe_liveness`
(event_log.rs:640-643) is an unconditional `Unknown` with no path to `Dead`,
and that the only other route to `Dead` is `alive_or_reused`
(event_log.rs:657-678), which requires `process_identity_token` to return
`Some` — the non-unix stub (event_log.rs:731-734) always returns `None`, so
that route is closed too. Cross-checked the LockForce table text
(event_log.rs:26-30) against this and confirmed the "regardless of tier"
wording carries no platform caveat there, only in `probe_liveness`'s own doc
comment (event_log.rs:617). Not a correctness bug — `Unknown` is handled
conservatively everywhere it's consumed. Finding survives unchanged.

## F5 — macOS liveness probe depends on a `ps` subprocess (Low)

**Location:** event_log.rs:710-727 (`process_identity_token`, macOS).

**Severity:** Low

**Description:** On macOS, `process_identity_token` shells out to `ps -p
<pid> -o lstart=` for every reuse-screen check (event_log.rs:712-717), which
happens on every `authorize_steal`/`lock_holder_is_alive` call against a pid
that is currently alive (`alive_or_reused`, event_log.rs:657-678). This adds
subprocess-spawn latency to a path that may be called frequently (queue
busy checks, hygiene sweeps, per the doc comment at event_log.rs:541-542),
and introduces a dependency on an external binary being present, unmodified,
and fast — e.g. inside minimal containers/sandboxes without `/bin/ps`, or
under heavy system load where spawning is slow. The failure mode is safe
(`out.status.success()` false, or empty stdout, both map to `None` →
`alive_or_reused` falls back to `Alive`, event_log.rs:661-663), so this is a
robustness/performance concern, not a correctness one.

**Impact:** Slower or flakier lock-contention checks on macOS specifically;
in the worst case (a hung/missing `ps`) every alive-pid probe eats the
`Command::output()` timeout-free blocking wait with no cap, though it never
misclassifies a live process as dead.

**Trigger:** Run in an environment where `/bin/ps` is missing, sandboxed
away, or replaced by a slow/hung shim; every `authorize_steal` and
`lock_holder_is_alive` call against a live macOS pid now pays a subprocess
spawn (or blocks/hangs) instead of a syscall.

**Suggested remediation:** None required functionally; if the latency
matters in practice, a native `libc`/`sysctl` (`KERN_PROC_PID` / `p_starttime`
via `sysctl`) route would avoid the subprocess, at the cost of `unsafe`
FFI. Documenting the current tradeoff (simplicity over a syscall) is
sufficient if the maintainers consider it acceptable.

**Verification:** Confirmed `process_identity_token` on macOS
(event_log.rs:710-727) spawns `std::process::Command::new("ps")` with no
timeout, and that both failure modes (`!out.status.success()`, empty
stdout) map to `None` (event_log.rs:718-726), which `alive_or_reused`
(event_log.rs:661-663) treats as `Alive` — never `Dead`. So the failure mode
is confirmed safe (perf/robustness only, no correctness exposure). Finding
survives unchanged.

## Soundness notes

Invariants examined and found upheld:

- **fsync-per-lifecycle event.** Every lifecycle `append()` call performs
  `write_all` + `flush` + `sync_data` (event_log.rs:239-241) *after*
  draining any buffered deltas first, so file order matches append order and
  the fsync covers both the drained deltas and the new lifecycle line in one
  syscall (they share the same fd). Stream-delta-only appends never call
  `sync_data` (event_log.rs:231-236), matching the documented asymmetry.
- **Torn-tail tolerance is confined to the true final line.** `parse_log`
  (event_log.rs:281-332) only tolerates an unparseable line when
  `is_final` (event_log.rs:296, checked at 301); any unparseable line
  followed by more bytes is `LogCorruption` (event_log.rs:310-315). Since
  appends only ever extend the file, a torn write can only ever land at the
  true tail, so this correctly distinguishes "crash mid-write" from
  "corruption."
- **Torn-tail repair happens before the append handle opens.** `acquire`
  performs truncation/newline-repair (event_log.rs:159-171) strictly before
  `OpenOptions::new().append(true).create(true).open(...)`
  (event_log.rs:177), so a new append can never glue onto a partial line.
  Both repair branches call `sync_data()` (event_log.rs:164, 170) before the
  append handle is opened.
- **Seq continuity is a hard per-line check.** `expected = events.len() as
  u64 + 1` (event_log.rs:317-325) correctly requires seq to start at 1 and
  increase by exactly 1 with no gaps or duplicates, and an empty file
  trivially satisfies this by never entering the loop.
- **UTF-8 torn-tail safety.** `parse_log` operates on raw bytes and decodes
  each line with `String::from_utf8_lossy` (event_log.rs:297), so a torn
  write that splits a multi-byte character cannot make the whole log
  unreadable — confirmed by design comment at event_log.rs:278-280 and the
  code matching it.
- **Unix lock-steal serialization closes the classic unlink race.**
  `StealGuard::acquire` (event_log.rs:496-527) re-stats the guard path
  *after* winning the `flock` and compares `dev`+`ino` against the held fd's
  own metadata (event_log.rs:516-519); a mismatch (the path was unlinked and
  possibly recreated while waiting) retries on the fresh file rather than
  trusting a flock on a dead inode. Combined with the outer retry loop in
  `steal_lock` (event_log.rs:381-406, in particular the `NotFound` case at
  event_log.rs:398 and the `AlreadyExists` re-probe at event_log.rs:403),
  a rival's unguarded first `create_new` attempt slipping into the
  remove→create window is re-judged from scratch rather than surfaced as a
  raw I/O collision.
- **Pid-reuse screening never demotes Alive to Dead on uncertainty.**
  `alive_or_reused` (event_log.rs:657-678) returns `Alive` whenever either
  token is unobtainable (event_log.rs:658-663), only returning `Dead` on a
  positive token *mismatch*. Every platform-specific `process_identity_token`
  (linux: 691-703, macOS: 710-727, other: 731-734) returns `None` rather
  than a fabricated value on any failure, so the "uncertainty must never
  demote Alive to Dead" invariant (stated at event_log.rs:564-567) is upheld
  in every branch examined.
- **Linux `/proc/<pid>/stat` field indexing is comm-safe.** Indexing from
  `stat.rfind(')')` (event_log.rs:699) rather than a fixed offset correctly
  handles a `comm` field containing spaces or `)` characters before reading
  field 22 (`nth(19)` from the fields after the last `)`, i.e. field 3 as
  index 0) — verified arithmetic: field 22 − field 3 = index 19.
- **Reuse tokens never rely on wall-clock arithmetic.** The linux token
  (boot_id + monotonic tick count) and the macOS token (a `ps`-rendered
  spawn timestamp compared only for byte equality, never re-parsed as a
  clock) are both immune to NTP/manual clock steps, consistent with the
  stated rationale (event_log.rs:99-101, 646-656) and directly tested by
  `own_pid_reuse_is_decided_by_token_equality` (event_log.rs:808-824).
- **Lock file removal on a failed acquire does not strand the mission.**
  The `match open() { ... Err(e) => { remove_file(&lock_path); ... } }`
  pattern (event_log.rs:189-195) runs regardless of *which* step inside
  `open()` failed, so a failed parse/repair/open never leaves an orphaned
  lock file blocking all future acquires.
- **Concurrent lock-free readers only ever observe a torn tail, never a
  torn middle.** Because the writer's `file` handle is opened with
  `.append(true)` (event_log.rs:177) and all writes go through it, any
  partial write a concurrent `read_events`/`read_events_after` call
  (event_log.rs:271-273, 336-340) observes mid-write can only affect the
  file's current end, which `parse_log`'s final-line tolerance already
  handles gracefully.

## Minor / style

- event_log.rs:296-315 — a purely empty trailing line (e.g. a stray extra
  `\n` at end-of-file with no content before the next byte) is logged as
  `"dropping unparseable final event line (torn write)"`
  (event_log.rs:302-307) even though nothing was actually torn — it's just
  a blank line. The warning message could distinguish an empty final
  segment from a genuinely malformed one for a clearer operator-facing log.
- event_log.rs:124 — the `open` closure is a `FnMut` local named like a
  function but capturing `lock_file`, `paths`, and `mission_id` by
  reference/move; a short doc comment above it noting it exists purely to
  scope the "release lock on failure" `match` at event_log.rs:189 (rather
  than being a reusable helper) would help a reader who expects it to be
  hoisted to a real method.
- event_log.rs:592-596 — `LockInfo::token`'s doc comment says "third line"
  but doesn't cross-reference that `read_lock_info` (event_log.rs:601-610)
  tolerates a *present-but-empty* third line as `None` too
  (event_log.rs:608); worth a one-clause mention since it's easy to miss
  when reading `LockInfo` in isolation.

## Rejected during verification

None. All five findings (F1-F5) and every soundness note were independently
re-derived against the code — reading `event_log.rs`, `events.rs`,
`error.rs`, and `paths.rs` fresh before re-reading the prior claims — and
each finding's stated trigger sequence was traced through the actual control
flow rather than accepted on the strength of its prose. No finding was
refuted, no line reference was found out of range or misattributed, and no
severity was found clearly mis-scaled. The principal surfaces enumerated for
this pass (append/fsync asymmetry & delta-loss window, torn-tail repair, seq
validation, lock-steal concurrency & the `StealGuard` unlink race, pid-reuse
token screen) are each covered, either by a surviving finding (F1, F2, F3)
or by a soundness note recording why no bug was found (torn-tail repair, seq
continuity, `StealGuard` unlink-race closure, pid-reuse screening) — no
additional Critical or High issue was discovered among them.

## Verification

No source file was modified; the only file created or edited by this review
is `docs/reviews/event-log-review.md`. `git diff --name-only main` output is
quoted verbatim in the worker report.
