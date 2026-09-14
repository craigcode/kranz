# ACP worker and gate contract — remaining work

Status: scoped 2026-09-14, reconciled against public `f550630`;
recommendations below are proposed operator decisions, not accepted
implementation contracts. This review made no runtime changes and ran no paid
agent sessions.

The proposal fits the [positioning ADR](../knowledge/decisions/positioning-governance-evidence-layer.md):
Kranz owns dispatch, consent, judgement and evidence. However, the initial ACP
backend (KRZ-301), gate interface (311), pack contract (313), gate results (312),
provenance replay (325) and evidence export (326) already exist. Keep their
completed tickets closed. This scope describes the remaining product work.

## What exists, and what the proposal adds

| Surface | Current implementation | Remaining work |
|---|---|---|
| Worker dispatch | `backend.rs`: `AgentBackend::start(SessionSpec)` and `AgentSession::{next_event, send_user_message, abort}` | Extend this seam deliberately for pending permissions; do not introduce a second `Worker` abstraction |
| ACP transport | `backend_acp.rs`: protocol-1 NDJSON JSON-RPC, initialize/new/prompt, tool/text events, bounded streams, process-group/Job Object supervision | Real adapter compatibility, capability/identity receipts, explicit authentication and selected-model proof |
| ACP permissions | Inline deny-pattern evaluation; unmatched requests allowed; `allowed_tools` not interpreted; denials become tool results | A durable human decision before answering the peer, exact action binding, strict option handling |
| Worker lifetime | Fresh worker specs use `resume: None`; ACP rejects resume; worker-only and opt-in in `config.rs` | Preserve per-feature freshness; optional same-feature recovery later |
| Containment | ACP reports no sandbox enforcement; enforced configurations are rejected | A real wrapper around adapter and descendants, with platform-specific proof; optional client fs/terminal services later |
| Gate checks | `gate.rs`: synchronous `Gate`, deterministic-before-model `GatePipeline`, pass/fail outcome, artefact and optional score | Versioned external evaluation request/response and asynchronous supervision |
| Gate events | `gate.result` records outcomes at `Approval` and `FinalGate`; other lifecycle decisions have their own events | Correlate request, result, resolution and consumption across all stages without rewriting history |
| Human consent | Plan approvals, mission-wide capability grants, standards waivers/attestations; server/Slack/dashboard controls | Reuse authority and inbox infrastructure for a live, one-call permission; avoid changing old grant semantics |
| Packs | Schema 2/3/4; generic command gates; Flight Rules pinning, contextual checkers and human authority | Add an external gate declaration to this contract; keep domain content in private packs |
| Evidence | `validator_snapshot.rs`, `provenance.rs`, `evidence_bundle.rs`, `gate_results.rs` | Build a restricted evaluator input bundle; exported audit bundles include logs and are not suitable validator inputs unchanged |
| Check controls and reviewer policy | `contract_controls.rs` supplies advisory valid/defective controls; `reviewer_independence.rs` enforces configured reviewer separation | Reuse these checks and policy constraints; the open read-back ticket covers its remaining independent interpretation and input isolation |

The current backend is fixture-proven infrastructure, not proof that arbitrary
ACP agents honor every Kranz policy. The completed ACP ticket itself names a
live soak as follow-up. The governance series map's original “new” labels are
historical planning status, not the current implementation inventory.

## Corrections to the source proposal

1. **ACP does not enforce one session per feature.** Kranz must create and bind
   the sessions. It must also control adapter homes, native memory/config and
   any recovery, so a fresh session ID alone is not a context-isolation proof.
2. **Permission callbacks are not guaranteed for every action.** The protocol
   permits an agent to request permission; its tools may execute locally.
   Therefore observing callbacks does not prove all effects are mediated.
   See [ACP prompt lifecycle](https://agentclientprotocol.com/protocol/v1/prompt-turn).
3. **Client file and terminal capabilities are optional services, not a
   sandbox.** Shell commands can write files without `fs/write_text_file`.
   Direct adapter I/O, native tools and MCP children need containment too.
   See [filesystem](https://agentclientprotocol.com/protocol/v1/file-system) and
   [terminal methods](https://agentclientprotocol.com/protocol/v1/terminals).
4. **Cancellation is cooperative; termination is supervision.** Keep the
   existing whole-process-tree kill. A hung stdin write must not delay it
   indefinitely. Pending permission requests must settle on cancellation.
   See [cancellation](https://agentclientprotocol.com/protocol/v1/prompt-turn#cancellation).
5. **Resume is optional, not a kill guarantee.** Current stable v1 documents
   capability-gated `session/resume` as well as `session/load`. The backend's
   “unstable-v2 only” comment is stale; its actual refusal remains intentional.
   Mission-log recovery and restoring an adapter conversation are separate.
   See [session setup](https://agentclientprotocol.com/protocol/v1/session-setup).
6. **ACP framing is newline-delimited JSON-RPC.** Do not import LSP's
   Content-Length framing. See [transport specification](https://agentclientprotocol.com/protocol/v1/transports).
7. **The permission extension is not portable grant authority.** The adapter
   documents describe optional presentation metadata and adapter-owned effects;
   clients return exact offered option IDs. “Always” can change session or
   stored policy. Do not infer scope from labels. See the
   [Claude extension](https://github.com/agentclientprotocol/claude-agent-acp/blob/main/docs/permission-extension.md)
   and [Codex extension](https://github.com/agentclientprotocol/codex-acp/blob/main/docs/permission-extension.md).

The maintained adapters are
[`@agentclientprotocol/claude-agent-acp`](https://github.com/agentclientprotocol/claude-agent-acp)
(Claude Agent SDK) and
[`@agentclientprotocol/codex-acp`](https://github.com/agentclientprotocol/codex-acp)
(Codex App Server). These are first-party project references, not a certification
of a specific installed release. Pin released adapter/runtime versions during
the compatibility slice; main-branch documentation can describe unreleased work.

## Target ACP worker

```text
approved feature + policy + isolated workspace
    → existing AgentBackend / SessionSpec
    → contained ACP adapter → native runtime and descendants
    ↔ bounded transport → normalized events → engine's single log writer
    ↔ permission request → durable consent → exact ACP response
    → report + actual git deliverable → existing validation and merge
```

Keep one adapter process and one fresh ACP session per feature attempt for the
first release. No connection pool, cross-feature context reuse, new planner,
agent goal extension or native subagent scheduler is required.

Persist the engine run ID separately from the peer's session ID, adapter/runtime
identity, negotiated protocol/capabilities and selected model/mode. The current
configured model label is not evidence that the peer selected it. Verify the
structured worker report engine-side; a successful ACP turn is not feature
completion. Keep the nonempty-deliverable and pinned-base gates unchanged.
Record missing cost as unavailable, not free; context-window counts are not
billable input/output usage. Budget behavior when spend cannot be observed is
part of readiness, not an invented token conversion.

Authentication needs its own proof under `agent_env.rs`: the current ACP spawn
forwards no ambient vendor credential. Specify the minimum explicit credential
or native-login material for each certified adapter, preserve private homes,
and keep server tokens, unrelated secrets and host policy outside child reach.
Do not solve compatibility by restoring ambient HOME or environment inheritance.

### Live permission lifecycle

The current callback answers within `AcpSession::next_event`; the current grant
flow parks after a run and widens mission policy for a retry. Connecting those
with a direct call to `approve_pending_grant` would change the authorization.

Introduce a narrow broker at the existing backend/runner seam, specified by a
`contractChangeRequest` before changing `backend.rs`, `events.rs` or `types.rs`.
It must carry run/session/tool/request identity, immutable action and option
fingerprints, policy/plan revision, workspace identity and an absolute deadline.
Reuse the control inbox and authenticated surfaces, with a distinct one-call
scope inside the consent model. Do not repurpose question events or silently
append an ACP approval to `command_grants`/`deny_exceptions`.

Recommended resolution order:

1. An engine prohibition refuses the call; an unknown/unclassifiable action
   cannot be automatically allowed. Shell display titles and wildcard matches
   alone do not prove a command safe. Refresh correlated tool input from the
   exact request, rejecting inconsistent or stale subjects.
2. An already authorized, fully understood action may receive only its certified
   one-time option under pinned policy; record the policy decision too.
3. Otherwise persist the request and await authorized human input. Surface the
   exact action, why it needs consent, and its scope. V1 offers one-time allow
   or rejection/cancellation only. Unsupported effects fail closed.
4. Persist the resolution before returning an exact offered `optionId`. An
   `allow_always`, mode change or arbitrary first option is never a fallback.
5. Expiry, abandonment or peer death closes the request. Duplicate/stale clicks
   cannot authorize a different call. Restart preserves the original deadline
   and audit record; it does not resurrect a dead JSON-RPC request.

The broker must let stdout/stderr, cancellation and controls progress while
waiting. Bound concurrent requests per run and total memory; backpressure cannot
block an abort. Existing buffered parallel-worker logs need a durable path for
permission events before any response, with the engine still the only writer.
Separate operator-wait time from execution-stall time while retaining absolute
permission deadlines and run limits. Record request, decision and response-send
times; a crash after a decision but before sending has an uncertain delivery
state, not an invented completed tool effect.

## Target gate contract

Share a **versioned envelope with typed subjects**, not one untyped `evidence`
bag or one universal human/LLM/script authority. Preserve `GateOutcome` as the
check result and add a separate engine-owned disposition: a check may pass or
fail; policy may allow progress, block, or require human action. A score never
changes the verdict. A plugin may request escalation, never claim human identity.

| Stage | Typed subject / evidence | Authority and integration |
|---|---|---|
| Plan approval | Proposed plan revision, policy digest, base SHA, estimate and contract diagnostics | Checks precede existing plan consent; no worker exists yet |
| Command permission | Run/session/tool/request IDs, action/options digests, cwd and policy | Only the exact pending invocation; no automatic policy widening |
| Milestone validation | Milestone-start SHA, candidate tree, criteria, test receipts, isolated snapshot | Independent deterministic and model checks; existing retry policy consumes failures |
| Final gate | Whole deliverable against pinned base, report, actual changed paths | Retain this fifth internal stage; today's `FinalGate` is not merge |
| Merge | Live base + candidate + exact scratch integration tree, current policy identity | Drift invalidates earlier approval; floors still judge the actual integration tree |

An illustrative external evaluation (placeholder digests here; real hashes and
field definitions belong in the schema slice):

```json
{
  "jsonrpc": "2.0",
  "id": "eval-17-attempt-1",
  "method": "gate/evaluate",
  "params": {
    "schemaVersion": 1,
    "evaluationId": "eval-17",
    "gateId": "synthetic-tests",
    "stage": "milestone-validation",
    "missionId": "m-example",
    "policyDigest": "sha256:<approved-policy>",
    "subject": {
      "kind": "milestone",
      "milestoneId": "ms-2",
      "baseSha": "<milestone-start-sha>",
      "candidateTree": "<judged-tree-id>",
      "planRevision": 1
    },
    "evidence": {
      "manifestRef": "file:gate-inputs/eval-17/manifest.json",
      "digest": "sha256:<input-manifest>",
      "sourceLogRange": [1201, 1387]
    },
    "deadline": "2026-09-14T18:00:00Z"
  }
}
```

```json
{
  "jsonrpc": "2.0",
  "id": "eval-17-attempt-1",
  "result": {
    "schemaVersion": 1,
    "evaluationId": "eval-17",
    "evidenceDigest": "sha256:<input-manifest>",
    "verdict": "pass",
    "rationale": "All declared assertions executed successfully.",
    "artefacts": [{"path": "results.json", "sha256": "<result-digest>"}],
    "requiresHuman": false
  }
}
```

The host binds the evaluator identity, implementation/config digest, stage,
ordering, timestamps and any authenticated human actor. The plugin cannot choose
its own blocking/advisory status or claim `human:operator`. An evaluator unable
to judge returns a typed error or escalation, not a fabricated pass/fail; the
new lifecycle records that status without coercing it into a legacy verdict.

### Evidence and execution rules

- Build a minimal immutable input manifest: authorized scope, criteria, candidate
  diff/tree, test commands and raw exit codes, prior independent findings and
  declared context. Hash actual judged bytes including dirty/untracked inputs
  where allowed; a moving branch name or HEAD alone is insufficient. Fresh
  checks bind their receipts to that input. Prior test evidence is labelled by
  its producer, revision and environment rather than accepted on a worker claim.
- A model validator receives neither worker transcript/reasoning nor unrestricted
  access to the mission log. `sourceLogRange` is provenance, not read authority.
  Restrict files, native session state, shared `.git` routes and network access
  as well as prompt contents. The operator's audit view may expose more.
- Use `validator_snapshot.rs` and containment for evaluation. Read-only source
  plus writable build/output scratch is preferable; when a checker needs a
  writable source snapshot, record mutations and keep the input identity fixed.
  Neither validator nor external gate may mutate the actual candidate or main.
- Spawn a pinned executable with argv, cleared environment and an explicit
  capability policy. Resolve approved gate code/dependencies from trusted base
  content, never the worker-edited copy of the same path. Pin executable content
  and referenced checker assets, not only the manifest's command string.
- V1 subprocess transport: one process per evaluation, one `gate/evaluate`
  request and one terminal response, NDJSON JSON-RPC on stdout, diagnostics on
  stderr. Unknown schema, invalid/duplicate response, nonzero exit, missing
  response, oversized output or deadline failure cannot satisfy a blocking gate.
  A valid-looking pass followed by a failed exit is not a successful evaluation.
- Reuse `command_exec.rs` sandbox/environment/supervision primitives; add structured
  stdin/stdout handling rather than parsing a shell command's prose. Supervise
  asynchronously so human waits never block `Gate::evaluate` or the mission
  control loop. Bound frames, runtime, output, children and artifact sizes.
- Accept artifacts only from the designated output directory via no-follow
  traversal. Import and redact them before computing durable export hashes;
  distinguish raw evaluation fingerprints from hashes of retained redacted
  evidence. Missing retained bytes remain explicitly unresolved during replay.
- The engine pins registrations at approval and rechecks relevant live-base
  policy at merge. Packs add to engine floors. Preserve current advisory pack
  checks and Flight Rules enforcement/waiver semantics. No migration silently
  turns every advisory failure into a block or lets a plugin waive an engine floor.

### Durable lifecycle and migration

Specify additive request/result/resolution/consumption events linked by
evaluation ID, attempt ID, subject digest and policy digest. Reuse
`gate.result` for existing pass/fail evidence; do not silently redefine its two
legacy surfaces or use it as a new state transition. Add new lifecycle payloads
and optional joins under a deliberate contract change, with old-log fixtures.
Expose approval, permission, milestone, final and merge as explicit new stage
values; join existing `plan.*`, `grant.*`, `validation.*` and merge records.

An engine accepts at most one resolution per request and rechecks its binding
before consumption. Replays only reconstruct state; they never re-execute a
gate or send an ACP permission response. A restart may rerun an interrupted
checker as a new attempt against the same snapshot; it must not claim exactly
once external execution. Human escalation parks in the durable inbox, not a
long-lived plugin process. Existing CLI, server, Slack and dashboard remain
clients of the same authority checks.

## D-X — operator decisions before implementation

| Decision | Recommendation | Consequence |
|---|---|---|
| D-A: What is v1? | Contained ACP worker + human one-call permissions + subprocess checks + stage/evidence joins | Finish the governance integration without rewriting the engine |
| D-B: Who may approve? | Preserve stage-specific authority; models/scripts evaluate, humans retain required consent | Replaceable evaluators do not make approvers interchangeable |
| D-C: ACP option scope | Certified one-time operations only; reject unsupported effects | Persistent grants, mode changes and permission-extension effects are separate work |
| D-D: Wire protocol | Versioned one-shot JSON-RPC evaluation over NDJSON stdio | No plugin daemon, registry marketplace or remote transport in v1 |
| D-E: Enforcement migration | Preserve current floors, advisory checks and exact waiver rules | A common envelope cannot silently alter mission outcomes |
| D-F: Containment support | Certify adapter/platform pairs only after hostile-process probes | Unsupported combinations keep their current explicit refusal; fs/terminal RPC is never sufficient proof |
| D-G: Recovery | Fresh feature attempts first; same-feature ACP resume later | Mission replay works without restoring a provider transcript |
| D-H: Platforms and pilot | First prove on the operator's macOS host, then Linux; Windows either proven or explicitly unavailable for the new capability | Whole-tree kill stays cross-platform; no unsupported enforcement claim |

These are recommendations for plan approval, not blockers to this scoping work.
Live adapter runs still need a bounded fixture, versions, model budget and
operator-authorized credentials. Scoping does not dispatch them.

## Delivery slices and effort

Estimates are engineering days for one experienced contributor including tests
and review, not agent wall-clock predictions. Adapter incompatibilities and
platform containment are the main uncertainty. Dependencies below are encoded
in the new tickets; none supersedes a completed foundation ticket.

| Slice / ticket | Depends on | Deliverable | Days |
|---|---|---|---:|
| S1 `gate-evaluation-contract-v1` | — | Typed subjects, schemas, authority matrix, evidence/lifecycle contract changes | 2–3 |
| S2 `acp-adapter-compatibility-proof` | — | Pinned adapter fixtures, auth/model/report/cancel receipts and readiness | 3–5 |
| S3 `gate-subprocess-evaluator` | S1 | Contained one-shot executable gate and synthetic pack declaration | 4–6 |
| S4 `acp-live-permission-consent` | S1, S2 | Broker, durable one-call decisions, CLI/server/Slack/dashboard surfaces | 5–8 |
| S5 `gate-lifecycle-evidence-integration` | S1, S3 | Stage adapters, restricted inputs, policy binding, replay/export | 4–7 |
| S6 `acp-worker-containment-proof` | S2 | Real wrapper and hostile-process tests for supported adapter/platform pairs | 4–7 |
| S7 `acp-governed-mission-acceptance` | S4, S5, S6 | Seeded end-to-end mission, crash/race proof, private-pack seam demonstrated synthetically | 3–5 |

Core v1: **25–41 engineering days (roughly 5–8 weeks)**, contingent on D-A…D-H.
Start S1 and S2; S3/S5 and S4 can then progress independently, with S6 required
before claiming containment. First useful delivery is a synthetic script gate
judging pinned evidence, followed by the live permission workflow. Do not make
ACP the default merely because its protocol tests pass.

Two later slices complete the broader proposal and need separate scopes:

- **Client fs/terminal mediation (7–12 days)**: secure path handles, session
  ownership, touch-set checks, secret handling, terminal create/output/wait/
  kill/release, bounded output and cancellation. Reuse the shipped supervisor;
  no terminal UI. Demonstrate that direct I/O cannot bypass any claimed
  mediation. Secret scanning at a text-write RPC cannot cover shell writes.
- **Same-feature session recovery (3–5 days)**: negotiated load/resume, durable
  native state, exact workspace/policy binding, history deduplication and cost
  deltas. Lost permission requests expire; recovery never replays authorization
  blindly. No cross-feature or worker-to-validator session reuse.

Including those later slices: **35–58 engineering days (roughly 7–12 weeks)**.
Persistent adapter permission changes, remote gates, hosted checks credentials,
new glasses interaction design and domain-specific validators remain separate.
The existing glasses REST consumer can reuse the new decision API when ready.

## Acceptance and review bar

One synthetic mission must approve a plan, run a contained ACP feature, pause
for a one-time permission, execute deterministic checks before independent
validation, reject a seeded defect, accept its repair and judge the actual
scratch merge tree. It must produce a nonempty deliverable and a portable
audit record explaining authorization, changes, checks and remaining human work.

Prove negative paths as well: stale/duplicate/wrong-session permission replies;
unknown options and durable-option-only requests; changed tool subjects; missing
cost/model evidence; silent/hung/oversized peers; restart before/after response
send; expired requests; forged human actors; policy drift; changed checker code;
malformed/failed plugin exits; artifact traversal; validator transcript access;
out-of-root writes, token reads and forbidden network effects. Primary checkout
stays byte-untouched, no push occurs, and no empty deliverable completes.

Review every slice for correctness (binding/races), readability (typed subjects),
architecture (existing seams/floors), security (containment/authority) and
performance (bounded streams, snapshot cost, human waits). Use unique nonvacuous
test filters and run the repository's full workspace gates. S4 also runs all
dashboard gates and synchronizes the embedded bundle. Live proof receipts state
versions/platforms and distinguish tested support from unavailable combinations.

## Validation of this scoping change

Document links, JSON examples, ticket frontmatter and the dependency graph
were checked on 2026-09-14. The inventory was reconciled against public main,
including the shipped contract controls and reviewer-independence policy.
Workspace regression results belong to the integrating pull request; they do
not establish live ACP compatibility or acceptance of the proposed contract.
No runtime/dashboard code changed and no paid agent session was dispatched.
