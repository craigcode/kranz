# Gate evaluation contract v1

S1 design contract, 2026-09-14. This document and its schemas specify the next
integration slices; they do not enable external gates or change runtime behavior.

Kranz owns authority and evidence. An evaluator supplies a check result; it
cannot authorize an action, change a policy, claim a human identity or waive an
engine floor. The existing `Gate`, `GatePipeline` and `AgentBackend` remain the
integration seams. [Schemas and fixtures](../crates/engine/schemas/README.md)
ship as source data inside the engine crate. Five stages share an envelope with
different typed subjects.

## Decisions from the integration scope

The operator approved proceeding with the recommended release and S1–S3 work.
This record selects the previously proposed D-A–D-H defaults for implementation;
it does not grant a plugin access to production or authorize paid adapter runs.

| Decision | Selected contract |
|---|---|
| D-A | Governance integration through existing seams; S1 specifies data, S3 executes checks, S4–S7 complete consent/containment/end-to-end proof |
| D-B | Stage-specific engine authority; script/model results never substitute for required human consent |
| D-C | Live ACP approval is one invocation only; persistent effects and unsupported options fail closed |
| D-D | One subprocess, one JSON-RPC `gate/evaluate` request, one terminal response, NDJSON stdio |
| D-E | Preserve engine floors, advisory checks, Flight Rules enforcement and exact waiver rules |
| D-F | Certify adapter/platform containment using hostile-process probes; protocol callbacks prove no containment |
| D-G | Fresh process/session per feature attempt; no same-feature resume in this contract |
| D-H | Prove the new capability on macOS, then Linux; Windows remains unavailable until its own evidence exists |

D-H affects the new evaluator and ACP containment capabilities, not the existing
Windows CLI or its tested production gate support. S1 schemas are portable and
perform no process execution. Supported platforms belong to readiness receipts.

## Three different decisions

1. **Evaluation:** an authenticated host identifies the configured evaluator.
   The subprocess reports `judged` with `pass`/`fail`, or `escalate` without a
   verdict. A JSON-RPC error is inability to evaluate, not a failure finding.
2. **Disposition:** the engine combines accepted results with the registration's
   blocking/advisory policy, immutable floors, waiver rules and stage state.
   It records `proceed`, `block` or `require-human`. A score is evidence only.
3. **Consent:** the existing authenticated operator surface records an allowed
   decision for that stage and exact subject. A checker cannot write this record.

An advisory failure stays visible and advisory. An execution error never
satisfies a blocking requirement. A human cannot use this interface to waive a
non-waivable engine prohibition; applicable Flight Rules waivers use the existing
separate, narrowly bound mechanism. Human escalation ends the evaluator process
and enters the durable inbox; it does not keep a subprocess waiting for a click.

A named initiator establishes accountability, not impersonation. Record the
invoking principal, the executing workload/session, any delegated credential
scope and the authorizing principal separately. Effective tool authority is the
intersection of credential rights, approved mission scope and engine tool policy.
Do not copy a human's unrestricted production credential into an evaluator.

Current local REST tokens and signed control files establish capability authority,
not per-person SSO identity. Preserve that distinction in new receipts: an
unattributed local capability must not become `human:operator` from a display label.
Slack can supply its verified user identity through the trusted bridge. Record
the authenticated channel/credential principal and decision source; those facts
do not prove physical human presence. S1 introduces no new SSO or impersonation
mechanism.

## Envelope and subjects

Use JSON Schema 2020-12 for structural validation and the additional host checks
below for relational and filesystem constraints. The schema version is distinct
from JSON-RPC's `2.0` and the pack version. Unknown fields and versions are
rejected. All object keys are case-sensitive. A schema match alone never proves
a correct digest, safe path, process identity or authorization.

The request has `jsonrpc`, string `id`, `method: gate/evaluate`, and `params`.
The JSON-RPC ID names the attempt and must be echoed exactly. Params contain:

- `schemaVersion: 1`, `evaluationId`, `attemptId`, `gateId`, `missionId`;
- `stage` and `subject`;
- `binding` containing `subjectDigest`, `planDigest`, `policyDigest`,
  `registrationDigest` and `workspaceId`;
- `evidence` containing the minimal manifest's relative path and byte digest;
- an absolute UTC deadline, plus engine-selected resource limits.

IDs are opaque nonempty bounded ASCII strings; they carry no path or authority
semantics. The host mints evaluation/attempt IDs, keeps engine run IDs distinct
from peer session IDs and does not recycle IDs across attempts. Hash strings are
`sha256:` followed by 64 lowercase hexadecimal characters. Git object IDs carry
an explicit algorithm, rather than assuming all repositories use SHA-1.

| Stage | Required typed subject | Consuming authority |
|---|---|---|
| `plan-approval` | `kind: plan`, revision, proposed plan digest, pinned base commit | Existing plan consent after checks; no worker session |
| `command-permission` | `kind: invocation`, run ID, peer session ID, tool-call ID, peer request ID, action/options digests, cwd identity | Exact live one-call permission; never `command_grants` or `deny_exceptions` |
| `milestone-validation` | `kind: milestone`, milestone ID, start commit, candidate snapshot digest, criteria digest | Existing independent validation/retry policy |
| `final-gate` | `kind: deliverable`, pinned base commit, candidate snapshot digest, feature-receipt digest | Existing final deliverable checks; not merge consent |
| `merge` | `kind: integration`, live base commit, candidate commit, scratch integration tree, snapshot digest | Existing actual integration-tree checks and required merge authority |

The host rejects mismatched stage/subject combinations, unknown request joins,
wrong session IDs and changed bindings. Any relevant drift creates a new logical
evaluation; a retry against identical bindings gets a new attempt ID. One
registration may declare supported stages, but the response cannot select the
stage it judged or its own place in deterministic-before-model ordering.

## Hash the bytes that were judged

Hashes cover immutable byte objects, not branch names, display strings or a
reserialized approximation. The host writes the subject object to a retained
UTF-8 JSON file and hashes those exact bytes. The request's inline subject must
decode equal to that file. The evaluator need not guess a canonical serializer;
it verifies the provided bytes. Reject duplicate JSON object keys before parsing
so two consumers cannot silently judge different interpretations.

The input builder must resolve every authority binding, not merely accept a
hash-shaped string. Each executable evidence pack includes exactly one subject,
plan, policy and registration object with engine-established provenance. Stage
requirements add the action and offered options for a permission; snapshot and
criteria for milestone validation; snapshot and feature receipts for the final
gate; or the actual integration snapshot for merge. Their hashes must equal the
corresponding typed subject/binding fields. Registration-specific scope, diff,
check receipts and context are additional declared inputs. Missing required
bytes is a readiness/evaluation error. The synthetic schema fixtures deliberately
exercise structure with a smaller inventory and do not satisfy this execution
completeness requirement.

The same rule applies to the minimal evidence manifest and policy/registration
objects. Each manifest entry carries a stable artifact ID, logical role, relative
input path, length and hash. Sort entries by artifact ID and reject duplicate
IDs/paths before freezing the manifest. Reuse the frozen bytes on retry; do not
recreate a "same" manifest with new timestamps and silently retain its ID.

A snapshot inventory records each included path's bytes, file type and executable
mode, plus deletions. Snapshot identity includes permitted dirty and untracked
files; HEAD alone is insufficient. Non-UTF-8 source names must be represented
losslessly by an explicit encoding or fail readiness with a named limitation.
Ignored caches are excluded and cannot be presented as evidence. Symlinks and
submodules need an explicit inventoried representation; no implicit traversal.

Test receipts name their producing run, checked snapshot, command, environment
fingerprint, exit code and executed assertion count when available. An exit zero
with no selected tests remains insufficient where non-vacuity is required. A
worker's claim is not promoted into an engine-observed receipt. Later edits
invalidate earlier receipts unless the engine can prove their full input binding.

## Minimal input versus audit export

The evaluator input contains authorized scope, criteria, pinned changes, check
receipts, prior independent findings and explicitly declared context. Each input
has a provenance entry. Raw worker transcript/reasoning, arbitrary event logs,
provider login state and authority tokens are absent. A source log range is
provenance metadata only and cannot be dereferenced from inside the evaluator.

The full auditor export remains a separate operator product. It may contain more
history under existing redaction/export rules; never pass that bundle wholesale
to an independent validator. Filesystem and network policy must enforce the same
restriction as the input builder. Read-only shared `.git` still exposes history:
external gate inputs must not provide a route through it to forbidden content.

The checker starts in a fresh per-attempt workspace with `inputs/` (read-only),
`outputs/` and `build/` (writable), plus a private scratch home. Request and
manifest paths are relative to this workspace and must start with `inputs/`.
Result artifact paths are relative to `outputs/`; they must not repeat that
prefix or address another root. The host chooses all physical paths and mounts;
the checker does not receive the real checkout or shared Git directory. Trusted
checker/runtime content is separately read-only. S3 must prove these mappings
and access restrictions before declaring a platform supported.

Wire paths use normalized relative POSIX components under an assigned input or
output root. Reject empty/dot/dot-dot components, absolute paths, drive/UNC forms,
backslashes, NUL, ambiguous case collisions and alternate data streams. The host
also rejects trailing dots and reserved device components (such as `CON` or
`NUL.txt`) before mapping these labels to a filesystem. Runtime mapping is
host-owned; a display path never becomes a trusted path lookup.
The host opens every component without following links and verifies regular-file
identity, size and digest through the opened handle. Schema path validation is
only the first check, not filesystem confinement.

Output import verifies raw bytes and the checker-supplied hash, enforces count
and byte caps, then redacts before durable export. Store separate raw-evaluation
and retained-redacted hashes with the transformation identity. Never describe a
hash of redacted bytes as proof of unavailable raw bytes. Missing retained data
is `unresolved`, not regenerated evidence. Private input retention is an explicit
host policy; an external artifact URI does not grant network access.

## Terminal response

The result echoes schema version, evaluation/attempt IDs, subject, policy,
registration and evidence digests. It contains a bounded rationale, an
`artifacts` array (empty when there are no output artifacts) and either:

- `status: judged`, `verdict: pass | fail`; or
- `status: escalate`, with no verdict.

There is no `actor`, `approved`, `blocking`, `waiver` or `stage` field in a result.
Additional fields claiming that authority are invalid. Human action belongs to
the host's authenticated resolution, not untrusted strings in plugin output.
Optional confidence values cannot change a stated verdict or waive a check.

Optional findings carry bounded severity/summary and at least one evidence anchor.
Each anchor names an input artifact ID and its exact digest. Text anchors may add a
one-based inclusive line range; the host verifies order and bounds against the
referenced UTF-8 bytes. LF delimits lines, CRLF is one delimiter, and a terminal
LF does not create an extra line. Empty input has zero lines; bare CR and Unicode
line-separator characters do not create diff lines. Missing artifacts, mismatched
hashes and invented lines are rejected mechanically. An artifact reference is sufficient for binary
evidence; the checker must not invent a text location for it.

A JSON-RPC error contains an integer code and bounded message. Standard parse,
invalid-request, unknown-method, invalid-params and internal-error codes retain
their meanings. Application codes 1001, 1002, 1003 and 1004 mean unsupported
version, missing input, unsupported capability and evaluation failure respectively.
Error data may echo known request IDs; an uncorrelated error can only fail the current process attempt.
An error response never contains `result`, and a result never contains `error`.
A null response ID is allowed only to report an unidentifiable malformed request;
it cannot satisfy an engine-issued evaluation.

The wire is UTF-8 NDJSON: one compact JSON object terminated by LF, no batches,
notifications, progress messages or subprocess-to-host RPCs in v1. Diagnostics
use stderr. The host sends one request, closes stdin, drains both pipes, validates
one response and waits for successful exit and whole-tree cleanup. A pass JSON
followed by nonzero exit, duplicate response, trailing non-whitespace stdout,
overflow, deadline or a surviving child is an unsuccessful attempt. There is
no early acceptance before process completion. Limits are host-selected and
include write time, frame bytes, total output, stderr tail, wall time and output
artifact bytes/count. Cancellation cannot wait indefinitely on a blocked stdin.

## Lifecycle contract-change proposals

These proposals are explicit `contractChangeRequest`s for follow-on slices.
S1 does not modify existing event/state/backend contracts or reinterpret logs.

| Target | Additive proposal | Owner and consumption |
|---|---|---|
| `events.rs` | `gate.evaluation-requested` with complete immutable bindings and attempt/deadline | Engine persists before spawn |
| `events.rs` | `gate.evaluation-finished` with accepted judged/escalated/error status, process exit and artifact joins | Engine after validation and process cleanup; subprocess never writes events |
| `events.rs` | `gate.resolution-recorded` with disposition, policy explanation and separately authenticated consent when required | Engine checks stage authority and accepts at most one resolution |
| `events.rs` | `gate.evaluation-closed` with attempt ID and reason | Engine recovery or explicit retry closes an unconsumed attempt; never replays effects |
| `events.rs` | `gate.resolution-consumed` with the rechecked subject and the action the engine attempted | Engine alone; consumption is not proof that an external action completed |
| `events.rs` | `permission.requested`, `permission.resolved`, `permission.response-recorded`, `permission.closed` | Durable one-call broker, distinct from legacy mission grants |
| `types.rs` | Default-empty maps for pending evaluations/permissions and consumed resolution IDs | Fold validated events; cache is never authority |
| `backend.rs` | A pending-permission event plus a separately callable bounded response handle | Runner can keep draining output and abort while consent waits |

The proposed backend extension is a default-absent
`AgentSession::permission_responder(&self) -> Option<PermissionResponder>` plus
a serializable `AgentEvent::PermissionRequested` payload. The concrete responder
is a bounded in-memory channel handle, not a persisted capability. Existing
backends keep the default `None`; a backend must not advertise pending-permission
support without an operational responder and cancellation path. No new `Worker`
trait or worker scheduler is introduced.

The peer-facing request payload reports only what the adapter actually observed:
peer session/request/tool IDs, raw input and offered options. The engine supplies
mission/run, workspace, plan/policy and deadline bindings from its own state.
A responder accepts the exact correlated resolution only after the broker has
persisted it, validates that the peer request is still live, and returns a
bounded delivery receipt. It cannot mint a principal or widen mission policy.
The response handle never survives replay; restored pending calls whose peer
has died are closed. Existing `abort` remains usable even if no responder exists
or a queued response is stalled. S4 implements and tests this proposal.

For existing approval/final checks, `gate.result` keeps its pass/fail shape and
two legacy surfaces. Optional omitted-by-default join fields can link accepted
judged results to the new lifecycle. Do not invent a legacy `pass` for an error
or escalation. The new stage enum is separate from `GateSurface`; extending an
old field to unrelated stages would silently change existing consumers.

A one-call permission binds raw action/options, request and tool identity, run,
peer session, workspace, plan/policy and deadline. Allow requires an exact offered
certified `allow_once` option; unknown, durable-only and inconsistent choices
cannot authorize. Rejection uses certified `reject_once`, otherwise cancellation;
`reject_always` also has a durable effect and is not a fallback. Reject/cancel
closes that request without widening policy.
Persist resolution before attempting the peer response. Record attempted/sent/
uncertain delivery separately from later tool outcome. A timestamp or successful
pipe write alone cannot prove the effect happened.

## Crash, replay and migration

| Situation | Required behavior |
|---|---|
| Crash before durable request | Nothing authorized; no result is reconstructed |
| Request durable, no terminal result | Interrupted attempt; new attempt permitted only on unchanged immutable bindings |
| Result durable, no resolution | Recompute disposition under the recorded/current required policy; never invent human consent |
| Resolution durable, not consumed | Revalidate subject/policy/deadline and required actor authority before consumption |
| Crash around external action/ACP response | Record uncertain delivery; do not automatically repeat side effects |
| Expiry, cancellation, abandonment or peer death | Close outstanding requests, preserve original deadlines and receipts |
| Duplicate result or click | Accept no second transition; identical retries may return the existing receipt, conflicting payloads fail |
| Plan, candidate, live base, policy or checker code changes | Earlier consent/result does not authorize the new subject |
| Old logs without lifecycle joins | Existing reducers and outcomes unchanged; missing history remains unknown |
| Old configurations and pack schemas 2/3/4 | Existing command gates and advisory/waiver semantics unchanged |
| New external declaration on an old binary | Explicit unsupported schema/load failure; never silently skip a required gate |
| New events on an old binary | Downgrade unsupported for those active missions; retain newer binary, do not rewrite logs |

Replay reconstructs state only. It never spawns a checker, pushes a ref, dispatches
an agent, sends an ACP reply or reapplies an external action. Exactly-once event
transitions do not imply exactly-once external execution. Retry-safe checkers
may rerun as a new attempt; any externally mutating checker is outside v1.

## Validation obligations

Structural fixtures must round-trip all five subjects and judged/escalated/error
responses. Negative fixtures reject stage mismatch, missing binding, unsupported
version, extra authority fields, malformed response and unsafe artifact paths.
Host binding tests must reject changed subject/policy/evidence/registration,
wrong attempt/session, stale deadlines and duplicate object keys. Keep digest
byte tests distinct from schema validation. Each test filter must uniquely name
the new tests, with a positive executed count.

S3 adds process/containment tests, S4 adds consent races and S5 adds legacy-log
replay fixtures. Until those pass, this document is a contract, not evidence that
the runtime enforces its requirements.

References: [integration scope](scoping/acp-worker-gate-contract.md),
[JSON-RPC 2.0](https://www.jsonrpc.org/specification),
[JSON Schema 2020-12](https://json-schema.org/draft/2020-12/json-schema-core).

## S5 foundation checkpoint (2026-09-16)

At this historical checkpoint the lifecycle branch implemented four audit events, pure folding,
source snapshot identity, typed stage input assembly and retained-artifact export
checks. The input builder derives prerequisite status from required checks and
engine-observed receipts bound to content and environment. That checkpoint did not
connect the mission stage drivers; configuration refused external evaluators. See the [review and remaining integration work](reviews/2026-09-16-gate-lifecycle-foundation.md).

The v1 portable path alphabet now includes ASCII dotfiles and interior spaces.
Dot/dot-dot components, empty components, trailing spaces/dots, absolute paths,
backslashes and alternate streams remain invalid. This additive change allows
ordinary source labels such as `.github/workflows/ci.yml` without encoding them
as unrelated filenames.

## S5 approval checkpoint (2026-09-18)

The engine approval driver resolves evaluator declarations and checker files from
its pinned base, freezes the normalized plan and existing advisory diagnostics,
and records request, result, resolution and consumption before `plan.approved`.
Local repository authority and the authenticated local API have distinct actor
attribution. Neither attribution proves physical human presence. A later checker
failure consumes none of the earlier passing checks. Base or mission-branch
drift refuses approval before branch creation or plan commit.

Diagnostic gate verdicts are labelled separately from command execution
receipts. A failed advisory approval lint is not promoted to a blocking floor,
nor converted into a made-up exit code. Raw and retained artifact digests stay
separate. Text retention is scrubbed; binary inputs retain an explicit omission
marker, not a claim to retain their original bytes. Retention writes use
no-follow directory handles and exclusive file creation.

Recovery closes unconsumed attempts, preserving their original results and
consent. It requires a fresh explicit attempt and never reruns a checker or
consumes a saved approval. `gate.evaluation-closed` is an additive contract
change; old records omit `closed` entirely.

That initial checkpoint kept mission admission disabled. The following stage
integration supersedes that restriction; its synthetic proofs make no provider
or release claim beyond the tested native engine and Docker evaluator boundary.


## S5 stage integration (2026-09-18)

Tracked repo-relative evaluators now run at initial and proposed-revision approval,
milestone acceptance, final deliverable checks and local merge on macOS/Linux.
The sealed first approval supplies the immutable checker set. Revisions preserve
that set; the legacy partial re-plan API refuses configured external gates and
requires the proposed-revision approval flow. External command-permission
executables remain unsupported and are rejected at configuration; the separately
implemented S4 broker supplies live one-call consent and its audit joins.

The stage input manifest carries the source log range. Actual command receipts
reference engine decision-event sequence numbers, checked content, environment,
real exits and bounded scrubbed output. Generic shell exits do not claim a test
count. Existing milestone contract commands remain advisory; final commands and
live-base merge commands are required. The existing PTY harness supplies fresh
final evidence. Required test-count checks in the input builder remain fail-closed
when a producer cannot supply the count; stage drivers do not infer counts from
exit zero. Legacy gate.result diagnostics retain their original meanings.

Final evaluation happens after engine report/lesson writes and checks that same
source snapshot. Accepted feature receipts refer to engine-recorded commits and
independent validation runs (or consumed independent milestone checks), never
worker test claims. Merge snapshots are captured before its legacy gate ladder;
checker/manifest drift on the live base refuses the operation. Required merge
commands currently run a second time through the typed receipt runner because
the legacy injectable executor returns only a boolean. That extra cost is explicit.
Both executions must pass, and neither may mutate the integration snapshot.

The single-writer merge audit records consent, attempted consumption and the
actual local merge outcome separately. No new push path exists. A changed source,
base or mission branch closes unconsumed passing attempts. Dropped asynchronous
stage evaluations leave a durable pending request that resume closes; replay
never executes a checker or advances a ref.

CLI status, dashboard and optional Slack display review obligations and evidence
bindings. Escalation requires investigation and a fresh stage attempt after the
cause is addressed; these views do not offer an override for blocking failures.
See the [stage review](reviews/2026-09-18-gate-stage-integration.md) for validation
and remaining acceptance work.
