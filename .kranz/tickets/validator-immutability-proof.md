---
title: Prove validators cannot write: before/after assertions + drop wildcard Bash (P1)
priority: 2
schedule: once
---

## Goal
The "read-only validator" is nominal today: `writable: false` is not
enforced by the CLI, contract commands become `Bash(<cmd>*)` wildcard
prefixes (as loose as `python3 -*`), and the sandbox allows writes to the
session checkout. Make it structural: assert HEAD/index/worktree are
byte-identical before and after every validator session (a validator that
commits or edits fails the round), and drop the `python3 -*` wildcard
from command allow patterns. Engine-run contract commands (shipped) make
validator Bash unnecessary for the contract itself; scrutiny's split
(shipped) keeps it read-only by construction.

## Context
From the review (P1 #5). The out-of-contract sweep detects tampering
post-hoc; this turns detection into prevention per session.

## Acceptance hints
- A validator that writes/commits fails its round with a named violation.
- python3 -* is no longer in any validator allow-set; existing contracts
  using heredoc forms get a documented engine-run path.
- cargo test --workspace green.

## Resolution notes (shipped)

- **Identity assertion** (`crates/engine/src/validator_integrity.rs`, wired
  into `orchestrator.rs::validation_round` around BOTH the primary validator
  session and its one retry): capture `git rev-parse HEAD` +
  `git status --porcelain` before the spawn, re-capture after, any drift
  emits `validator.tamper` (milestoneId, runId, role, headBefore/headAfter,
  gained/lost porcelain entries) and blocks the milestone — no retry, no
  waivable finding. The assertion is "no tracked file changed, HEAD
  unchanged, index unchanged", plus no new non-ignored file (a dropped test
  file manufactures a pass as easily as an edit). Porcelain respects
  .gitignore, so gate artifact churn (`target/`, the gitignored `.kranz`
  engine runtime) never trips it.
- **Narrowed allow patterns** (`permissions.rs::command_allow_patterns`):
  only the verbatim contract command and its exact `&&`/`||`/`;`/`|`
  segments become `Bash(<form>*)` rules. The leading-two-token catch-alls
  (`python3 -*`, `python3 -m*`, `cargo test*` from `cargo test --workspace
  x`) are gone from every validator allow-set. Heredoc contract commands
  need no validator Bash rule: contract commands are executed engine-side
  (`validation_round`'s captured PASS/FAIL evidence, validator repair 3/5) —
  that engine-run path is the documented route for heredoc-shaped checks.

## Remaining gap (honest interim, not containment)

Validators CAN still write to the session checkout — the permission
patterns and sandbox do not make the tree read-only. What shipped is
detection with teeth: any write is caught before the round can pass and is
recorded in the append-only event log. The follow-up for true immutability
is an **immutable-snapshot validator**: run each validator in a
copy-on-write worktree (or read-only mount of the checkout) that is
discarded after the round, so writes are structurally impossible rather
than caught. The `writable: false` spec field is separately being made
fail-closed by the parallel backend-sandbox work.

## Follow-up shipped: copy-on-write immutable validator snapshot

The gap above is closed (`crates/engine/src/validator_snapshot.rs`, wired
into `orchestrator.rs::validation_round` around BOTH the primary validator
session and its single retry — the same sites the fingerprint wraps):

- **Snapshot**: before each validator session the engine builds a
  THROWAWAY detached worktree of the session checkout under the mission's
  gitignored `runs/` scratch (`validator-snapshot-<kind>`): HEAD plus the
  worker's uncommitted state (`git diff --binary HEAD` → `git apply`, and
  a byte copy of every untracked non-ignored file), plus a COPY of the
  real `target/` — APFS clonefile (`cp -c`) → Linux reflink
  (`cp --reflink=always`) → size-capped plain copy → fresh empty target
  (the cost is named in the log/event). The real `target/` is never
  shared or symlinked. Session cwd, the validator's contract-command cwd,
  and the sandbox profile's `session_cwd` all point at the snapshot, so
  where the sandbox can express it the real checkout is not writable at
  all. Only the verdict crosses back; the snapshot is discarded after the
  round regardless of outcome (RAII). Deliverable gates and the
  out-of-contract sweep keep running against the REAL checkout. Snapshot
  creation failure blocks the round honestly (fail-closed).
- **Fingerprint → tripwire** (`validator_integrity.rs`): the
  before/after identity assertion stays on the REAL checkout. With
  isolation in place it should never drift — a `validator.tamper` event
  now means the isolation itself failed (the one mutation class the
  snapshot cannot contain is shared git refs; worktrees share the common
  `.git`, and the tripwire's `for-each-ref` half covers it).
- **Event**: additive `validation.snapshot` (milestoneId, role, path,
  targetTier, creationMs, detail).
