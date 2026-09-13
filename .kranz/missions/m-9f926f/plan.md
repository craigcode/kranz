# Mission plan — m-9f926f

**Goal:** Close review findings F3, F4, and F5 in crates/engine/src/event_log.rs — validate mission identity across every parsed log event, document-and-enforce the non-unix liveness limitation at the LockForce table, and cache the macOS ps identity-token probe so repeated liveness checks spawn at most one ps per pid.

Branch `kranz/mission-m-9f926f` (from `main`). Approved plan of record; the machine-readable twin is [plan.json](plan.json). Live status: `kranz status` or the dashboard.

## Validation contract

Defined before any feature; gates mission completion.

- **[a1]** The full engine test suite (including all new per-finding regression tests) passes. 
  `cargo test -p kranz-engine`
- **[a2]** Clippy is clean across all targets with warnings treated as errors. 
  `cargo clippy -p kranz-engine --all-targets -- -D warnings`
- **[a3]** No files are changed outside crates/engine/src/event_log.rs and docs/reviews/event-log-review.md, measured against the base commit pinned at plan approval. 
  `test -z "$(git diff --name-only $KRANZ_BASE_SHA -- ':!crates/engine/src/event_log.rs' ':!docs/reviews/event-log-review.md')"`
- **[a4]** The edit to the review document is append-only against the base commit: no pre-existing content line is removed. 
  `test -z "$(git diff $KRANZ_BASE_SHA -- docs/reviews/event-log-review.md | grep '^-[^-]')"`
- **[a5]** Acquiring (or parsing) a log whose Nth event (N>1) carries a mission_id different from the first event's is refused with EngineError::LogCorruption, detected during parse_log's single existing walk of the file (no second read); a dedicated regression test encodes this exact scenario and passes. *(agent judgement)*
- **[a6]** The behavior for an existing-but-empty (zero-valid-event) pre-existing events.jsonl is explicitly pinned by a passing test — it is adopted as a fresh log (seq resumes at 1, no error). *(agent judgement)*
- **[a7]** The LockForce decision table's documentation states that automatic Dead detection is unix-only (kill(pid,0) plus the own-pid token-reuse screen), so recovery from a foreign crashed holder on non-unix always requires an explicit force tier; the non-unix arm of probe_liveness emits a debug-level log recording that Dead is unreachable there and continues to return Unknown (never Dead — the 'uncertainty never demotes to Dead' invariant is preserved, and no steal behavior changes). *(agent judgement)*
- **[a8]** A test (cross-platform helper or documented cfg reasoning) pins that the non-unix liveness fallback can never yield Dead. *(agent judgement)*
- **[a9]** On macOS, repeated identity-token / liveness checks for a single pid spawn at most one ps subprocess within the cache window, proven by a cfg(target_os = "macos") test that observes an explicit spawn-count seam; and the cache is keyed by pid with the recorded-vs-current token comparison never skipped, proven by a test showing a differing recorded token still yields Dead (a stale/cached token never lets a reused pid be mistaken for the original holder). *(agent judgement)*
- **[a10]** docs/reviews/event-log-review.md carries a short appended 'Remediated' note for each of F3, F4, and F5 describing the fix and referencing the commit that closed it. *(agent judgement)*

## Milestone 1 — F3 — mission identity validated across every parsed event

### 1.1 Validate every event's mission_id in parse_log's single pass

Close finding F3 in crates/engine/src/event_log.rs. Read the F3 section of docs/reviews/event-log-review.md in full first (its Description, Trigger, and Suggested remediation).

Problem: `EventLog::acquire` validates mission identity by inspecting only the FIRST parsed event (currently around event_log.rs:162-171: `if let Some(first) = parsed.events.first() { if first.mission_id != mission_id { return Err(InvalidState(...)) } }`). `parse_log` (around event_log.rs:321-372) walks every line but only checks `seq` continuity — it never compares `mission_id`. So a log whose first line matches but a LATER line carries a foreign mission_id (external tampering, a path mixup, copy-pasted tail of another mission's log) is accepted and replayed as this mission's history.

Required change — keep parse_log a SINGLE pass (no second read of the file):
1. In `parse_log`'s existing `while offset < bytes.len()` loop, after each event is parsed and its seq validated, assert its `mission_id` equals the mission_id of the FIRST successfully parsed event. On the first event, record its mission_id; on every subsequent event, a mismatch returns `EngineError::LogCorruption` with a message naming the path, the line number, the expected (first) mission_id, and the found one — mirroring the existing seq-discontinuity LogCorruption arm. This makes parse_log validate internal mission-identity consistency for ALL its callers (read_events / read_events_after included), which do not know an expected mission_id — hence 'matches the first event', not 'matches an externally-supplied id'.
2. Leave `acquire`'s existing first-event-vs-expected check in place and unchanged (it stays `EngineError::InvalidState` — it answers a different question: does this log belong to the mission we were asked to open). The Nth-event-foreign case is caught earlier by parse_log as LogCorruption before that check is reached.
3. Empty pre-existing log: preserve current behavior — an existing events.jsonl with zero valid events is adopted as a fresh log (last_seq 0, next_seq 1, NO error). Do not change this; just pin it with a test.

Do NOT touch the F1/F2 machinery (drain_lines, flush_if_due), the lock-steal path, or liveness probing — those are out of scope for this feature.

Regression tests (add to the in-file `#[cfg(test)] mod tests`, using tempfile as the existing tests do; build valid JSONL event lines with the real Event/EventKind types and serde_json so seq continuity holds):
- A test named `acquire_rejects_foreign_mission_id_in_later_event`: write an events.jsonl whose event at seq 1 has mission_id "m-a" and event at seq 2 (or later) has mission_id "m-b", with correct seq continuity so the ONLY defect is the mission_id mismatch; assert `EventLog::acquire(paths, "m-a", ...)` returns an error and that it is `EngineError::LogCorruption` (match the variant). Prove the mismatch is what triggers it (a same-mission control log with identical seqs acquires cleanly).
- A test named `parse_log_rejects_mixed_mission_ids` (or exercised through read_events) proving parse_log itself flags the mismatch as LogCorruption, independent of acquire.
- A test named `acquire_adopts_empty_preexisting_log_as_fresh`: create an existing but empty events.jsonl, acquire for some mission id, assert it succeeds and `last_seq()` is 0 / the next append lands at seq 1.

After the code and tests are green, append (do not rewrite) a short 'Remediated (F3)' note to docs/reviews/event-log-review.md — one short paragraph immediately after the F3 section (or in a dedicated 'Remediated' subsection at the end) stating that parse_log now asserts per-line mission_id consistency (LogCorruption on mismatch) and that empty-log adoption is pinned, referencing this feature's commit. Do not edit any earlier prose.

Verify before reporting: `cargo test -p kranz-engine` green, `cargo clippy -p kranz-engine --all-targets -- -D warnings` clean. Report the actual test-runner output for the new tests as testEvidence.

Done when:
- A log whose Nth event (N>1) has a mission_id differing from the first event's is rejected with EngineError::LogCorruption, produced inside parse_log's single existing walk (no second file read).
- acquire's first-event-vs-expected mismatch still returns EngineError::InvalidState (unchanged).
- An existing-but-empty events.jsonl is adopted as a fresh log with no error and next seq == 1, pinned by a named test.
- cargo test -p kranz-engine is green and cargo clippy -p kranz-engine --all-targets -- -D warnings is clean.
- docs/reviews/event-log-review.md gains an appended 'Remediated (F3)' note and no earlier line is removed or rewritten.


## Milestone 2 — F4 — non-unix liveness limitation documented and enforced

### 2.1 Document unix-only Dead detection at the LockForce table and log the non-unix fallback

Close finding F4 in crates/engine/src/event_log.rs. Read the F4 section of docs/reviews/event-log-review.md in full first (Description, Impact, Trigger, Suggested remediation).

Problem: the `LockForce` doc-comment table near the top of the file (currently around event_log.rs:21-31) states as an unqualified property: 'A holder that is provably DEAD is always stolen ... regardless of tier.' But `probe_liveness` can only ever return `Dead` on unix (a `kill(pid,0)` ESRCH, or the own-pid token-reuse screen). On non-unix, probe_liveness's arm (currently around event_log.rs:684-687) unconditionally returns `Unknown` for a foreign pid, and process_identity_token's non-unix stub always returns None, so `Dead` is unreachable there. The table gives no platform caveat, so an operator reads 'dead holders auto-recover regardless of tier' and is surprised when non-unix crash recovery demands an explicit --force-lock.

Behavior must NOT change. Specifically: the non-unix arm must keep returning `Unknown` — do NOT change it to `Alive`, because Unknown + LockForce::IfNotLive currently STEALS (force works) whereas Alive + IfNotLive would REFUSE, silently breaking non-unix crash recovery. Preserve the invariant that uncertainty never demotes to Dead.

Required changes:
1. Amend the LockForce table's documentation (its intro sentence or the Dead row) with a one-line platform caveat: automatic Dead detection is unix-only (kill(pid,0) ESRCH plus the own-pid token-reuse screen); on non-unix targets only the own reused-pid token screen can ever yield Dead, so recovering the lock from a foreign crashed holder there always requires an explicit force tier (--force-lock / --dangerously-steal-live-lock).
2. In probe_liveness's `#[cfg(not(unix))]` arm, emit a `tracing::debug!` (not warn) noting that liveness cannot be proven on this platform so the holder is reported Unknown and Dead is unreachable here — then return `Unknown` as today.
3. Factor the non-unix fallback verdict into a tiny helper (e.g. `fn non_unix_liveness_fallback() -> LockLiveness { LockLiveness::Unknown }`) compiled on ALL platforms, called from the non-unix arm, so a cross-platform test can pin it. Add a `#[cfg(not(unix))]` inline doc/comment near the arm explaining the reasoning. Keep the change minimal — no new dependencies, no signature changes to probe_liveness/authorize_steal/lock_holder_is_alive.

Do NOT touch F3 (parse_log/mission_id) or F5 (macOS ps caching).

Test: add a test to the in-file `#[cfg(test)] mod tests` named `non_unix_liveness_fallback_is_never_dead` asserting the helper returns `LockLiveness::Unknown` and specifically not `LockLiveness::Dead`. This is a cross-platform test (runs on our macOS/Linux CI) that pins the invariant behind the cfg arm; it is acceptable in place of a cfg(not(unix))-only test that CI cannot execute. In a comment on the test, note this cfg reasoning explicitly so a reviewer understands why the assertion lives in a cross-platform helper.

After green, append a short 'Remediated (F4)' note to docs/reviews/event-log-review.md (append-only, after the F4 section or in the shared Remediated subsection) stating the table now carries the unix-only caveat and the non-unix arm logs at debug + returns Unknown unchanged, referencing this commit.

Verify before reporting: `cargo test -p kranz-engine` green, `cargo clippy -p kranz-engine --all-targets -- -D warnings` clean. Report test-runner output as testEvidence and quote the new table caveat text in the report.

Done when:
- The LockForce table documentation states automatic Dead detection is unix-only and that non-unix recovery from a foreign crashed holder requires an explicit force tier.
- probe_liveness's non-unix arm emits a tracing::debug! about Dead being unreachable and still returns LockLiveness::Unknown (not Alive, not Dead) — no steal behavior changes on any platform.
- A cross-platform test pins that the non-unix liveness fallback returns Unknown and never Dead.
- cargo test -p kranz-engine is green and cargo clippy -p kranz-engine --all-targets -- -D warnings is clean.
- docs/reviews/event-log-review.md gains an appended 'Remediated (F4)' note and no earlier line is removed or rewritten.


## Milestone 3 — F5 — macOS ps subprocess removed from the lock-steal hot path via a per-pid cache

### 3.1 Cache the macOS identity-token probe behind an observable spawn seam

Close finding F5 in crates/engine/src/event_log.rs. Read the F5 section of docs/reviews/event-log-review.md in full first (Description, Impact, Trigger, Suggested remediation), plus the F5 note in the acceptance hints of the mission goal.

Problem: on macOS, `process_identity_token` (currently around event_log.rs:754-771) shells out to `ps -p <pid> -o lstart=` on EVERY call. It is called from `alive_or_reused` on every authorize_steal / lock_holder_is_alive check against a live pid (queue busy checks, hygiene sweeps), so a single steal decision or a burst of liveness checks spawns many `ps` processes.

Required change — introduce a cache so repeated checks for one pid spawn at most one `ps`, WITHOUT weakening the pid-reuse screen:
1. Introduce a spawn SEAM: rename the current macOS body that actually runs `ps` to an inner function (e.g. `fn ps_identity_token(pid: i32) -> Option<String>`) and have `process_identity_token(pid)` be a thin cache layer that calls it. The seam must let a test observe how many times the real `ps` spawn happened — e.g. an `AtomicUsize` spawn counter incremented inside `ps_identity_token` (a `#[cfg(test)]` counter is fine, or an always-present counter with a test-only reader). Keep the seam macOS-scoped; linux and the other-platforms stub keep their current behavior (linux reads /proc and need not be cached; do not add caching there).
2. Cache design — pick the SIMPLEST option that keeps the pid-reuse screen sound. A per-process cache keyed by pid, storing (token, captured_instant), with a short TTL, or a per-acquire memoization — your choice. Non-negotiable soundness rules: (a) key the cache by pid; a lookup for a different pid must never return another pid's token; (b) the token stored is the raw ps output — `alive_or_reused` must STILL perform `current == recorded` against the (possibly cached) current token; never cache or short-circuit the Alive/Dead VERDICT itself; (c) a stale cache entry must never let a REUSED pid be judged the original holder — keep the TTL short (sub-second is ample; the cache only needs to collapse the repeated checks within one steal decision / burst) so a cached token is only reused within a window far shorter than any realistic pid-reuse turnaround, and document this reasoning in a comment. Caching a `None` result is acceptable (None → Alive is the conservative fallback and never yields a false Dead) but is not required; if you cache None, it can only ever keep the verdict Alive, never produce a false Dead. Use std-only synchronization (Mutex / OnceLock); add no new dependencies.
3. Thread-safety: liveness checks can run concurrently; the cache must be safe under concurrent access.

Existing tests that must keep passing unchanged: `identity_token_is_stable_for_a_live_process`, `identity_token_of_a_dead_pid_is_none`, `own_pid_reuse_is_decided_by_token_equality`. A cache that returns identical tokens for the same live pid keeps all three green — verify it does.

New tests (gate with `#[cfg(target_os = "macos")]` since they exercise the real ps path; our CI host is macOS):
- `macos_identity_token_caches_one_ps_per_pid`: reset/read the spawn counter, call `process_identity_token(std::process::id() as i32)` twice in quick succession, assert the real `ps` spawn count increased by exactly 1 and both returned tokens are equal.
- `macos_cache_never_masks_pid_reuse`: prove the cache never short-circuits the recorded-vs-current comparison — construct a LockInfo whose recorded token differs from the current (cached) token for a live pid and assert `probe_liveness`/`alive_or_reused` still returns `LockLiveness::Dead` (i.e. the cache supplies `current`, but a differing `recorded` still yields Dead). Mirror the style of the existing `own_pid_reuse_is_decided_by_token_equality` test.

Do NOT touch F3 (parse_log/mission_id) or F4 (LockForce table / non-unix arm).

After green, append a short 'Remediated (F5)' note to docs/reviews/event-log-review.md (append-only) describing the cache (keying by pid, short TTL / per-acquire memoization, comparison never skipped) and the spawn seam, referencing this commit.

Verify before reporting: `cargo test -p kranz-engine` green (the macOS-gated tests run on the host), `cargo clippy -p kranz-engine --all-targets -- -D warnings` clean. Report the actual test-runner output including the two new macOS tests as testEvidence.

Done when:
- process_identity_token on macOS is backed by a pid-keyed cache in front of an observable ps-spawn seam; repeated checks for one pid within the cache window spawn at most one ps, proven by a cfg(target_os="macos") test reading a spawn counter.
- alive_or_reused still compares recorded vs current token on every call; a differing recorded token yields LockLiveness::Dead even when the current token is served from cache, proven by a test.
- The cache is keyed by pid and its window is short enough (documented) that a reused pid can never be judged the original holder; no new dependencies, std-only synchronization, concurrency-safe.
- The three pre-existing identity-token/reuse tests still pass unchanged, and cargo test -p kranz-engine plus cargo clippy -p kranz-engine --all-targets -- -D warnings are green/clean.
- docs/reviews/event-log-review.md gains an appended 'Remediated (F5)' note and no earlier line is removed or rewritten.

