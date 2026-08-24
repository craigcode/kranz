---
title: Inviolable invariants
owner: agent
freshness: check-on-touch
last_verified: 2026-08-24
verified_against:
  - AGENTS.md
  - docs/design.md
  - crates/engine/src/git_ops.rs
  - crates/engine/src/event_log.rs
  - crates/engine/src/orchestrator.rs
  - crates/engine/src/merge.rs
  - crates/engine/src/merge_gate.rs
  - crates/engine/src/sandbox.rs
  - crates/cli/src/exec.rs
  - crates/engine/src/scrub.rs
  - crates/engine/src/types.rs
---

## What this is

The load-bearing invariants a change in kranz must never break, each with WHY
and where it is enforced. These are not style preferences; several have a scar
behind them (named incidents, respawns, a shipped-broken push). Break one and
you can corrupt a mission log, ship an empty deliverable, or push to a remote
kranz is never supposed to touch. If a change seems to require violating one,
that is a `contractChangeRequest`, not an edit.

## kranz never pushes (local default)

On a local host the engine only writes the working tree and **local** refs: it
advances the mission branch, tags milestones, and merges `--no-ff` into the base
branch locally. It never contacts a remote. The human runs `git push`.
WHY: git is the source of truth (plan §4.4) and a machine that can push can
ship unreviewed work; a piped gate once masked a build failure and shipped a
broken push (AGENTS.md rule 2, rule 4).

The **sole** exception is the explicit, ref-restricted `kranz exec --push
<REMOTE>` cloud handoff: `push_mission_branch` is the one and only push path,
and its sole non-test caller is [exec.rs](../../../crates/cli/src/exec.rs)
`cmd_exec`, gated to a `MissionStatus::Complete` run. The guard in
[git_ops.rs](../../../crates/engine/src/git_ops.rs) `push_mission_branch`
refuses anything that is not a `kranz/*` ref, and rejects a `:`, leading `-`,
or whitespace (no `--force`, no `src:dst` refspec, no `main`, no merge) **before
any git process runs**. [merge.rs](../../../crates/engine/src/merge.rs) states
in its module doc that the gated merge never calls push — the base branch is
only ever advanced locally.

## base_sha is pinned at approval and never re-resolved

At plan approval `approve_plan` resolves the base branch tip exactly once
(`self.repo.rev_parse(&base)`) and records it on the `plan.approved` event; it
lands on `mission.base_sha` and is exported to worker/validator sessions as
`KRANZ_BASE_SHA`. Every later diff (final gate, gated merge, out-of-contract
sweep) uses the pinned sha, never the moving branch name.
WHY: incident m-660ffc — a contract's `git diff main` assertion raced a commit
landing on the base branch mid-mission (design.md deviation 6). Re-resolving the
base anywhere after approval reintroduces that race.

## The event log is append-only, single-writer, redact-at-write

One engine process owns `events.jsonl` at a time via a lock file
([event_log.rs](../../../crates/engine/src/event_log.rs) `EventLog::acquire`).
Seq starts at 1 and increases by exactly 1 — any gap/duplicate is
`LogCorruption`; the append handle is opened `append(true)`; a torn final line
from a crash is repaired before the next append. Every payload is scrubbed on
the way in: `append_redacting` runs `scrub::scrub_json_value` and emits a
`secret.redacted` audit event immediately after any redaction (fingerprints
only, never values).
WHY: two writers on one log corrupt it, so the liveness probe carries a hard
rule — **anything uncertain must NEVER report Dead** (a false Dead lets two
engines write one log). Stealing a provably-live lock needs
`--dangerously-steal-live-lock` (design.md "Cross-process control").

## A mission must deliver (empty-deliverable gate)

Before any contract assertion, `final_gate`
([orchestrator.rs](../../../crates/engine/src/orchestrator.rs)) counts non-meta
commits in `base_sha..HEAD`; if that count is 0 the mission terminates
`Failed` with an explicit reason. A green contract can never override this — it
runs first. WHY: a run that produced nothing must fail honestly, never COMPLETE
on an empty diff (AGENTS.md rule 8). Do not defeat this gate.

## Worktree isolation: the primary checkout stays byte-untouched

In worktree mode, mission-branch git ops run in a dedicated integration
worktree (`setup_mission_worktree`/`teardown_mission_worktree`), reached via
`active_repo()`/`active_root()`, never the primary tree. Deliverable twins
(plan.md, revised-plan.md, report.md) are written **untracked** into the primary
runtime dir for operator visibility, never committed there. The out-of-contract
sweep asserts the primary is still on `primary_branch_at_start` and
`is_clean_tracked` — a tracked write into the primary is a finding.
WHY: the primary checkout must survive a mission byte-untouched; never check out
the mission branch in, or commit to, the primary tree from run/approve/validation
(AGENTS.md rule 7).

## Full-workspace gates, bare exit codes, fmt before done

The regression suite gates the **whole workspace**, never `-p kranz-engine`
alone (a crate-scoped pass hides breakage in consumers). Read a gate's raw exit
code — never pipe it (`... | tail -1` once masked a failure). Run
`cargo fmt --all` before finishing; CI gates `cargo fmt --check`. The gated-merge
runner [merge_gate.rs](../../../crates/engine/src/merge_gate.rs) encodes the same
suite in order — `cargo fmt --all --check`, `cargo clippy --workspace
--all-targets -- -D warnings`, `cargo test --workspace`, plus six
`apps/dashboard` gates (including an embedded-bundle freshness check) only when
the dashboard is touched — and stops at the first failure with the base branch
untouched (AGENTS.md rules 1–3).

## Related standing rules

- **Contract files are additive-only**: `events.rs`/`types.rs` add fields with
  `#[serde(default)]`; never break old logs/configs (AGENTS.md rule 6). This is
  why `base_sha` is `Option` (types.rs).
- **Sandbox fails closed**: a requested `enforce` the platform cannot honor
  refuses to run rather than run unsandboxed;
  [sandbox.rs](../../../crates/engine/src/sandbox.rs) `fs+net` on macOS runs
  loopback-only Seatbelt egress + the filtering egress proxy
  ([egress_proxy.rs](../../../crates/engine/src/egress_proxy.rs)), and
  `--unshare-net` on Linux (bwrap cannot reach a host proxy), rather than
  fake a hostname egress allowlist. A proxy that cannot start fails the run
  before the session spawns.
