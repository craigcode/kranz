---
title: Inviolable invariants
owner: agent
freshness: check-on-touch
last_verified: 2026-11-14
verified_against:
  - crates/acp/src/lib.rs
  - crates/acp/src/terminal.rs
  - crates/engine/src/acp_terminal.rs
  - crates/engine/src/backend_acp/terminals.rs
  - docs/scoping/shared-acp-client.md
  - crates/engine/src/container_egress.rs
  - crates/engine/src/orchestrator/live_permissions.rs
  - crates/engine/src/review_packet.rs
  - crates/server/src/lib.rs
  - crates/engine/src/acp_worker.rs
  - crates/engine/src/acp_resources.rs
  - crates/engine/src/backend_acp.rs
  - crates/engine/src/reviewer_independence.rs
  - crates/engine/src/sandbox_container.rs
  - crates/engine/src/sandbox_container/mount_proof.rs
  - crates/engine/src/sandbox_container/mount_proof/tests.rs
  - crates/engine/src/control.rs
  - crates/engine/src/paths.rs
  - AGENTS.md
  - docs/design.md
  - crates/engine/src/git_ops.rs
  - crates/engine/src/git_process.rs
  - crates/engine/src/event_log.rs
  - crates/engine/src/orchestrator.rs
  - crates/engine/src/orchestrator/finalization.rs
  - crates/engine/src/merge.rs
  - crates/engine/src/merge_gate.rs
  - crates/engine/src/sandbox.rs
  - crates/cli/src/exec.rs
  - crates/engine/src/scrub.rs
  - crates/engine/src/types.rs
---

Rechecked 2026-11-14 (UTC) against the grant decision binding fixes: a grant decision can now be bound to the pending request's sequence number and kind, and a mismatch is refused without changing the pending request or widening any grant. `types.rs` gained fields only. No invariant changed.

Rechecked 2026-11-13 (UTC) against a test-only change in `agent_env.rs`, `command_exec.rs`, `comparison_metrics.rs`, `knowledge.rs` and `sandbox.rs`: five engine tests no longer depend on where the contract scratch directory lives. No product code changed. The invariants are unchanged.

Rechecked 2026-11-12 (UTC) against the Sgian mission-runner terminal wiring: automatic Sgian terminal wiring remains restricted to uncontained checkout ACP workers with an external `KRANZ_SGIAN_BIN` and `0600` `.kranz/serve.grant.key`, and `terminal/create` evaluates `SessionSpec.disallowed_tools` (including shell `-c` subcommands and `.kranz` authority paths) and ambiguous-display checks before presenting one-call operator consent. Contained profiles and invariants are unchanged.

Rechecked 2026-11-11 (UTC) against the second audit round (mission id check, `kranz exec` question output). No invariant changed.

Rechecked 2026-11-11 (UTC) against the Linux socket mask in the bubblewrap wrap. No invariant changed.

Rechecked 2026-11-11 (UTC) against the evidence and containment fixes: the final gate fails a pty-script assertion whose most recent session failed, and contract-command output reaches the validator inside an untrusted-output fence. No invariant changed.

Rechecked 2026-11-11 (UTC) against the base-policy merge check: a mission still cannot weaken the checks that judge its own diff, and a base-branch policy change made after approval now needs explicit acceptance at merge. This is detection at the merge, not prevention: a checkout-mode worker can still move the base ref itself. No invariant changed.

Rechecked 2026-11-11 (UTC) against the push-guard and secret-scan fixes: the one push path still pushes a single `kranz/*` branch and nothing else, now also against tag-following, submodule recursion and push options, and only after the mission's range passes the secret scan. No invariant changed.

Rechecked 2026-11-11 (UTC) against the consent-binding fixes: a remote comment cannot queue an unreviewed plan unless the operator opted in from the global config layer, and an approved grant means exactly the target shown. No invariant changed.

Rechecked 2026-11-11 (UTC) against the merge-binding fix: the hosted merge lands only the mission commit sealed at completion, and changed paths are read NUL-separated so a quoted filename cannot skip a path-scoped gate. These strengthen the merge invariants; none changed.

Rechecked 2026-11-10 (UTC) against the plan-consent and branch-resolution fixes: base and mission branches are resolved as `refs/heads/<name>` at approval and merge, so a same-named tag cannot stand in for either, and engine-run git ignores replace refs. This strengthens the pinned-base and merge invariants; none changed.

Rechecked 2026-11-10 (UTC): Added opt-in `SgianTerminalProvider` for uncontained host ACP sessions; contained worker profiles and enforced sandboxes refuse it, `.kranz/serve.grant.key` joins authority/sandbox denies and scrubbing, `sgian ctl` invocations run `env_clear`d with bounded terminal output limits, and unconfirmed Sgian cleanup fails closed. All documented invariants hold unchanged.

Rechecked 2026-11-09 (UTC): Merged ACP R4 terminal budgets and v2 host admission onto v0.5.0 main alongside the security audit remediation. Exact-action authority, environment clearing, owned cleanup, pre-credential v2 admission, authenticated CLI serving, and workspace containment hold unchanged.

Rechecked 2026-11-07 (UTC): Reconciled ACP R4 with current main: exact-action authority, environment clearing, owned cleanup and pre-credential v2 host admission remain enforced. Terminal expiry is recoverable only after confirmed cleanup. The public compatibility-probe constructor still refuses released worker profiles.

Rechecked 2026-11-07 against the security audit fixes: ordinary CLI reads are authenticated, development origins require explicit opt-in, and dashboard overrides cannot follow asset symlinks. Workspace setup and data hooks inherit the resolved worker containment policy; unsupported containment fails closed. No event schema, primary-checkout isolation, mission push, or delivery invariant changes.

Rechecked 2026-11-02 (UTC) against the signal intake pipeline: `.kranz/signals/` is both read-denied and write-denied across sandboxed sessions and permission profiles, its append-only ledger uses domain-separated HMAC sealing (`kranz.signal-ledger.v1`) and an out-of-repo high-water mark, and `/api/hooks/sentry` joins `/api/hooks/github` as an independently HMAC-authenticated webhook route that creates only undrafted `signal-proposal: unconsented` tickets. All documented invariants hold unchanged.

Rechecked 2026-10-22 (UTC) against P2/P3 engine hardening and contract read-back: read-back sessions run in an isolated scratch directory with all file/shell/search tools disabled and receive only `command` and `checkerFiles`. All documented invariants hold unchanged.

Rechecked 2026-10-18 (UTC) against the security and correctness hardening sweep: Git tag/config/network guards, sandbox authority masks, validator snapshot nofollow checks, and credential scrubbing were tightened. All documented invariants hold unchanged.

Rechecked 2026-10-12 (UTC): R4 keeps exact one-use terminal authority and owned-namespace execution. Capacity is reserved before consent; controls have separate bounded accounting. Command/retention expiry is recoverable only with confirmed cleanup. Production terminal capability and the qualified 600-second v2 session ceiling stay unchanged.

Rechecked 2026-10-11 (UTC) against the hosted stack headroom change: stream read buffers moved to the heap and large stage futures are boxed where they are awaited. Memory placement only; no stage, gate, permission or invariant behavior changed.

Rechecked 2026-10-06 (UTC): R3 measurement profiles exist only in test builds. Production v2 remains refused; live measurements require a separately approved, one-attempt manifest. Kernel accounting remains on the protected supervisor channel.

Rechecked 2026-10-06 (UTC) against ACP resource failures (R2). The test-only
resource revision adds bounded tmpfs and session-time limits plus protected
supervisor observations. Classification requires observed evidence, not a worker
exit code or configured ceiling alone. The optional completion/state fields are
additive. Namespace removal is recorded separately from private-home cleanup;
engine death can leave a stopped namespace and ledger for explicit recovery.
Production v1 supervision and the existing authority boundaries are unchanged.

Rechecked 2026-10-04 (UTC) against ACP profile resource ceilings (R1); the sandbox container tier still carries `--pids-limit`, and ACP ceilings apply only to revisions that declare them; currently that is a test fixture. Overrides have runtime floors and revision maxima, and production v1 profiles remain unchanged. No invariant changed.

Rechecked 2026-10-04 (UTC): the container fixture helper now matches its
Unix-only test callers. Production containment, authority and deadlines are unchanged.

Rechecked 2026-10-03 (UTC) for the completed container-denial fixture scope.
File and network access probes require guest receipts and confirmed cleanup;
production routing, process ownership and mission contracts are unchanged.

Rechecked 2026-09-30 against the container-denial fixture changes: production
sandbox policy and deadlines are unchanged. File-access denial probes now
require guest execution, the expected access failure and confirmed cleanup;
local synthetic checks do not substitute for the live Linux receipt.

Rechecked 2026-09-29 (UTC) against the public-documentation addition to AGENTS.md; consent, containment, Git and event-log invariants are unchanged.

## What this is

Rechecked 2026-09-26: Sgian write credentials now require explicit operator
opt-in to an absolute helper outside the checkout and are never issued to
enforced workers. Approval controls render and refuse ambiguous characters;
external acceptance requires committed, covered source. Progress batches use
the existing single writer, and mount admissions re-prove the current image.

Rechecked 2026-09-25 for the shared ACP extraction: framing, bounded readers
and unique-key decoding move to `kranz-acp`. Engine process supervision,
credential filtering and durable permission authority retain their owners.
Broker answers are a local input type and cannot be created by a wire frame.
Released worker profiles keep filesystem, terminal and resume capabilities
disabled. A private terminal fixture now requires exact-action consent and uses
the owned namespace, scoped handles and confirmed descendant cleanup. It adds
no host execution fallback or production configuration switch. See the
[terminal contract](../../scoping/acp-terminal-provider.md).

Rechecked 2026-09-22 for the Sgian release corrections: coordination helpers use
bounded process-tree supervision and a cleared discovery environment. Worker
credential guards cover dropped futures; the scrubber recognizes client tokens.
These reinforce existing process and evidence boundaries without granting any
new mission authority. Unconfirmed revocation still needs operator action.

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
any git process runs**. The remote must also be a plain, already-configured
name; flag-shaped values and arbitrary URLs cannot become Git options. The handle it runs on is hardened like every other
(below), so the tree the worker just wrote cannot answer the push with a
`pre-push` hook or a planted `core.sshCommand`.
[merge.rs](../../../crates/engine/src/merge.rs) states
in its module doc that the gated merge never calls push — the base branch is
only ever advanced locally.

## base_sha is pinned at approval and never re-resolved

At plan approval `approve_plan` resolves the base branch tip exactly once
(`self.repo.rev_parse(&base)`) and records it on the `plan.approved` event; it
lands on `mission.base_sha` and is exported to worker/validator sessions as
`KRANZ_BASE_SHA`. Mission deliverable diffs use the pinned sha, never a replacement resolved
from the moving branch name. The external approval driver re-reads the branch
only to detect drift and refuse consumption; it does not replace the pin. Merge
separately binds its scratch integration to the live base it will advance.
WHY: incident m-660ffc — a contract's `git diff main` assertion raced a commit
landing on the base branch mid-mission (design.md deviation 6). Re-resolving the
base anywhere after approval reintroduces that race.

## Approved reviewer independence survives fallback

An enabled `reviewerIndependence` role must retain a known different model family
from every recorded worker attempt. The approved plan owns this policy; live
config and plan revisions cannot relax it. Each resolved reviewer, retry and
confirmation is checked before launch; unknown identity, same-family fallback
or a skipped required role blocks the milestone. Closure also requires a
successful compatible review from the latest relevant validation round: neither
a skip nor a replayed completion status substitutes for that evidence. Final
gates and lesson preparation must preserve the reviewed checkout; detected drift
starts a fresh validation epoch and blocks. Containment remains a separate
requirement. [Implementation](../../../crates/engine/src/reviewer_independence.rs);
[configuration](../../config-composition.md#reviewer-independence-reviewerindependence).

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

Human review packets observe existing authority; they never issue consent or
replace a gate. Their HTTP views require a header capability even on otherwise
anonymous loopback servers and are generated in memory, outside validator
inputs and committed reports. Existing controls recheck the exact request and
binding when the operator acts. [Review packet boundary](../../review-packets.md).

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
diff programs and text converters. Local Git runs with a cleared, allowlisted
environment. Every invocation rechecks the original driver boundary, retaining
its overrides across clones and driver removal while refusing new driver names
and repository ordinary/conditional includes. Enforced sessions and gates
protect configuration files, Git indirection and metadata directory nodes;
unprotectable writable layouts fail closed. Unsandboxed host writers can still
race a preflight read. Explicit network operations retain their separately
guarded operator credential path. Every Git subprocess has a deadline and
separate raw stdout/stderr limits; overflow is an error, never a successful
truncated result. See [Git execution boundary](../../git-execution-boundary.md).
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
([finalization.rs](../../../crates/engine/src/orchestrator/finalization.rs)) counts non-meta
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
  The preflight must also own its helper and confirm daemon absence: a failed
  exchange or uncertain cleanup never grants admission. Interrupted creation or
  unconfirmed removal retains private recovery intent; a guest deadline bounds
  the running helper after engine death. Required runtime mount proofs currently
  support Docker on Linux/macOS and refuse other runtimes before spawn. Session
  preflight uses the configured image; a missing pinned image is refused without
  pulling. Qualified ACP worker profiles also pin argv, private file credentials
  and startup settings, and require worktree isolation. Their engine-run checks
  stay offline and receive no selected worker login. Contained ACP completion
  requires an observed Docker-client exit status; expiration of its bounded
  wait fails closed, and killing that client cannot establish success. See
  [mount preflight ownership](../../acp-containment.md#mount-preflight-ownership).
  Filtered egress requires a recognized stable Docker daemon >=25.0.5, a DNS
  prerequisite rather than blanket qualification. Read-only caches are readable
  and host-mutable, and default-bridge peers share relay access: the documented
  boundary assumes a trusted host/daemon, not hostile Docker tenants.
- **Authority material is write-denied, not only read-denied**: every path the
  read deny names, plus `<repo>/.kranz`'s engine-owned stores (`queue/`,
  `tickets/`, `lessons/`, `hook-status/`, `signals/`), every SIBLING mission dir, the
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

## One-call consent never widens mission policy

An ACP answer names an exact request and binding digest. The engine must persist
the resolution before writing the protocol response; a sent receipt is not proof
of the effect. Prohibited or incomplete actions cannot be approved, and only
unique certified once-only options qualify. Expiry, action drift, a dead peer
or a lost response channel closes the request without retrying the effect.
A deadline belongs to its session, not sibling candidates. Mission-wide controls
still stop and join the batch before they apply; only permission answers and
noninterrupting messages are handled immediately.
Local capability authority is recorded as such; only a verified Slack interaction
supplies a Slack user identity. Permission callbacks alone remain cooperative;
only the explicit qualified worker profiles add the separately proven container
boundary. See [consent operations](../../acp-live-permissions.md) and
[profile admission](../../acp-containment.md#qualified-ordinary-workers).
