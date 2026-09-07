---
title: Inviolable invariants
owner: agent
freshness: check-on-touch
last_verified: 2026-09-07
verified_against:
  - crates/engine/src/sandbox_container.rs
  - crates/engine/src/control.rs
  - crates/engine/src/paths.rs
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
any git process runs**. The handle it runs on is hardened like every other
(below), so the tree the worker just wrote cannot answer the push with a
`pre-push` hook or a planted `core.sshCommand`.
[merge.rs](../../../crates/engine/src/merge.rs) states
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

## The event log is append-only, single-writer, redact-at-write, sealed

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

Since the 2026-09-01 adversarial audit the log is also **sealed**. Every line a
keyed writer produces carries `h` (a sha256 chain over the previous `h` and the
event's canonical bytes) and `m` (an HMAC of `h` under the repository authority
key); `parse_log_bytes` verifies both, so every reader inherits the check.
New seals carry `v:2` and bind that version through the
`kranz.event-log.v2\n` hash prefix. They use exact float parsing and sorted
object keys. Versionless seals retain the legacy parser and hash format;
never relax a failed hash or re-sign old evidence to make an upgrade pass.
Unsealed lines are refused at or above the mission's out-of-repo seal floor,
integrity may never be dropped mid-log, and `resume` refuses a log that ends
below the recorded high-water mark. WHY: three gate decisions read the log back
mid-run, so a forged append or a rollback by truncation is a live consent
bypass, not only an audit gap.

## The consent substrate is authenticated, and its key lives outside the repo

The control inbox (`.kranz/missions/<id>/control/*.json`) carries operator
consent: grant approvals, revision decisions, answers, config changes. Every
file therefore carries a `sig` HMAC over the mission id and the command's
canonical JSON ([control.rs](../../../crates/engine/src/control.rs)); `drain`
and `peek_interrupt` verify it in constant time and quarantine anything
unsigned or wrongly signed to `.bad` without applying it. The key that signs
control files and MACs the log is per-repository and lives OUTSIDE the
repository, under the global kranz dir (`$KRANZ_HOME`, else `~/.kranz`), at
`keys/<repo fingerprint>.key`, mode `0600` in a `0700` dir
([paths.rs](../../../crates/engine/src/paths.rs)). Seal floors and high-water
marks sit beside it.
WHY: it was a plain directory any process with repo write access could drop a
file into, which made forging a human approval a one-file operation for a
worker, a validator, or any gate command (the 2026-09-01 adversarial audit,
C1). Never move this key inside the repo, and never widen the sandbox denies
that cover its directory. A key an agent can read or replace proves nothing.

## Engine-side git is hardened by default

`GitRepo::open` ([git_ops.rs](../../../crates/engine/src/git_ops.rs)) returns a
handle that neutralizes executable git configuration (hooks, `core.fsmonitor`,
filter drivers, custom merge drivers, `gpg.program`) and ignores user and
system config scope for local operations. Engine diffs also disable external
diff programs and text converters. Protected handles are refreshed after a
worker returns, before its output is inspected.
`open_unhardened` is the explicit escape hatch, with no engine caller, and
`ensure_identity` pins the operator's resolved `user.name`/`user.email` into
local scope so hardened invocations do not restamp commits.
WHY: engine-side git runs in the tree the worker controls, at the integration
worktree handle, the per-feature checkpoint commit, and the push. An unhardened
handle would execute a planted `pre-commit` or `core.sshCommand` outside every
sandbox with the engine's full ambient environment. Hardening was previously
opt-in at a handful of sites; the sites that forgot were the hole (the
2026-09-01 adversarial audit, H3). Invert this back and the hole returns.

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
  before the session spawns. The container provider extends the same rule to
  the mounts themselves: a runtime that accepts a bind mount and shares
  nothing would leave the declared write set silently absent, so a host
  without a continuous CI receipt must pass a bind-mount round trip before
  either a session or a gate resolves
  ([sandbox_container.rs](../../../crates/engine/src/sandbox_container.rs)).
- **Authority material is write-denied, not only read-denied**: every path the
  read deny names, plus `<repo>/.kranz`'s engine-owned stores (`queue/`,
  `tickets/`, `lessons/`, `hook-status/`), every SIBLING mission dir, the
  global key and seal dirs, and the session cwd's `.git` config-and-hook
  surface. Seatbelt also seals the direct children of `<repo>/.kranz` and
  `.kranz/missions` and denies the operator's own tty read, write and ioctl;
  bwrap unshares pid, ipc, uts and cgroup and takes `--new-session`
  unconditionally; the container tier carries the relay's `--user`,
  `--cap-drop ALL`, `no-new-privileges` and `--pids-limit`, and mounts only the
  Cargo cache subdirs rather than the credential-bearing `$CARGO_HOME` root.
  Mount-backed sandboxes use private read-only authority directory views,
  preserving allowed policy entries while hiding token/config files created
  or atomically replaced after launch. Seatbelt also pins authority ancestors
  against renames that would move protected files outside their path denies.
  On Linux, restoring a writable private workspace must also restore any Git
  or cache write protections it covers, before deeper authority masks are
  applied. A later host bind must never reopen a protected path or hidden
  credential.
  WHY: under `workerIsolation: "checkout"` the session cwd is the repo root, so
  a contained worker could once overwrite the trust root it could not read, or
  forge approvals into a sibling mission's inbox (the 2026-09-01 adversarial
  audit, H2/H3/H7/H8/H11/H12).
