# Review: Atomic (atomicdotdev/atomic) vs kranz (2026-08-08)

Atomic is a Rust "Semantic Change Graph" — a from-scratch VCS built for
agent-native development, replacing git's line diffs with patch theory plus a
token-level CRDT, and attaching AI attribution, provenance graphs, session
attestations, and a cross-session knowledge vault directly to the change
graph. Author: Alex Lavaee (per operator; web search corroborates — Applied
AI engineer at Microsoft Research). Apache-2.0, workspace v0.14.0, 10 crates.

Method note: reviewed 2026-08-08 from a clone of
github.com/atomicdotdev/atomic at v0.14.0. The public history is a single
squashed commit, so commit-activity signals are unavailable; maturity was
judged from code and tests (~8,048 test annotations, 24 TODO-class markers
in the core crates, a 27-script e2e shell harness). Every doc claim below
was checked against code by a subagent pass; the doc-vs-code contradictions
are listed because two of them are lessons in themselves.

---

## Positioning read (the headline)

Atomic is the first reviewed project attacking kranz's *own* thesis — that
provenance/evidence is the durable layer while execution commoditises — but
from **below the git line**: it rebuilds the VCS so that attribution, cost,
intent, and the causal "why" live inside the change graph itself. Kranz
attacks the same thesis from **above**: mission-level consent, gates,
event log, and evidence over unmodified git.

| Axis | Atomic | kranz |
|------|--------|-------|
| Substrate | replaces git (patch theory + CRDT) | git-native by identity |
| Unit of record | the change / turn | the mission / event |
| Consent | HumanGate nodes recorded post-hoc | grants gate execution up front |
| Identity | Ed25519 built, largely unenforced | enforced grants, no crypto |
| Adoption ask | new VCS + hooks in every agent | wraps existing repos/CLIs |

The lanes are complementary, not contested: atomic wants to *be* the
substrate; kranz governs work on the substrate everyone already has. The
adoption ask of a replacement VCS is enormous, which protects kranz's lane —
and their existence is fresh validation of the positioning ADR
(`docs/knowledge/decisions/positioning-governance-evidence-layer.md`): a
second team concluded the durable value is provenance, not codegen.

Two of their failures are as instructive as their successes: the README's
"every change carries a cryptographic signature" is false in code (only
vault nodes are signed; changes carry an unsigned pubkey reference), and a
complete Ed25519 delegation model (scoped permissions, budgets, expiry)
is enforced nowhere. Crypto theater is what evidence looks like when
enforcement comes second. Kranz's ordering — enforced consent first, fancy
attestation later — is the right one; keep it.

---

## Verdicts

### 1. Attestations as chain-linked, lifecycle-independent audit nodes — **ADOPT (best single borrow)**

At session end, atomic writes an `Attestation`: per-model token/cost
breakdown, wall vs API duration, lines added/removed, the set of changes
covered, and `previous_attestation` — a hash chain across session resumes.
Crucially these are graph nodes *outside any view/branch changelog*
(`atomic-core/src/change/attestation.rs`), so they survive branch deletion,
and "coverage %" is queryable: which changes have no attestation. The kranz
mapping: a per-session evidence record that exists *independent of mission
lifecycle*, so an abandoned/deleted mission still leaves its audit trail —
exactly the gap the `kranz abandon` reconciliation ticket circles — plus a
mechanical "unattested work" query for the evidence bundle (KRZ-326).
Chain-across-resumes is the detail worth stealing verbatim: resumed
sessions are one accountability object, not N disconnected ones. Draft
ticket below.

### 2. "Done is granted, not written" — **ADOPT (doctrine + validator)**

Atomic's intent gate refuses `status: done` unless every acceptance
criterion marked met carries `verifiedBy` + `evidence` (a URN pointing at
the actual change); the `why` prose must *exist* but is never schema'd
("presence is enforced, content is left honest"); and every fact has
exactly one authoring site — duplicates are renders, so contradiction is
unrepresentable (`atomic-canonical/src/gate.rs`). This doctrine was born
from a real bug of theirs: frontmatter said done, body said backlog, both
"valid." Kranz just lived the same bug class — the `.status` sidecar vs
frontmatter divergence that migrate-state folded away, and the
abandon/ticket-state reconciliation finding from m-a5a8fd. The general fix
is atomic's: terminal ticket states become transitions *granted by a
validator* that demands evidence links (mission report, gate results,
event ids), never a field an agent writes directly. This is the strongest
anti-agent-gaming design in their repo and it slots into the gate-plugin
lane (KRZ-311/312) without new machinery. Draft ticket below.

### 3. Consent as a first-class provenance node — **ADVISORY (schema reference for KRZ-325)**

Their per-turn provenance DAG types Goal, Exploration, Decision,
Commitment, Verification, PatchProposal, Error nodes with causal edges
(`LedTo`, `VerifiedBy`, `BlockedBy`, `FailedWith`) — and makes human
consent events (`HumanGate` / `HumanGateResolution`: accept, guide,
reject, revise) nodes in the same graph, with the resolution propagating
contributor attribution onto downstream Commitments. Kranz's event log
already records grants and escalations; what it does not yet have is the
*causal* claim — this approval led to that commitment led to that diff —
which is precisely what `provenance-replay` (KRZ-325) must reconstruct.
Adopt the taxonomy as the reference vocabulary when KRZ-325's schema is
designed; no separate ticket. Their classify/consolidate pass (raw tool
events rolled up into Decision nodes) is also the right shape for making
replay legible rather than a firehose.

### 4. Hook-installer craft and the metadata deny rule — **ADOPT (small, into KRZ-302)**

`atomic agent enable` merges idempotent, exact-command-deduped hook entries
into each agent's native settings via atomic temp+rename writes — and adds
a Claude Code permissions **deny rule blocking the agent from reading
atomic's own metadata directory**. Agent identity rides plus-addressing:
`claude+60f5 <human@…>` — human accountability preserved, agent and
session visible in every log line without new identity infrastructure.
All three details belong in `claude-code-hook-gate-projection` (KRZ-302):
the worker session must not be able to read (or be prompted into quoting)
kranz's gate config and audit metadata; hook installation must be
idempotent and non-clobbering; dispatched work should carry
mission-tagged authorship. Draft ticket below covers the deny-projection
slice; the other two are acceptance-hint lines on KRZ-302.

### 5. Vault-context trust discipline — **ADVISORY (repo-knowledge lane)**

Their cross-session memory retrieval returns entries labeled
`untrusted_historical_data`, pins pulls to a revision hash and fails
closed if the entry drifted between selection and pull, and writes
provenance `used` edges only for sources actually selected — retrieval ≠
use, so lineage is never faked by mere lookup. Combined with ampcode §6's
supersede-aware retrieval, this completes the spec for
`repo-knowledge-ranked-brief-injection`: recency/contradiction-aware
selection, injection labeled untrusted, revision-pinned, and only *used*
sources entering the evidence trail. Record on that ticket when it lands;
no action now.

### 6. Reasoning-signature capture — **ADVISORY (evidence-bundle detail)**

Their provenance record stores the provider's cryptographic signature over
reasoning blocks (Anthropic's `reasoning_signature`) — provider-signed,
tamper-evident proof that recorded chain-of-thought is genuine. Cheap to
capture where the backend exposes it, and a genuinely stronger evidence
artifact than transcript text alone. One line on `evidence-bundle-export`
(KRZ-326): capture provider signatures over model output where available;
never fabricate the field where not (house rule: no fabricated evidence).

### 7. Per-turn cost capture reality — **VALIDATION (kranz ahead)**

Atomic's "every turn records tokens and cost" holds only for OpenCode
(whose plugin sends them; they even read OpenCode's SQLite store directly
to recover ground truth). Claude Code's hooks expose no token/cost data,
so their Claude turns record model/session/prompt and zero cost. Kranz
captures cost from backend result parsing at mission granularity — the
seam that actually has the numbers. Confirmation that hook-level cost
capture is a dead end for `backend_claude`; keep cost at the
report/event seam, and note their SQLite-recovery trick as the fallback
pattern if a backend's stream under-reports.

### 8. Ed25519 identity and delegation — **PARK (with the one usable piece named)**

Full signing/delegation crates exist (typed permissions with an implies
hierarchy, path/view globs, budgets, expiry, revocation) — and nothing
enforces them; ordinary changes aren't even signed. The lesson is §
positioning: enforcement first. Kranz's grants already *are* enforced
delegation scopes; adding key ceremony now would be theater. The one piece
worth keeping on file: when evidence bundles (KRZ-326) eventually want
signatures, their stack — JCS (RFC 8785) canonicalization + Ed25519 as
`eddsa-jcs-2022`, DIDs as `did:key` — is the standards-track reference,
already proven in their vault-node proofs (`atomic-canonical/src/proof.rs`).

### 9. Views + reflink sandboxes, token-level CRDT merge — **DO NOT CHASE**

Copy-on-write working-tree clones against one shared change graph, and
token-level merges where two agents editing one line don't conflict, are
genuinely clever — and they are substrate features that only make sense if
you own the VCS. Kranz's git worktrees + gated merge deliver the governed
version of the same isolation on the substrate everyone has. Rebuilding the
substrate is atomic's bet and the positioning ADR's frozen zone. Their
honest caveat is also worth quoting into the record: concurrent
multi-process writers are "not load-tested" — even the team that owns the
substrate hasn't proven the fan-out story kranz gets from plain worktrees.

### 10. Seventeen agent-hook adapters — **VALIDATION for KRZ-301/303**

Their `atomic-agent` crate ships hook adapters for 17 agent harnesses
(Claude Code, Codex, OpenCode, Gemini CLI, Cursor, Cline, Copilot, Devin…)
behind one registry/manifest. Independent confirmation that the
heterogeneous-agent surface is real and adapter-shaped — the same bet as
`acp-worker-backend` and `heterogeneous-dispatch-pool` — and a useful
catalog of which harnesses expose which lifecycle events when those
tickets need per-backend capability tables.

### Do not chase (collected)

- **The VCS itself** — patch theory, CRDT merge, views, sandboxes (§9).
- **A triplestore/RDF layer.** Even atomic keeps RDF as a projection, not
  storage; kranz's event log needs the same restraint.
- **An in-house TUI agent** (their "Sherpa" emits the richest provenance
  because they control it). Kranz's leverage is making *external* agents
  legible — the ADR's whole point.
- **Vault as core.** Their vault is the knowledge base inside the product;
  kranz's positioning puts knowledge in packs, core stays domain-free.

---

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

## Follow-ups outside tickets

Advisory only, nothing applied to other docs this session:

- **KRZ-325 (`provenance-replay`)**: adopt the node/edge taxonomy and the
  consent-node attribution-propagation shape from §3 when the schema is
  designed; classify/consolidate is the replay-legibility pattern.
- **KRZ-326 (`evidence-bundle-export`)**: reasoning-signature capture (§6)
  and, if bundle signing ever lands, the eddsa-jcs-2022/JCS reference (§8).
- **KRZ-301/303 capability tables**: atomic's 17 adapters as the survey
  source for which harness exposes which lifecycle events (§10).
- **`repo-knowledge-ranked-brief-injection`**: the §5 trust discipline
  (untrusted labeling, revision-pinned fail-closed pulls, retrieval ≠ use)
  joins ampcode §6's supersede-awareness as that ticket's spec.
- **Roadmap pattern-notes**: an Atomic paragraph belongs beside the
  Warp/Cursor/Amp scans — same-thesis-from-below, adoption-ask moat,
  crypto-theater caution. Not applied here.
