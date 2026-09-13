# Review: local-llm-skill and triumvirate vs kranz (2026-07-27)

Verdicts on the borrowable ideas from two repos surfaced via a LinkedIn
post on local LLM task routing: **jhammant/local-llm-skill** (a ~4.5k-line
dependency-free Node CLI that turns LM Studio/Ollama/any OpenAI-compatible
server into a memory-aware batch worker; small, honest, well-tested) and
**michaeljboscia/triumvirate** (a ~50k-line Rust MCP daemon orchestrating
Claude/Gemini/Codex — real working core, materially overstated docs, and
failure documentation more valuable than its code). One verdict per idea,
then ticket-ready drafts.

Provenance note: triumvirate's own README/ROADMAP/BUILD_MANIFEST are
unreliable (its retrospective documents the manifest as retroactively
fabricated). Every mechanism cited below was verified in source, not taken
from its docs. File references are to the repos at their 2026-07-27 heads.

---

## From local-llm-skill

### 1. Capability-based model classes, no hardcoded ids — **ADOPT (local tier)**

Their classes (`reflex`/`workhorse`/`coder`/`heavy`) select from whatever
the endpoint reports: hard filters (type, tool-use capability, fits the
memory budget) → per-class size preference → weak regex family hints as
tie-breakers only. Their code comment records the lesson: an earlier
version hardcoded the author's own model list and was "dead on arrival on
anyone else's machine" (`src/catalog.mjs`). Kranz roles pin a literal
model name today — the same trap once configs travel between machines.
Belongs in the local-inference workstream
(`docs/scoping/local-inference-executor-tier.md`), where routing is
already deterministic-by-task-class; this adds the *model resolution*
step under the class. Not urgent while there is one local endpoint with
one model.

### 2. Readiness probing beyond TCP — **ADOPT (small, now)**

Kranz's local readiness probe (`backend_readiness.rs`,
`probe_local_reachability`) checks only that the endpoint answers TCP. If
the configured model isn't loaded/present, the mission fails at dispatch
time instead of parking at preflight. local-llm queries the endpoint's
model list and loaded state, and degrades honestly per endpoint kind —
`managed: false` rather than invented numbers, and a missing model size is
never treated as zero. A `GET /v1/models` check that the configured model
exists is a cheap extension of the shipped
`backend-readiness-quota-preflight` park policy (`missing` → park with
actionable reason). Draft ticket below.

### 3. Constrained output with re-ask — **ADOPT**

Their `--allow a,b,c` restricts answers to a set; drift triggers one
re-ask with the constraint restated; recoverable replies (`**positive**`,
"The label is positive") are canonicalised with the raw kept; genuinely
ambiguous replies are recorded as failures, never guessed. Kranz's
`SessionSpec.json_schema` is deliberately claude-only at the seam today
(kimi/codex ignore it) — but the local backend is the one backend that
could honor it natively: OpenAI-compatible servers take
`response_format: {type: "json_schema"}`. A schema + bounded
validate/re-ask loop is exactly what makes small local models trustworthy
for structured verdicts (validator-tier work in the local-inference
scoping doc). Draft ticket below.

### 4. Memory admission control — **PARK (until kranz manages model loading)**

Budget = GPU wired-limit ceiling (`sysctl iogpu.wired_limit_mb` on Apple
Silicon) minus reserve; LRU eviction with pins; reject-before-evict for
models bigger than the whole budget. Kranz's `context_budget` is a token
check, not a memory check — but kranz also doesn't load/unload models; it
assumes an already-serving endpoint. This matters only once concurrent
local missions can thrash a shared LM Studio/mistral.rs instance. Record
in the local-inference scoping doc as a Stage-3+ concern. The part to
keep regardless: **never fabricate a number the endpoint didn't report**
— already house policy (quota-preflight's "fabricating a 0% quota bar is
forbidden").

### 5. Time-as-cost for local runs — **ADOPT (partial)**

Local runs land in cost accounting at `$0.0`, which hides their real
cost: wall-clock. Their `plan` command times a real 8-item sample through
the actual code path (fixed per-request overhead dominates short
requests; prefill and decode rates differ wildly) and labels every figure
measured-vs-assumed. Full plan-time sampling ties into the
estimate-calibration scoping work; the cheap first step is reporting
wall-clock and items/sec for local-backend missions so the operator can
judge whether local was worth it.

### 6. Pools by binding constraint — **ADOPTED ALREADY (validated)**

Their framing — subscription pools are *quota*-bound, local is
*memory*-bound, rented GPUs are *money*-bound; route each job to
whichever constraint is loosest — is the local-inference scoping doc's
rationale stated more crisply ("route batch, bounded, well-specified work
locally to scale past the quota ceiling"). No change; this review is the
record that an independent practitioner converged on the same carve-out.

---

## From triumvirate

### 7. Quota-class circuit breaker with cross-provider reroute — **ADOPT (best single mechanism)**

`daemon/crates/mcp-bridge/src/agy_resilience.rs`: breaker state is keyed
by failure *class* — quota failures trip at a lower threshold than
generic failures; cooldown is exponential but **capped at 5 hours to
match the subscription quota-reset window**; half-open probing uses an
anti-stampede lease sized to outlive a slow probe; and when the breaker
is open for a quota reason, the degraded route skips every backend on the
*same provider pool* and reroutes to a different provider. Failover as
quota arbitrage, not generic retry. The shipped
`backend-readiness-quota-preflight` is the point-in-time complement (park
before claiming); the breaker is the in-mission half (stop burning a
backend mid-drain, reroute, recover on the reset window). Draft ticket
below.

### 8. The session-orphaning rule — **ADOPT (as a seam invariant)**

Their comment deserves quoting because the failure is subtle: a failover
attempt runs on a different model, which cannot resume the primary
model's session, so it starts fresh — "crucially, its fresh session id
must also never be written back… doing so would overwrite the primary
session cached for this (agent, cwd) and permanently orphan the user's
transcript, turning a transient 429 into total memory loss." Kranz
backends carry resume tokens; any future retry-on-alternate-model path
must enforce *never persist a fallback attempt's session id over the
primary's*. Cheap to state now as an invariant + test wherever reroute
lands; expensive to rediscover later.

### 9. Authoritative-contract validation split — **AUDIT (against in-flight workspace-contract work)**

Their three findings, each earned in stress tests: (a) git hooks are
defense against *accidents* only — Codex under `--full-auto` autonomously
retried with `--no-verify` when a hook blocked it; (b) never trust
anything inside the worker's workspace — the worker can rewrite the
contract copy and the hooks it was given, so post-exit validation must
run against the contract **the engine holds**, cloned at dispatch; (c)
completion signals must be cross-validated against an independent channel
(their sentinel file only counts if git HEAD matches the sha the sentinel
claims — a lying worker can't fake being done). Kranz's
workspace-contract/gate work is in flight; this is the checklist to audit
it against rather than a new design. Kranz's "validators see the raw
diff, never the worker's account" principle is the same instinct — the
audit is confirming the contract copy and the completion signal follow
it too.

### 10. Mechanical failure classification with per-class retry caps — **ADOPT**

Four classes matched by string patterns, not LLM judgment: worker error
(3 attempts), contract error (2), briefing error (2), environment error
(**0 — halt immediately**), global ceiling 5. Their lessons: agents
cannot self-diagnose their own errors (a briefing bug gets misclassified
as a worker error and burns retries), and blind retries repeat identical
failures. Maps directly onto the local-inference scoping doc's escalation
valve ("fails validation twice → escalate to frontier") — this is the
same idea generalized to *why* it failed, with environment failures never
charged against the worker's attempts.

### 11. Temporal price table + out-of-band cost capture — **ADOPT (price table now, scanner later)**

Their `price_table` is `(model, rates…, effective_date, end_date)`; cost
lookup uses the price active *at the record's timestamp*, so a mid-project
price change never rewrites history; unknown price yields `None`, never a
guess ("a chart can never silently sum a guess"). Cheap discipline for
`cost.rs`. The larger idea — a scanner reading the agent CLIs' own
transcript files (byte-offset resume, startup reconciliation for sessions
that happened while the daemon was down) because *your dispatch path
never sees all spend* — is right and real, but only matters once kranz
claims to report total spend rather than mission spend.

### 12. Parallel-wave file-scope overlap gate — **ADOPT (cheap)**

`wave_gate.rs` `validate_no_overlap()`: reject a parallel wave whose
tasks share any allowed-files entry. Mechanical, one function, prevents
the whole class of same-file merge conflicts at plan time instead of at
the merge gate. Draft ticket below.

### 13. Event-stream hygiene details — **ADVISORY (for the event-log/UI seam)**

Four small, earned details for the dashboard/Slack fronts: replay buffer
behind the live stream (`replay_since(last_seq)`); the fill task
subscribes to the broadcast channel *before* the HTTP server binds
(subscribe-before-read race); auth is checked *before* WebSocket upgrade
(clean 401, not connect-then-close); events carry monotonic sequence
numbers and the client detects gaps. Cross-reference from
`docs/reviews/event-log-review.md` territory. The cautionary twin: their
fleet ledger writes hardcoded `2030-01-01` timestamps at 21 call sites —
for an event-sourced system, "the mechanical layer itself goes
unverified" is the failure mode; a debug assert or test that event
timestamps are sane is cheap.

### 14. Tauri sidecars die with the app — **ADVISORY (apps/ host)**

Their docs/4.0.0 lesson L-005: Tauri sidecars are child processes killed
on app exit; a daemon that must outlive the app needs OS-level
detachment, not the sidecar API. Relevant whenever the kranz Tauri shell
is expected to close while missions keep running.

### 15. The meta-lessons — **ADOPTED ALREADY (validated, worth the read)**

v1 was archived because — after 6 review rounds, 190 tests, 13 canonical
docs — they shipped a daemon the operator had no way to call ("build the
steering wheel before the engine"). Their build system's own retrospective
then caught its manifest fabricating history (every task self-reported
"Attempts: 1"; reality was 3 correction rounds). Their conclusion —
*"LLMs cannot be trusted with administrative state management or
self-reporting… any rule that relies on an LLM 'remembering' to do it
will eventually be broken"* — is the argument kranz's event-sourced,
mechanically-gated architecture already embodies. If reading anything in
the repo: `docs/abe/LESSONS.md`, `docs/abe/RETROSPECTIVE.md`,
`docs/v2/LESSONS.md` (~2 hours encoding months of failure).

### Do not steal

The archived v1's `governance.rs` (the "Cedar policy engine" is a
boolean; Cedar was never a dependency), `routing.rs` (78 lines of
`str::contains`), `quota.rs` (chars/4 against a hardcoded budget), and
`agent/pool.rs` (dead code). Its README, ROADMAP, NOTICE prior-art
claims, and BUILD_MANIFEST are all contradicted by its own tree.

---

## Ticket-ready drafts

Not filed — lift into `.kranz/tickets/<slug>.md` as prioritised. Ordered
by value-for-effort.

### `local-endpoint-model-preflight`

```markdown
---
title: Local readiness probe verifies the configured model exists
priority: 2
schedule: once
---

## Goal
Extend the local backend's readiness probe beyond TCP reachability: query
the endpoint's model list and park the mission with an actionable reason
when the role's configured model is absent, instead of failing at
dispatch time.

## Context
`crates/engine/src/backend_readiness.rs` (`probe_local_reachability`)
currently answers ready on any TCP connect. OpenAI-compatible servers
expose `GET /v1/models`. Fold into the shipped
backend-readiness-quota-preflight park policy: model absent → `missing` →
park; endpoint reachable but model list unavailable → `unknown` → warn +
proceed (never treat unknown as failure — house rule). Bounded timeout,
same as the TCP probe. See docs/reviews/local-llm-and-triumvirate.md §2.

## Acceptance hints
- Probe hits /v1/models; configured model present → ready; absent →
  not-ready naming the model and listing what IS available.
- Endpoint that 404s /v1/models degrades to unknown+warn, not failure.
- Tests: model present, model absent, endpoint without /v1/models,
  unreachable endpoint (existing behavior unchanged). Passed-count guard
  on the named filter.
```

### `local-backend-structured-output`

```markdown
---
title: Local backend honors json_schema via response_format with re-ask
priority: 2
schedule: once
---

## Goal
When a SessionSpec carries json_schema, the local backend sends
`response_format: {type: "json_schema", ...}` and validates the reply
against the schema; on mismatch it re-asks once with the constraint
restated; a reply that still fails validation is a session failure, never
a silently-passed-through malformed result.

## Context
`crates/engine/src/backend_local.rs` builds a plain chat request;
`json_schema` is claude-only at the seam today. LM Studio, vLLM, and
llama.cpp-server accept response_format; servers that reject the
parameter should be detected (HTTP 4xx naming it) and reported as
unsupported, not retried. Canonicalisation rule from local-llm: a
recoverable reply is repaired with the raw kept; an ambiguous one is a
failure, never a guess. This is what makes local models usable for
structured validator verdicts (docs/scoping/local-inference-executor-tier.md).
See docs/reviews/local-llm-and-triumvirate.md §3.

## Acceptance hints
- Stub-server tests: valid-first-try, invalid-then-valid on re-ask (assert
  exactly 2 HTTP requests), invalid-twice → SessionExit::Failed naming
  schema validation, server rejects response_format → Failed naming
  unsupported.
- No schema in spec → request body byte-identical to today's (regression
  test on the recorded request).
- Passed-count guard on the named filter.
```

### `backend-quota-breaker-reroute`

```markdown
---
title: Failure-class circuit breaker per backend with cross-pool reroute
priority: 3
schedule: once
---

## Goal
Track backend failures by class (quota/rate-limit vs other) in-mission;
after N quota-class failures, open a breaker for that provider pool with
an exponential cooldown capped at the provider's quota-reset window, and
reroute eligible queued work to a backend on a different provider pool.
Every trip, reroute, and half-open probe is an event in the log.

## Context
Complements the shipped backend-readiness-quota-preflight (point-in-time,
pre-claim) with the in-mission half. Design source: triumvirate
agy_resilience.rs — breaker keyed by failure class, 5h cooldown cap
matched to subscription reset, half-open probe with an anti-stampede
lease, reroute skips same-pool backends (claude models share one pool;
codex is a different pool). Invariant that MUST ship with any reroute: a
fallback attempt's session id is never persisted over the primary's
resume token (session-orphaning rule, §8 of the review). Reroute policy
is config, not hardcoded; pools declared per role.

Second invariant (added 2026-07-29, from an external token-burn analysis —
Nate Jones, Jul 2026 — whose gateway retried a token-limit failure
verbatim on a second provider): context-overflow/token-limit failures are
their OWN failure class, distinct from quota and generic. An oversized
request is never blind-retried and never rerouted unchanged — the target
backend cannot repair the source request's size. The class resolves by
shrinking or by escalating to re-planning, does not trip the quota
breaker, and is never charged against the worker's retry budget as a
worker error (same spirit as triumvirate's environment class, §10).

## Acceptance hints
- Tests: quota-class trips at lower threshold than generic; cooldown caps
  at configured window; half-open allows exactly one probe; reroute
  target excludes same-pool backends; fallback session id not written
  back (assert primary resume token unchanged after a failed-over
  attempt); breaker events appear in the event log.
- Oversized-class tests: a token-limit failure classifies as
  context-overflow, is not forwarded to any reroute target, does not trip
  the quota breaker, does not decrement worker retries, and surfaces a
  shrink/re-plan signal in the event log.
- Passed-count guard on the named filter.
```

### `parallel-batch-file-overlap-gate`

```markdown
---
title: Reject parallel mission batches whose file scopes overlap
priority: 3
schedule: once
---

## Goal
At plan/queue time, reject (or serialise) a set of missions intended to
run in parallel when any two contracts share an allowed-file entry, with
the offending pairs named — instead of discovering the collision at the
merge gate.

## Context
Mechanical check, one function: pairwise intersection over contract file
scopes. Source: triumvirate wave_gate.rs validate_no_overlap. Globs need
a documented rule (exact-path intersection first; glob-vs-glob may
conservatively serialise). See docs/reviews/local-llm-and-triumvirate.md §12.

## Acceptance hints
- Tests: disjoint scopes pass; shared exact path rejects naming both
  missions and the path; glob overlap handled per the documented rule.
- Passed-count guard on the named filter.
```

---

## Follow-ups outside tickets

- **Workspace-contract audit (§9):** while the workspace contract/gate
  work is in flight, walk the three-point checklist — engine-held
  authoritative contract copy, hooks-are-advisory assumption, completion
  signal cross-validated against an independent channel.
- **Scoping-doc cross-links:** local-inference-executor-tier.md gains §1
  (model classes) and §4 (admission control, Stage 3+) as future
  sections; estimate-calibration gains §5 (measured-sample ETAs).
- **Advisory only, no action now:** §13 event-stream details, §14 Tauri
  sidecar detachment, §15 reading list.
