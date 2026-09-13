# Beads as kranz's work-item store — bridge, don't substrate

Status: investigation complete 2026-07-24. Answers the five questions in
`.kranz/tickets/beads-workstore-investigation.md`. D-BW-1..4 are
**recommended** decisions, recorded here for operator confirmation (per the
scoping-doc convention they bind once confirmed). No code changed.

## Why this was asked

Beads (github.com/gastownhall/beads, MIT, very active — v1.1.0 2026-07-04,
daily commits) offers things kranz's tickets lack: a discussion primitive
(comments/threads), cross-repo federation, and "less custom machinery."
The counterweight going in: kranz's claim/drain layer is mission-safety
machinery, not a list of files, and D5 (`docs/gascity-citizenship.md:919`)
already deferred the same question with "kranz-native queue is the better
target" — a queue that has since **shipped** (M2.75 + pipeline-view D-B).
D5's wording stands: the spool is a spike-era stand-in; it never said the
replacement should be beads.

## The five questions, answered from evidence

### 1. Does beads' schema carry kranz's ticket fields?

Partially, with an official extension point for the rest. Verified against
`internal/types/types.go` on main:

- **Native:** `acceptance_criteria` (kranz's `## Acceptance hints`),
  `description`/`design`/`notes` (Goal/Context/scoping prose), `priority`
  (0–4 vs kranz's 1–3), `labels` (the spike's routing mechanism),
  `blocks` dependencies with cycle rejection (kranz's `blocked-by`),
  `due_at`/`defer_until` (a superset of kranz's `schedule`).
- **Not native:** `task-class` (executor-tier routing), structured scoping
  answers, and mission linkage. Beads' documented answer is `metadata`
  (arbitrary validated JSON — "the preferred extension point for data that
  is specific to an integration, orchestrator, team workflow") or
  `external_ref` (e.g. `kranz-m-<id>`).
- **Correction to the ticket's premise:** `touch_set` is not a ticket
  field — it lives on the approved `Plan`/`Mission` (types.rs:94,62),
  declared by the planner and enforced by permissions. It would never be a
  bead regardless of substrate; conflating it here would be a category
  error.

Verdict: workable via `metadata`, but every contract-relevant field is
one schema migration away from meaning something different upstream.

### 2. Can beads model the pipeline state machine without lossy mapping?

Almost — the one thing it cannot model is the part that is not a status.
Beads has seven built-in statuses plus **custom statuses with behavior
categories** (`bd config set status.custom "needs_context:active,review:wip,parked:frozen"`),
which covers New→Drafting→NeedsContext→Review→Queued→Running→Parked→
Done/Failed honestly. What has no analogue is **DELIVERED/LANDED** — and
that is not a gap to fill, because the split is *derived* (`merged.rs:38`:
is the mission branch tip an ancestor of the live base tip?), not stored.
Any substrate would recompute it kranz-side anyway. The closest beads
primitive (a `gh:pr` gate) measures GitHub state, not local ancestry.

### 3. What does the queue claim protocol map to?

More than expected — beads grew the primitives post-spike:

- **Atomic claim:** `bd update <id> --claim` ("first claim wins; repeating
  a claim you already hold is idempotent") ≈ kranz's atomic rename to
  `.claimed.<pid>`.
- **Dead-claim recovery:** claim **leases with TTL + heartbeat**
  (`lease_expires_at`, `heartbeat_at`) ≈ kranz's `recover_dead_claims`
  (pid-liveness + 1h fallback).
- **Repo-busy guard:** `bd merge-slot acquire/release` is a direct
  analogue of `.repo.busy.lock`.
- **No analogue:** priority+seq ordering files, `.mutate.lock`
  cross-process semantics, and the M8 fair round-robin across repo queues
  (beads has assignee/priority-filtered `bd ready`; cross-repo fair
  scheduling is kranz's catalog's job under any substrate — see Q5).
- **Caveat:** embedded mode is single-writer by file lock; multi-writer
  needs server mode (`dolt sql-server`). Claim atomicity per mode is
  asserted in docs but not mechanically detailed; the Dolt re-platforming
  is months old.

### 4. Migration/provenance for the 85 committed tickets and the dogfood record?

Cheap, and smaller than it looks. Tickets import via `bd create` with
`external_ref`/`metadata` carrying the kranz slug for provenance; the
committed `.md` files stay as the human-readable record (beads' own
`issues.jsonl` is explicitly "an export for viewers, not the source of
truth" — the same shape kranz already has). The dogfood record — mission
event logs, reports, lessons, calibration corpus — is **not ticket data**
and does not migrate under any answer; beads would only ever replace the
ticket index, not the mission record. Migration cost is a day; the
question is what it buys (Q5).

### 5. Bridge-hardening vs substrate replacement, against the D5 precedent?

D5 said native, and native shipped. Substrate replacement now means
trading a queue whose claim semantics are ours, gated (45 suites), and
contract-aligned — for a fast-moving external system whose own
re-platforming (Dolt) is months old, whose per-mode claim semantics are
not yet mechanically documented, and whose dialect drift has **already
bitten the existing spike** (`bd set-state <id> blocked` in
`packaging/gascity/bin/kranz-run-bead` is stale against current upstream
`set-state <dimension>=<value>`; the fallback `bd update --status blocked`
is the correct form). That drift in a 200-line spike is the integration
risk in miniature; the queue claim layer is the worst possible place to
scale it up.

## Accepted decisions (recommended)

### D-BW-1 — Stay-native substrate; beads remains an interface

**ACCEPTED 2026-07-29** (operator). kranz's ticket/queue system stays kranz-owned. Beads is an *interface*
kranz interoperates with (the Gas City direction the citizenship work
already needs), never the store of record for `.kranz/tickets`. Rationale:
Q3's claim machinery is mission-safety infrastructure with semantics we
control and gate; Q1/Q2's mapping is workable but buys nothing the queue
does not already do; Q5's risk lands on the single worst component to
destabilize. This confirms the ticket's working hypothesis with evidence.

### D-BW-2 — Harden the bridge, sized as two mission briefs

**ACCEPTED 2026-07-29, ticketed** (operator wants beads/Gas City interop
this week): `.kranz/tickets/beads-bridge-translator-correctness.md` and
`.kranz/tickets/beads-bridge-provenance-return-path.md`.

The spike (`packaging/gascity/bin/kranz-dispatch`, `kranz-run-bead`)
hardens into the supported translation layer:

- **Brief 1 (translator correctness):** replace the stale `set-state`
  blocked path with `bd update --status`; claim via `bd update --claim`
  (lease-aware — heartbeats or short TTLs so a dead dispatcher releases
  work); status map `open/in_progress/blocked/closed` ↔ kranz
  Queued/Running/Blocked-report/Done; acceptance-criteria array
  unwrapping kept; a round-trip fixture test against a live `bd`.
- **Brief 2 (provenance + return path):** on dispatch record
  `external_ref = kranz-<slug>` and on close write the kranz mission id
  back as a `bd comment` (the bead becomes navigable to kranz's mission
  record and vice versa); refuse-and-comment path preserved.

### D-BW-3 — Adopt beads' ideas natively, not its store

**ACCEPTED 2026-07-29** (operator), ticketed:
`.kranz/tickets/ticket-discussion-primitive.md` and
`.kranz/tickets/ticket-defer-until.md`.

What beads has that is genuinely worth having, adopted kranz-side:

- **A ticket discussion primitive.** Beads' flat comment model
  (`{author, text, created_at}` per issue) maps naturally onto a
  `.kranz/tickets/<slug>.notes.jsonl` sidecar or a `## Discussion` body
  section — ticket as its own ticket when needed (the aspirational
  thread_ts capture stays a separate Slack-side concern).
- **Deferral.** `defer_until` (hidden from ready until then) is a
  strictly better `schedule` for one-shot tickets; consider when the
  scheduler next opens.

Federation (D-BW-1's premise) stays Gas City's side of the bridge — it is
their topology, not ours to import.

### D-BW-4 — Revisit triggers

**ACCEPTED 2026-07-29** (operator). Re-open this question only if: (a) beads' per-mode claim semantics are
mechanically documented AND stable across two minor releases; (b) the
native queue shows a concrete bead-shaped deficiency (multi-agent claim
contention the rename protocol cannot express); or (c) the Gas City
citizenship work requires kranz tickets to *be* beads for routing, not
merely translatable to them. None holds today.

## Sources

- Upstream: `internal/types/types.go`, and docs on main: metadata.md,
  statuses.md, dependencies.md, comment.md, graph-links.md, dolt.md,
  coordination.md, federation.md, json-schema.md
  (github.com/gastownhall/beads, fetched 2026-07-24).
- Local: `docs/gascity-citizenship.md` (D5), `packaging/gascity/bin/`
  (spike), `crates/engine/src/ticket.rs:91-118` (state machine, sidecar),
  `crates/engine/src/queue.rs:63-511` (claim protocol),
  `crates/engine/src/merged.rs:38` (DELIVERED/LANDED derivation).
- Unverified flag: the gc-vendored `bd` dialect version (the spike speaks
  `gc bd`, not upstream `bd`) — Brief 1 must verify against a live gc
  install before relying on any command shape.
