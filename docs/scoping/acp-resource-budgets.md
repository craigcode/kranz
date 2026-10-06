# ACP resource budgets: qualification scope

Status: scoped 2026-09-30, reviewed 2026-10-04 against public `a07467c2`. The D-R
recommendations below were proposed operator decisions. The operator approved
R2 implementation on 2026-10-06; production values/admission still require R3.
The initial scoping review made no runtime changes. R1/R2 implementation is
restricted to test-only revisions; neither ran paid agent sessions.

Ticket: [acp-resource-budget-qualification](../../.kranz/tickets/acp-resource-budget-qualification.md).
It blocks [acp-sgian-terminal-qualification](../../.kranz/tickets/acp-sgian-terminal-qualification.md).
Prior contracts: [ACP containment](../acp-containment.md),
[terminal provider](acp-terminal-provider.md),
[September 26 review](../reviews/2026-09-26-acp-gate-remediation.md).

## Scoping baseline (before R1)

Qualified ACP workers run in an engine-owned Docker container. The argv comes
from the shared `sandbox_container::container_run_args` builder, which
`OwnedContainer::prepare` (`acp_container.rs`) rewrites to `create` and
extends. Terminal commands run through `docker exec` into that same container,
so any container ceiling also covers them.

| Resource | Worker container today | Where |
|---|---|---|
| Processes | `--pids-limit 512`, shared by every worker and gate container | `sandbox_container.rs:482`, `run_prologue` |
| Memory | none | |
| CPU | none | |
| ulimits | none | |
| Session wall clock | none found for worker sessions (the 600 s stall timeout covers only the orchestrator stream) | `orchestrator.rs:147` |
| OOM detection | none; an OOM kill surfaces only as an exit status inside free-text `SessionExit::Failed` | `backend_acp.rs:1810-1864` |

One container in the repo already has a full ceiling set. `DockerEvaluator`
passes `--pids-limit=32 --memory=128m --memory-swap=128m --cpus=1 --ulimit
fsize=...` and rejects on `State.OOMKilled` (`gate_evaluation/subprocess.rs:251-273`,
`:457-500`). That is the pattern to reuse.

Profiles are id strings (`claude-acp-0.77.0-arm64-v1`, `codex-acp-1.11.0-arm64-v1`)
mapped by `AcpWorkerProfile::definition()` to a code-resident `Definition`
(`acp_worker.rs:31-77`). `Definition` has no resource fields. The mission policy
digest covers the operator config, and so the profile id, but not the
`Definition` behind it (`gate_evaluation/authority.rs:68-76`).

## Gaps

1. **Changing the shared prologue would silently change v1.** Adding
   `--memory` or `--cpus` to `run_prologue` alters every container worker and
   gate, and changes the `-v1` profiles' runtime contract without changing their
   ids. The containment doc already says a changed image needs its own
   qualification. The same applies to limits.
2. **Adapter and terminal share one memory pool.** A `cargo build` in a
   terminal and the Node adapter process live in one cgroup. Under a memory
   ceiling the kernel OOM killer selects a victim, which may be the
   adapter. The run then dies with no agent-visible terminal error.
3. **Long commands are impossible.** The guest driver's `WALL_SECONDS = 30`
   (`acp_container/terminal.py:18`) covers the command and the handle's
   retention until release. `wait_for_exit` has a hard 35 s timeout that calls
   `fail()` and ends the session (`acp_terminal.rs:467-478`). A two-minute build
   fails the whole run. The host also bounds `capture_driver` to 40 s; all
   three deadlines must change together to support longer commands.
4. **Terminal polling spends consent budget.** Every terminal request,
   including `output` polls, is inserted into the same 1,024-entry
   `seen_permission_ids` set as `session/request_permission`, and in-flight
   waits count against the same 16 `MAX_PENDING` slots as pending human
   decisions (`backend_acp/terminals.rs:109-117`, `backend_acp.rs:1483-1497`).
   An agent that polls output every second exhausts the set in about 17 minutes
   and the run fails with "duplicate or excessive ACP client requests".
5. **Exhaustion behavior is inconsistent.** Hitting `MAX_TERMINALS` or
   `MAX_CREATIONS` returns a `-32000` error the agent can handle, but only after
   the one-use authority is consumed (`acp_terminal.rs:378-380`). Most other caps
   fail the session. Neither outcome records which budget ran out in a typed
   field.
6. **Untested caps.** No test drives the 30 s wall, the 35 s wait, terminal or
   creation caps, the replay-set cap, the pending-slot cap, the 64-entry receipt
   queue, an agent that never polls output, or `session/cancel` with a live
   terminal outside `abort()`.

Output handling itself is sound. The host drains continuously into a bounded
`OutputTail` that drops from the front at UTF-8 boundaries, so the command never
blocks on an unpolled terminal and host memory stays at the byte limit. Keep it.

## Proposal

### Profile resource contract

Add resource fields to `Definition` and ship new profile ids
(`claude-acp-0.77.0-arm64-v2`, `codex-acp-1.11.0-arm64-v2`) that carry them.
R1 and R2 exercise test-only revisions. Production v2 ids remain unavailable
until R3 proves useful work, enforcement, failure evidence and cleanup on the
qualified host/adapter tuple. The `-v1` ids keep their current argv byte-for-byte and stay selectable until
the operator retires them. Non-ACP container workers and gates are out of scope
and keep the shared prologue unchanged.

Each v2 definition declares:

- `memory`, with `--memory-swap` equal to it (no swap);
- `cpus`;
- `pids` (overrides the shared 512 for the ACP container only);
- `nofile` and `fsize` ulimits;
- a size cap on each writable tmpfs;
- a session wall-clock ceiling (D-R5).

The engine applies these flags only on the ACP create path in
`OwnedContainer::prepare`, next to the existing splice. It records the applied
values in `OwnedContainer::receipt` and adds a `definitionSha256` over the
canonical `Definition` to the profile receipt, so evidence shows exactly which
contract ran.

R1 covers memory, CPU, process count, open files and file size. R2 adds a
per-tmpfs ceiling and a supervisor-enforced session deadline to the test-only
revision. Production v2 remains unavailable pending R3. Override ranges have a minimum as well as a maximum: Docker requires
at least 6 MiB of memory; the default 100 ms CPU period and Linux's 1 ms minimum
quota require at least 10 millicores. Runtime-valid values are not necessarily
large enough for an adapter. Qualification must verify the effective limits
and host support, including swap accounting, rather than treating accepted
create arguments as enforcement evidence. See [Docker resource constraints](https://docs.docker.com/engine/containers/resource_constraints/)
and [Linux CPU bandwidth control](https://www.kernel.org/doc/html/latest/scheduler/sched-bwc.html).

Starting values come from measurement, not guesswork. The qualification
fixture records peak RSS, CPU time and process count for each adapter doing a
synthetic feature that builds and tests a small Rust and Node project with the
declared toolchains. Set each ceiling at measured peak plus documented
headroom. Record the measurement receipt next to the profile definition.

The guest terminal driver may raise `oom_score_adj` for its children to favor
killing a command over the adapter. This is a best-effort hint that needs its
own qualification, not a guarantee about the victim. The adapter can still
die. A `SIGKILL` or exit 137 alone does not prove memory exhaustion; pair any
classification with the observations described below. The container memory
ceiling remains the boundary. Per-terminal cgroups
would need a writable cgroup filesystem inside the container, and sibling
containers would break the admitted-namespace decision (D2 in the
[shared ACP client scope](shared-acp-client.md)). Neither is worth that cost.

### Session wall clock

ACP sessions have no bound today. Claude-backend workers stop at the role's
turn cap (`maxTurns: 50` for workers, `types.rs:1780`), but the ACP backend
passes `max_turns: None` (`backend_acp.rs:2137`). The wall clock is the only
limit on a runaway ACP worker, so size it to catch runaways, not to budget
work.

The proposed default is **60 minutes**, with room for a 30-minute terminal
build, and an operator override up to a **4-hour hard maximum** under D-R2.
These are provisional operator choices, not qualified ACP measurements.
Validate both against the R3 evidence before admitting a production revision.
A turn cap for ACP depends on how
each adapter reports turns and stays a separate decision.

### Failure classification

Before removing a resource-bounded ACP container, inspect `State.OOMKilled`, the exit status
and available resource observations, using `DockerEvaluator` as a starting
point. Classify a failure as `memory`, `pids`, `wallClock`, `terminalBudget`
or `other` only when a named observation supports it. Bind that evidence to the
owned namespace and observation interval. An exit code alone cannot identify
the resource: CPU limits throttle rather than necessarily terminate, and a
child may handle process, descriptor or file-size exhaustion without exiting.
Retain `other` when attribution is uncertain; never turn a tool's narrative
into a confirmed limit hit. A configured ceiling and an observed exhaustion
are separate receipt fields. Carry the class, observations and configured ceilings in the additive,
`#[serde(default)]` `resourceEvidence` field on the existing worker completion
event and folded worker run. No new event type is added. Successful runs can
also carry observations; a handled process-limit hit or CPU throttling does not
turn a successful attempt into a failure. The operator-facing failure message names the limit, the
value, and the config key that can change it.

A resource failure fails the feature attempt honestly. Retry policy is
unchanged. A mission whose deliverable is empty still fails.

### Terminal budgets for production

Split the fixture's single deadline into separate budgets:

| Budget | Fixture today | Production proposal |
|---|---|---|
| Command lifetime | 30 s, includes retention | Profile-declared, default 30 min, capped by the session wall clock |
| Handle retention after exit | inside the 30 s | Separate 5 min; release or expiry frees the slot |
| `wait_for_exit` | 35 s, then session failure | Stays pending until exit or command lifetime ends; lifetime expiry kills the command and returns its status |
| Host `capture_driver` supervision | 40 s total | Covers command lifetime, retention and bounded cleanup; cannot keep the old fixed deadline |
| Per-RPC deadlines (create/kill/release) | 8 s | Unchanged |
| Guest cleanup and drain | 3 s each | Unchanged |

Commands still never outlive the ACP session. The exec control pipe, the
stdin-EOF kill and the five-second lease stay exactly as they are. Separating
supervision from lifetime here means separate deadlines, not a detached
background executor.

Split the counters by class:

- **Consent requests** (`session/request_permission` and `terminal/create`)
  keep the current 16 pending slots, 1,024 mission IDs and 300 s TTL.
- **Terminal control requests** (`output`, `wait_for_exit`, `kill`,
  `release`) get their own pending budget of 32 and refuse duplicate IDs only
  while in flight. A per-terminal rate limit on `output` (for example 10 per
  second) replaces the lifetime count, so polling cannot exhaust the run.

Check terminal and creation capacity before emitting the consent proposal, so
a full table never consumes a human decision or an authority. Where the caller
can recover safely (capacity, rate limit, lifetime expiry), return a typed
error to the agent and let the session continue. Fail the session only when
cleanup is uncertain, as today.

Terminal capability stays off for released profiles after this ticket. Turning
it on for a real adapter remains the separate
[adapter qualification](../../.kranz/tickets/acp-terminal-adapter-qualification.md).

## R2 implementation and contractChangeRequest

The approved R2 change adds optional `worker.completed.payload.resourceEvidence`
and `WorkerRun.resourceEvidence`; absent fields deserialize as `None` and are
omitted on serialization. Old logs and snapshots retain their existing shape.
The record binds the immutable container ID/image, supervisor hash, random
engine owner, configured ceilings, host observation interval, Docker state,
supervisor counter interval and classification. `namespaceCleanupConfirmed`
refers only to namespace removal; private-home cleanup has its existing separate
failure path. The runner uses a trusted backend method;
agent messages cannot supply this field. A failure before the ACP handshake
finishes still returns a closed session so the runner can retain its evidence.

Only the test revision selects the new supervisor. The released v1 supervisor
bytes, definitions and create arguments remain unchanged. The resource revision
uses a private cgroup namespace and disables automatic container removal so the
engine can inspect stopped state before explicit removal. Normal completion,
abort and Drop keep bounded cleanup. Engine death can leave an **exited**
container and its private recovery ledger; the existing five-second guest lease
still ends live execution. R3 must include this recovery posture in qualification.

The supervisor opens and unlinks an observation file before spawning the worker.
The host retains the original descriptor; the supervisor is non-dumpable and
passes no observation descriptor to its children. Recreating the pathname or
writing JSON to stderr cannot forge that inode. Samples are capped and carry a
checksum so a torn write is unavailable evidence. Cgroup v2 counters record
baseline/current OOM kills, process-limit events and CPU throttling. Unsupported
or unreadable counters remain null. A partial sample proves only its recorded
interval; it is not proof that nothing happened afterward. Docker inspection
must match the created ID and owner label. An OOM observation establishes memory
exhaustion; it does not by itself distinguish the configured ceiling from host
memory pressure. Concurrent resource hits remain `other` rather than inventing
one causal explanation. Descriptor/file-size and tmpfs errors are not inferred
from worker text or exit codes; uncertain failed attempts remain `other`.

`tmpfsMib` bounds `/dev/shm` and every explicit tmpfs mount. Its minimum is 64 MiB
because Docker manages `/dev` with a fixed 64 MiB tmpfs; the fixture verifies that
mount also stays within the ceiling. Host bind mounts are not tmpfs and this is
not an aggregate disk quota. `sessionSeconds` starts after the trusted supervisor
receives the launch contract, before peer spawn, and includes handshake and
permission waits. On expiry the supervisor records the deadline hit and exits
PID 1, killing all namespace descendants. Lease loss, normal EOF shutdown and
ordinary exit 124 are not classified as session expiry. Fixture defaults remain
provisional: 64 MiB tmpfs and 3,600 seconds, maxima 256 MiB and 14,400 seconds.

This is governance/evidence through the existing backend and completion seams.
It adds no terminal admission, provider session, retry policy, prompt routing or
production profile. R4 owns terminal-budget observations; R2 does not emit the
reserved `terminalBudget` classification.

## D-R: proposed decisions

| Decision | Recommendation | Consequence |
|---|---|---|
| D-R1: How do limits reach existing profiles? | New `-v2` profile ids; `-v1` argv unchanged | No silent contract change; operators opt in by changing one config value |
| D-R2: Who can change a ceiling? | Operator only, in `~/.kranz/config.json`, within runtime-valid minimums and a hard maximum the profile declares; never repo config, plan or worker (`PatchClass::Never`) | Overrides land in the policy digest through config; the receipt records the effective value |
| D-R3: How are defaults set? | Measured peak plus documented headroom from a qualification fixture, recorded as a receipt | Numbers are defensible; re-measure when an adapter or image changes |
| D-R4: Shared memory pool | Keep one container cgroup; qualify `oom_score_adj` as a best-effort hint; the ceiling is the boundary | No per-terminal cgroup machinery; an adapter kill remains possible and requires evidence-based attribution |
| D-R5: Session wall clock | Profile-declared ceiling for ACP sessions only: default 60 min, operator may raise to a 4 h hard maximum; re-check after R3 | Bounds a runaway worker that has no turn cap; other backends unchanged |
| D-R6: Terminal counters | Separate consent and control classes as above | Polling cannot fail a run; consent limits stay as reviewed |
| D-R7: Failure record | Additive classified field on the worker completion event | No new event type; old logs still parse |

## Delivery slices

Estimates are engineering days for one contributor including tests and review.

| Slice | Depends on | Deliverable | Days |
|---|---|---|---:|
| R1 Profile contract | D-R1, D-R2 | Memory/CPU/pids/nofile/fsize fields, create-path flags, operator override validation, receipts with `definitionSha256`, proven on a test-only fixture revision | 3–4 |
| R2 Classification | R1 | Namespace-bound resource observations, additive event field, operator message, tmpfs/session bounds; test-only revisions until R3 passes | 2–3 |
| R3 Container qualification | R1, R2 | Useful-work measurements and adversarial fixtures for memory, processes, CPU, descriptors, file size, tmpfs and wall time; prove enforcement (including throttling), honest attribution and removal, then admit measured production v2 revisions | 3–5 |
| R4 Terminal budgets | R1 | Lifetime/retention/wait/capture deadline split, counter classes, pre-consent capacity checks, qualified `oom_score_adj` hint | 4–6 |
| R5 Terminal qualification | R4 | Fixtures for a multi-minute build, delayed wait, unpolled output flood, each cap reached, cooperative cancel with a live terminal, cleanup after exhaustion | 3–4 |

Total: **15–22 engineering days**, a planning estimate to revisit after R1.
R1 and R2 add mechanisms on test-only revisions; R3 is required before any
production v2 profile becomes selectable. R4 can start in
parallel once R1's field shape is settled.

The measurement in R3 needs a live adapter session. That is a bounded,
operator-authorized run with its own credential and budget approval. Every
other proof is synthetic and runs in the Docker-gated `acp_containment_v1` and
`acp_terminal_v1` CI lanes, which require each test by name and reject skip
markers. Add the new tests to those required lists.

## Acceptance

- The approved v2 profiles state defaults, the operator override range, and the
  failure message for each limit.
- `-v1` container argv is byte-identical before and after (a golden test).
- Positive fixtures do useful work within limits; adversarial fixtures prove
  each limit's actual effect, with namespace-bound observations and honest
  attribution. Every run leaves a receipt, removes its namespace and leaves no
  host process or container behind. A CPU-throttled success is not a failure.
- A terminal build longer than the old 30 s completes, and an agent polling
  output for the whole run does not exhaust any budget.
- Primary checkout stays byte-untouched, nothing pushes, and an empty
  deliverable still fails.
- Full workspace gates pass. New test filters are unique and nonvacuous.

## Out of scope

Limits for non-ACP container workers, gate containers and the egress relay
(worth a follow-up; the relay already has `--pids-limit 64`). Windows Job
Object memory limits. Enabling terminals on a released profile. Sgian panes.
Crash recovery and credential renewal, which belong to
[acp-recovery-and-credential-lifecycle](../../.kranz/tickets/acp-recovery-and-credential-lifecycle.md).
