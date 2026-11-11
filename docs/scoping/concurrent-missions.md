# Concurrent missions: more than one running mission per repository (scoping)

Status: scoped 2026-11-11, DESIGN-FIRST. Decisions marked OPERATOR DECISION
are the operator's; PROPOSED ones stand unless the operator changes them.
Nothing here is built. D-A is a gate: if it is declined, the rest does not
apply. Review this note before drafting any `concurrent-missions-*` ticket.

## What this is

A repository runs one mission at a time. A queue of ten approved plans is
drained one after another, however unrelated their changes are. This note
scopes letting several approved missions run at once in one repository when
their approved write paths do not overlap, while merges stay one at a time.

The proposal is a change to admission and locking. It adds no worker pool, no
new agent role and no new prompt path. A mission runs exactly as it does
today; what changes is whether the next one must wait for it.

## What already exists (verified 2026-11-11)

- **One lock makes the rule.** `RepoBusyGuard` in
  `crates/engine/src/queue.rs` (section "Repo busy guard: one RUNNING mission
  per working tree") writes `.repo.busy.lock` and `.repo.busy.mission` under
  the queue directory. The drain loop takes it through
  `claim_front_when_repo_free` (`crates/engine/src/work.rs`), a hosted start
  takes it through `acquire_repo_busy` (`crates/server/src/host.rs`), and
  both hold it for the whole run. `is_repo_busy` reports one holder.
- **The merge takes the same lock.** `docs/merge-gates.md` and
  `docs/protocol.md` (`POST /api/missions/:id/merge`): the merge acquires the
  repo-busy lock, pins the live base and the mission tip, merges in a detached
  scratch worktree and runs the base-owned gate suite there.
- **Missions are already isolated from each other on disk.** Under the
  default `workerIsolation: "worktree"`, sessions work in a mission worktree
  on a mission branch and the primary checkout stays byte-untouched
  (inviolable invariants). Each mission has its own state directory and its
  own hash-chained event log under `.kranz/missions/<id>/`.
- **`workerIsolation: "checkout"` is not isolated.** Workers write in the
  primary checkout, so two such missions cannot run together under any
  design.
- **The orchestrator runs at the primary checkout**, read-only, with no OS
  sandbox (`SessionSpec { cwd: repo_root, writable: false, sandbox: None }` in
  `orchestrator.rs`). Several of these at once read the same tree and do not
  write it.
- **Plans declare write paths.** `Plan.touchSet` is a list of glob patterns,
  shown in full at plan approval. A write outside it parks a touch-path grant
  request.
- **A glob-overlap test exists.** `declared_touch_overlaps`,
  `glob_literal_stem` and `paths_overlap` in
  `crates/engine/src/pack/resolution.rs` compare glob stems on `/`
  boundaries and over-select when unsure (an empty stem overlaps everything).
- **The merge already detects a moved base.** It refuses a mission branch
  that moved after validation, refuses when the base's gate suite or secret
  allowlist changed since approval until the operator accepts
  (`base_policy_changed`), and gates the merged result, not the branch.
- **Ticket ordering exists.** `blocked-by` in ticket frontmatter, with cycle
  detection and an unsatisfied-blocker refusal in `deps::approve_ticket`.
- **In-mission parallelism exists and is separate.** `maxParallelWorkers`
  (default 1) fans out workers inside one mission (roadmap M3). This note
  does not touch it.
- **Hosts already run repositories in parallel.** `docs/backlog-and-slack.md`:
  one mission at a time per repo, N repos in parallel. The catalog rejects
  duplicate ids and duplicate canonical roots
  (`docs/scoping/m8-multi-root-host.md`); it has no rule about two roots
  sharing a remote.

## Invariants

1. Concurrency is off unless the operator turns it on in the global config
   layer. A repository's own config cannot raise it.
2. A mission runs concurrently only on the strength of what a human already
   approved: its plan's write paths and its ticket's `blocked-by`. No model
   decides admission.
3. Merges stay serialized and keep every existing merge check.
4. The primary checkout stays byte-untouched. `checkout` isolation never
   runs concurrently.
5. A mission's event log, cost record and evidence stay per mission and
   unchanged in shape. Contract files change additively only.
6. kranz still never pushes.

## D-A. Whether this fits the positioning freeze (OPERATOR DECISION)

`docs/knowledge/decisions/positioning-governance-evidence-layer.md` freezes
"worker pools beyond the shipped M3 machinery" and says further M3-style
parallel-execution investment is deprioritised unless it serves the
governance surface. Its 2026-07-31 gloss says the freeze covers pools whose
claimed value is code velocity or quality.

Against: the visible benefit of this change is throughput. That is a
velocity claim.

For: nothing here changes how an agent writes code. The change is in the
dispatch row of the ADR's own table ("Dispatch, gates, escalation, cost,
provenance, evidence: Kranz"). Admission is derived from approved authority,
and the record of why two missions were allowed to overlap in time is new
evidence.

The operator decides whether this is a carve-out, and if so the ADR gets a
dated gloss naming it before any ticket is drafted. If declined, the fallback
in "Without this change" is the supported answer.

## D-B. Split the lock (PROPOSED)

Replace the single busy lock with two:

- **Run slots.** Up to N holders, one per running mission. Each slot records
  its mission id and pid, with the same liveness and stale-recovery rules the
  busy lock has today.
- **Merge lock.** Exclusive, held only for the merge transaction.

A merge no longer waits for running missions and running missions no longer
wait for a merge. With N = 1 the behaviour is the same as today except that a
merge can proceed while a mission runs; D-F covers why that is safe.

`is_repo_busy` keeps its meaning for single-holder callers (it returns a
holder when any slot is taken) and gains a sibling that lists all holders.

## D-C. The limit is an operator-only key (PROPOSED)

`maxConcurrentMissions`, default 1, settable from `~/.kranz/config.json` only
and refused from the project layer by `check_project_layer_keys`, like the
other keys that widen what a repository can cause. It is not runtime
patchable from the control inbox. An upper bound is enforced in validation;
the proposed bound is 4 until D-H's measurements say otherwise.

## D-D. Admission by approved write paths (PROPOSED)

When a slot is free, the drain considers queued entries in queue order and
admits the first one that passes all of:

1. Its plan has a non-empty `touchSet`. A plan with none declares no bound
   and runs alone.
2. Its `touchSet` does not overlap the `touchSet` of any running mission,
   using the existing stem comparison, which errs toward "overlaps".
3. Neither it nor a running mission is `blocked-by` the other, directly or
   through the ticket graph.
4. The repository uses `worktree` isolation.
5. The existing readiness and pre-run checks pass.

An entry that fails 1 to 3 is skipped, not failed; it stays queued and is
considered again when a slot frees. Queue order is otherwise preserved:
admission never reorders two entries that could both run.

Each admission writes a decision to the admitted mission's log naming the
missions it was admitted alongside and the path comparison that allowed it.
That record is the evidence D-A's "for" argument rests on.

Starvation: a wide mission (for example `touchSet: ["**"]`) could wait behind
a stream of narrow ones. PROPOSED rule: once an entry has been skipped for
overlap, no later entry that overlaps it is admitted ahead of it.

## D-E. Grants that cross into another mission (PROPOSED)

Admission is only as good as the declared paths. A running mission that
writes outside its `touchSet` parks a touch-path grant request today. With
concurrency, a grant whose target overlaps another running mission's
`touchSet` is still offered, with the overlap named on the grant card and in
the dashboard panel, and approving it does not pause the other mission. The
merge check in D-F is what protects the result.

OPERATOR DECISION if preferred: refuse such grants outright while the other
mission runs. That is stricter and can strand a mission until its neighbour
finishes.

## D-F. Landing after the base moved (PROPOSED)

With two missions in flight, the second to merge meets a base the first one
changed. Existing behaviour already covers correctness: the merge happens in
a scratch worktree against the live base, the gate suite runs on the merged
result, and a textual conflict or a failing gate refuses the merge.

What is missing is a defined next step for that refusal. PROPOSED:

- A gate failure or conflict at merge leaves the mission Delivered and
  unmerged, as today, with the refusal recorded.
- A new operator action, "re-validate on current base", re-runs the mission's
  validation contract against a merge of the current base into the mission
  branch inside the mission worktree. It spends validator budget only, never
  worker budget, and it is an explicit click.
- Fixing a real conflict stays a human act or a new ticket. kranz does not
  dispatch a worker to resolve it on its own.

## D-G. Shared resources (PROPOSED, each to be verified in the first slice)

- **Budgets.** Role budgets (`maxBudgetUsd`) are per session. A per-repository
  ceiling across concurrent missions does not exist; PROPOSED is a
  `maxConcurrentBudgetUsd` admission check that refuses to admit a mission
  whose estimate would take the sum of running estimates over the ceiling.
- **Build caches and ports.** Gate and contract commands from two missions
  may share a build cache or bind the same port. The mission worktree gives
  separate build output by default for most toolchains; a repository that
  pins a shared cache directory or a fixed port needs to say so. PROPOSED is
  a per-repository `exclusiveGates: true` escape that serializes gate and
  contract commands across missions with a third lock, leaving agent
  sessions concurrent.
- **Disk.** The pre-run free-space check counts one mission's needs. It must
  account for the slots already taken.
- **Git.** Several worktrees share one object store and ref namespace.
  Mission branches are distinct, so ref updates do not collide; the first
  slice proves this with a soak test like the M3 corruption soak.
- **Egress proxy and sandbox.** One per session today; expected to need no
  change. To be confirmed.

## D-H. Surfaces (PROPOSED)

- `GET /api/queue` keeps `busyWith` (first holder, for old clients) and adds
  `running: [missionId]` and, per queued entry, `waitingOn` with the reason it
  was skipped (`overlap`, `blocked-by`, `no-touch-set`, `no-slot`). Additive.
- Dashboard pipeline and the Slack todo show more than one running row and
  the waiting reason.
- `kranz work` prints each admission and each skip reason.
- Metrics: time queued, time running, and merge refusals caused by a moved
  base, so the operator can see whether a higher limit is paying for itself.

## Without this change

Two clones of one repository, each registered as its own catalog entry, are
two repositories to the host and run in parallel today. The cost is a split
queue, two sets of mission state, and keeping each clone's base current by
hand. This is described from the catalog rules, not tested; verify it before
relying on it.

## Delivery slices

1. D-A decided and the ADR glossed. No code.
2. Lock split (D-B) with N fixed at 1: merge no longer blocked by a running
   mission. Soak test for shared-store git operations.
3. `maxConcurrentMissions` (D-C) and admission (D-D) with the decision record,
   the queue API additions and the CLI output (part of D-H).
4. Re-validate on current base (D-F).
5. Budget ceiling, `exclusiveGates`, disk accounting (D-G).
6. Dashboard and Slack rows, metrics (rest of D-H).

Slices 2 and 3 are the smallest useful unit. Slice 2 alone is safe to ship
and useful on its own.

## Out of scope

- Any change to how many workers a mission runs or how it picks them.
- Automatic conflict resolution or automatic rebase by an agent.
- Concurrency under `checkout` isolation.
- Scheduling across repositories, which the host already does.

## Open questions

- Is stem-level overlap too coarse for a monorepo where most plans declare
  paths under one top-level directory? If so admission will rarely fire and
  the limit buys little. Measure on the existing mission history before
  slice 3.
- Should a mission admitted alongside others record that fact in its merge
  evidence, so a reviewer of the merged change can see what else was in
  flight?
- Does the hosted start path (dashboard "Start execution") follow the same
  admission, or stay "refuse when no slot is free" as today?
