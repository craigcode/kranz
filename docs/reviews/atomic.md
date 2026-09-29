# Engineering notes from the Atomic review (2026-08-08)

Historical design inputs from [Atomic](https://github.com/atomicdotdev/atomic),
recorded against its v0.14.0 source. The public notes retain Kranz's technical
requirements and attribution. They do not certify that project's current
implementation or change Kranz's accepted architecture. Drafts below were
unfiled research proposals; current tickets and security policy govern any
future implementation.

## 1. Lifecycle-independent session attestations

An audit record should survive mission abandonment and branch deletion.
A proposed session attestation folds per-model usage and cost, wall/API
duration, diff statistics, covered event/commit identifiers and a hash link
to the previous session in a resumed lineage. Coverage queries should expose
work with no attestation. The historical source reference is
`atomic-core/src/change/attestation.rs`; Kranz's relevant seams are the
event log, abandonment reconciliation and evidence bundle.

## 2. Evidence-bound terminal states

A completion transition should carry resolvable evidence for its acceptance
points. Keep one authoritative state fact and derive display projections
from it. Check explanatory prose for presence, without treating it as
mechanical proof. Atomic's canonical gate supplied the historical pattern;
Kranz's ticket-state and abandonment reconciliation incidents provide local
regression cases. The draft below does not itself change lifecycle policy.

## 3. Consent and causal provenance

Replay should connect authorization to the proposal, action, verification
and resulting diff. Preserve event identifiers for causal links and keep
human resolutions explicit. Summaries can group low-level events for
readability without becoming a second source of truth. This informs the
existing provenance-replay work rather than a separate graph store.

Historical reference vocabulary: Goal, Exploration, Decision, Commitment,
Verification, PatchProposal and Error nodes; `LedTo`, `VerifiedBy`,
`BlockedBy` and `FailedWith` edges; human accept, guide, reject and revise
resolutions. Any schema adoption still needs an explicit contract review.

## 4. Idempotent hooks and metadata boundaries

Hook installation must preserve unrelated settings, deduplicate exact
entries and write atomically. Worker access to mission-control metadata
must follow the containment policy. Record mission/session attribution
without obscuring the human principal. Atomic's Claude hook installer was
the historical source reference. The unfiled deny-projection draft below
predates current fail-closed containment rules; it cannot authorize a
degraded backend that current policy refuses.

## 5. Historical-context trust

Treat retrieved notes as untrusted historical data. Pin the selected source
revision, refuse a changed source and record actual use separately from
retrieval. Combine that provenance with supersede-aware selection within
the existing repo-knowledge scope. Merely looking up a note must not imply
that a worker relied on it.

## 6. Provider signatures

Where a backend exposes an output signature, preserve its original bytes
and provenance alongside the corresponding artifact. Do not fabricate a
signature or infer authenticity without an applicable verification method.
Provider-specific signature fields need their own interpretation and
verification limits before they can support an evidence claim.

## 7. Usage capture

Collect token and cost fields at a backend interface that actually reports
them. Missing usage stays unavailable; it must not become a measured zero.
A fallback source, such as an agent-owned local record, requires explicit
provenance and reconciliation rules before contributing to mission totals.

## 8. Signing and delegation

A signature does not enforce authorization. Any future signed-bundle design
must keep verification separate from permission checks. JCS canonicalization
(RFC 8785), Ed25519 and `did:key` are historical research references, not a
selected or qualified implementation for Kranz.

## 9. Source-control and scope boundary

Kranz retains Git worktrees and gated merge. Replacing Git, introducing a
CRDT merge engine, building a general-purpose agent UI or storing domain
knowledge in core is outside the accepted boundary. Structured projections
must not become a second authoritative event store.

## 10. Adapter capability evidence

Maintain capability tables from version-pinned adapter observations.
Lifecycle hooks, permission requests, usage fields and cleanup behavior
require individual evidence. Atomic's adapter registry was a historical
survey input, not proof that any backend is qualified under Kranz.

## Ticket-ready drafts

Not filed — lift into `.kranz/tickets/<slug>.md` as prioritised.

### `session-attestation-chain`

```markdown
---
title: Chain-linked per-session attestation records, independent of mission lifecycle
priority: 2
schedule: once
---

## Goal
At session/mission end (including abandon), write an attestation record —
per-model token/cost breakdown, wall vs API duration, diff stats, ids of
events/commits covered, and a hash link to the previous attestation of the
same resumed lineage — stored so that mission abandonment or deletion
never removes it, and queryable for coverage (work with no attestation).

## Context
Atomic stores attestations as graph nodes outside any branch changelog so
they survive branch deletion, chained across session resumes
(atomic-core/src/change/attestation.rs; docs/reviews/atomic.md §1). Kranz
equivalent: the evidence spine (KRZ-322/325/326) plus the abandon-
reconciliation finding (m-a5a8fd) — the audit record must outlive the
mission object. Mostly a fold over existing cost/event data; the deltas
are the chain link, lifecycle independence, and the coverage query.

## Acceptance hints
- Tests: abandon still writes the attestation; resume chains to the prior
  one (hash verified); coverage query flags a mission with events but no
  attestation; per-model cost sums match the event-log fold.
- Evidence bundle (KRZ-326) includes the attestation chain.
- Passed-count guard on the named filter.
```

### `ticket-done-granted-not-written`

```markdown
---
title: Terminal ticket/mission states granted by an evidence-checking validator
priority: 2
schedule: once
---

## Goal
Make terminal state transitions (done/complete) a granted operation: a
validator refuses the transition unless required evidence links exist
(mission report, gate results, event ids for each acceptance point), and
duplicated state facts are derived renders of one authoring site, never
second writable fields. Prose fields are checked for presence, never
graded.

## Context
Atomic's intent gate: "status: done is granted by the gate, not written by
the agent"; acceptance criteria marked met must carry verifiedBy +
evidence URN; one authoring site per fact makes contradiction
unrepresentable (atomic-canonical/src/gate.rs; docs/reviews/atomic.md §2).
Kranz precedents: the .status-sidecar vs frontmatter divergence folded by
migrate-state, and abandon/ticket reconciliation (m-a5a8fd) — both are
this bug class. Slots into the gate-plugin lane (KRZ-311/312); no model
judgement involved, mechanical checks only.

## Acceptance hints
- Tests: done without evidence links → refused, naming the missing links;
  with links → granted and the granting check recorded as an event;
  evidence link targets must resolve (no dangling ids); direct frontmatter
  edit to a terminal state without the grant → surfaced by validation, not
  silently accepted.
- Passed-count guard on the named filter.
```

### `worker-metadata-read-deny`

```markdown
---
title: Project a deny rule blocking worker sessions from kranz metadata
priority: 3
schedule: once
---

## Goal
When dispatching via backends that support permission rules
(backend_claude first), project a deny rule preventing the worker session
from reading kranz's own control surfaces in the workspace (gate config,
audit/event metadata, mission control files), so a prompted or curious
agent cannot read the machinery that judges it.

## Context
Atomic's Claude Code installer adds a permissions deny rule over its
metadata directory alongside its hooks (atomic-agent/src/hooks/
claude_code/settings.rs; docs/reviews/atomic.md §4). Kranz equivalent
surface: whatever .kranz/ paths are mounted into the worker's worktree.
Belongs with claude-code-hook-gate-projection (KRZ-302); fail open only
where a backend has no rule surface, and record that gap as a
capability-table fact (KRZ-301/303), not silently.

## Acceptance hints
- Tests: dispatched session settings contain the deny rule; rule survives
  idempotent re-dispatch (no duplicates); backend without a permission
  surface → dispatch proceeds with the gap recorded in the mission events.
- Passed-count guard on the named filter.
```

---

## Recorded follow-ups

The research inputs map to provenance replay (§3), evidence export (§1,
§6, §8), adapter capability tables (§10) and historical-context provenance
(§5). These notes and the three drafts above do not schedule new work.
