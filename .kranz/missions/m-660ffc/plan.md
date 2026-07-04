# Mission plan — m-660ffc

**Goal:** Produce a rigorous, verified correctness-and-durability review of crates/engine/src/event_log.rs in docs/reviews/event-log-review.md, with file:line references and severities, changing no source.

Branch `kranz/mission-m-660ffc` (from `main`). Approved plan of record; the machine-readable twin is [plan.json](plan.json). Live status: `kranz status` or the dashboard.

## Validation contract

Defined before any feature; gates mission completion.

- **[a1]** The review document exists at docs/reviews/event-log-review.md. 
  `test -f docs/reviews/event-log-review.md`
- **[a2]** The mission is read-only: the only file changed relative to main is the review document; no source under crates/ (or anywhere else) is modified. 
  `[ "$(git diff --name-only main)" = "docs/reviews/event-log-review.md" ]`
- **[a3]** The document carries concrete code references in event_log.rs:LINE form. 
  `grep -qE 'event_log\.rs:[0-9]+' docs/reviews/event-log-review.md`
- **[a4]** The document uses the Critical/High/Medium/Low severity vocabulary. 
  `grep -qiE 'critical|high|medium|low' docs/reviews/event-log-review.md`
- **[a5]** Every file:line reference in the document is in range (event_log.rs is 825 lines) and names the construct the finding actually discusses; no dangling or mis-pointed citations. *(agent judgement)*
- **[a6]** Every substantive finding includes a concrete trigger/repro sketch, and the described failure is genuinely reachable in the code as written — no fabricated or purely-hypothetical bugs survive. *(agent judgement)*
- **[a7]** The audit covers the module's principal durability/correctness surfaces — per-append fsync asymmetry and the buffered-delta loss window (append/flush/drain_buffer/drop), torn-tail detection and repair (acquire, parse_log), seq-continuity validation, lock-steal concurrency and the StealGuard unlink race, and the pid-reuse identity-token screen — either flagging a risk or explicitly recording the invariant as sound. *(agent judgement)*

## Milestone 1 — Durability & correctness review drafted

### 1.1 Author the event-log review document

You are reviewing a single Rust module for correctness and durability risks and writing your findings to a NEW markdown file. This is a READ-ONLY review: you MUST NOT modify any source file, add or change tests, or run any state-mutating git command. The ONLY file you create or edit is docs/reviews/event-log-review.md (create the docs/reviews/ directory if needed).

Target file: crates/engine/src/event_log.rs (825 lines) — an append-only JSONL event log that is the mission's single source of truth. Read it in full. Key collaborators you may read for context and may cite when a risk genuinely spans them: crates/engine/src/events.rs (Event struct, EventKind, EventKind::is_stream_delta ~line 241), crates/engine/src/error.rs (EngineError::LogCorruption, EngineError::LockHeld), crates/engine/src/paths.rs (MissionPaths). Primary references must point at event_log.rs; cite a collaborator only when the risk truly crosses the boundary.

What to examine (reason about the actual code, do not merely tick these off):
- Append durability asymmetry: append() fsyncs lifecycle events (write_all+flush+sync_data, ~lines 239-241) but buffers stream deltas; flush()/drain_buffer() write without fsync (~lines 250-261); Drop flushes without fsync (~lines 343-357). Characterize exactly what is lost on a crash and whether that matches the module's stated contract (header comment lines 1-9).
- Buffered-delta throttle logic (~lines 231-236): is the oldest-buffered-at elapsed check correct? Can deltas sit unflushed indefinitely if no further append arrives? Interaction with lifecycle drain ordering (file order == append order).
- Torn-tail handling: parse_log (~lines 281-332) computes valid_len/terminated; acquire (~lines 154-175) repairs by truncation or newline-termination before opening the append handle. Look for gaps: partial-repair durability, the mission_id mismatch check inspecting only the FIRST event, empty-log and single-torn-line edge cases.
- Seq-continuity validation (~lines 317-325): off-by-one, duplicate/gap detection, empty-file behavior.
- Lock acquisition and stealing: acquire (~lines 104-196), steal_lock (~lines 377-407), authorize_steal (~lines 412-474), the LockForce matrix (~lines 20-45). Verify the concurrency claims in the doc comments are actually upheld by the code.
- StealGuard flock serialization and the unlink/inode-reuse race (~lines 487-537): does the dev+ino recheck close the race it claims to?
- pid-reuse identity-token screen: probe_liveness (~lines 618-644), alive_or_reused (~lines 657-678), process_identity_token per-platform (linux ~691-703, macos ~710-727, other ~731-734). Consider the macOS `ps` subprocess dependency, the linux /proc parsing (field indexing after the last ')'), and the 'uncertainty must never demote Alive to Dead' invariant.
- Error/cleanup paths: lock removal on failed open (~lines 189-195) and on drop (~lines 348-356); any path that could strand a lock or leave two writers.

Document structure (required):
1. A one-paragraph summary and a findings table (ID | Severity | Location | One-line summary).
2. One section per finding, each with: **Location** (event_log.rs:LINE, plus collaborator refs if relevant), **Severity** (exactly one of Critical / High / Medium / Low), **Description**, **Impact**, **Trigger** (a concrete step-by-step sequence — crash point, concurrent acquire, clock step, etc. — that manifests the issue), and **Suggested remediation** (prose only; make NO code change).
3. A 'Soundness notes' section listing invariants you checked and found upheld (e.g. fsync-per-lifecycle, torn-tail repair, token-based reuse screen), so the doc reads as a full audit rather than an open-ended bug list.
4. An optional 'Minor / style' appendix for nits, clearly separated from correctness/durability findings.

Severity guidance: Critical = silent data loss or two-writers-on-one-log corruption reachable under normal operation; High = data loss/corruption under a plausible crash/race; Medium = recoverable degradation or a narrow race; Low = robustness/clarity. Prefer precision over volume — a real Medium beats a speculative Critical.

When done, your WorkerReport must state that no source files were modified and quote the output of `git diff --name-only main` (it must list only docs/reviews/event-log-review.md).

Done when:
- docs/reviews/event-log-review.md exists and `git diff --name-only main` lists that file and nothing else (no source under crates/ modified, no tests added).
- The document contains a findings table and one detailed section per finding, each with a Location (event_log.rs:LINE form), a Severity of exactly Critical/High/Medium/Low, a Trigger sketch, and a Suggested remediation.
- Every cited event_log.rs line number is within 1..=825 and names the construct the finding discusses.
- A 'Soundness notes' section records invariants checked and found upheld.
- The review addresses, at minimum, the fsync/buffered-delta durability asymmetry, torn-tail detection/repair, seq-continuity validation, lock-steal concurrency including the StealGuard unlink race, and the pid-reuse identity-token screen.


## Milestone 2 — Findings independently verified and document finalized

### 2.1 Adversarially verify and finalize the review

A prior review of crates/engine/src/event_log.rs already exists at docs/reviews/event-log-review.md. Your job is independent adversarial verification and finalization. This remains a READ-ONLY review: do NOT modify any source file, add/change tests, or run state-mutating git commands. The ONLY file you may edit is docs/reviews/event-log-review.md.

Process:
1. Read crates/engine/src/event_log.rs (825 lines) in full and, where cited, its collaborators (events.rs, error.rs, paths.rs). Do this BEFORE reading the existing findings, so you form your own view.
2. For EACH finding, attempt to REFUTE it against the actual code. Default to skepticism: a finding survives only if its stated Trigger sequence genuinely produces the claimed failure given the code as written. Check specifically: (a) the file:line reference is in range and points at the construct described; (b) the mechanism is real, not a misreading of the control flow (e.g. verify claims about fsync ordering, the throttle elapsed-check, the StealGuard dev/ino recheck, the token equality screen, the seq/off-by-one logic against the exact lines).
3. Correct the document in place: fix inaccurate or out-of-range line references; rewrite or DELETE any finding you can refute (when deleting, leave a one-line note in a 'Rejected during verification' subsection stating the finding and why it does not hold); adjust severities that are clearly mis-scaled.
4. Add any Critical or High surface the first pass missed among the principal areas (append/fsync asymmetry & delta-loss window, torn-tail repair, seq validation, lock-steal concurrency & StealGuard unlink race, pid-reuse token screen). Do not pad with speculative low-value findings.
5. Mark each surviving finding as verified by appending a short **Verification** note (one or two sentences) explaining how you confirmed it against the code.

When done, your WorkerReport must summarize which findings were confirmed, corrected, and rejected, state that no source files were modified, and quote `git diff --name-only main` (it must list only docs/reviews/event-log-review.md).

Done when:
- `git diff --name-only main` lists only docs/reviews/event-log-review.md; no source or test files changed.
- Every surviving finding has a Verification note and a file:line reference that is in range (1..=825 for event_log.rs) and points at the construct it describes.
- No surviving finding can be refuted by its own stated Trigger sequence against the code; any rejected finding is recorded with a reason in a 'Rejected during verification' subsection.
- The final document still covers the principal durability/correctness surfaces enumerated in the spec, either as a finding or as a soundness note.

