# Bounded native-tool qualification harness

Historical preparation review at `0d41f41`; the later live attempts and
compatibility corrections are recorded in the [follow-up](2026-09-19-acp-native-tool-attempts.md).

This follow-up prepares the remaining S6 live tool-use check. It adds an opt-in
`shell-once` mode to the existing compatibility example and a deterministic
container test script. Ordinary ACP admission, mission permission policy,
client filesystem/terminal capabilities and the release version are unchanged.
No provider has been called and no credential or Keychain has been accessed for
this follow-up. Existing report-only authorizations have already been consumed.

## Workload and evidence

The proposed live batch is one prompt to pinned Claude ACP 0.77.0 / Agent SDK
0.3.270 and one to Codex ACP 1.11.0 / Codex 0.153.4 in the existing ARM64 image.
Each asks its native shell tool to run exactly:

```sh
echo kranz-acp-tool-fixture-v1 > fixture-result.txt
```

Only one exact command, in the session worktree, can receive an `allow_once`
decision. Raw request, synced decision and separate transport delivery receipts
must precede a matching successful completion. No durable option is selected.
The fixture also checks actual file bytes, rejects extra paths and non-regular
files, checks unchanged primary source and protected Git state, then creates
one host checkpoint commit after the worker's confirmed completion. It does not
claim a human merge or a full governed mission.

An empty streamed tool announcement may precede the complete proposal; it never
grants authority. Codex's notification can carry one of six enumerated literal
shell wrappers around the fixed command. Its permission proposal must carry the
exact unwrapped command. No shell parsing, fuzzy matching or display-title
matching expands the allow rule.

The pinned Codex mode named `read-only` actually uses workspace-write permissions
and may execute workspace commands without asking. The fixture requests explicit
escalation and fails without consent receipts; it does not infer enforcement
from the mode label. Claude's pinned source emits an empty Bash input before
refining it and maps its once option without durable metadata. Source pointers
and hashes are recorded in the [proof receipt](../compatibility/acp/tool-probe-proof.json).

The session remains limited to one prompt, 120 seconds for the turn, 180 seconds
for session setup/execution/cleanup, 512 events and 2 MiB of event capture. Local
Git preparation and verification are outside the session timer. The prepared
private live runner adds a 240-second outer deadline, hashes the reviewed binary
and each configuration, records a unique attempt before credential access, and
never retries. Timeout retains uncertainty about cleanup instead of claiming a
pass. There is no hard dollar cap; one ACP prompt can entail multiple provider
requests. Provider allowlists and minimal authentication channels remain as in
the prior report-only checks; there is no Keychain channel.

## Five-axis self-review

- Correctness: pass requires both observed consent/tool evidence and independently
  inspected filesystem/Git results. Synthetic refusals assert their specific
  error, so a generic startup failure cannot satisfy them. The default mode is
  still report-only. Exit failure, unknown exit status when reported, missing
  evidence, repeated consent, changed command or a second tool identity fails.
- Readability: fixture-specific matching and worktree assertions live in an
  example helper; the existing event drain owns durable receipts and the live
  responder. This is explicitly a qualification fixture, not reusable policy.
- Architecture: no production event schema, worker configuration or gate contract
  changes. The existing contained ACP transport and hardened Git APIs are used.
  The local checkpoint is attributed to the probe host, not to the worker.
- Security: tool mode requires the pinned container; credentials stay private;
  selected option extensions and additional authority are rejected; receipt sync
  precedes answer queuing; file reads are bounded and no-follow on the supported
  hosts. The fixed Git initializer clears its environment and disables templates
  and global/system configuration before any worker starts.
- Performance: no new dependency or production runtime cost. The 24 synthetic
  cases run serially against the daemon. Stream/capture limits and session
  deadlines remain bounded; local Git uses its existing bounded executor.

This is an author self-review, not an independent validator verdict. Cooperative
adapter telemetry cannot prove the absence of hidden commands or transient
modify/restore activity. Existing hostile namespace proofs cover the OS boundary;
this small workload qualifies only the observed vendor/image/platform behavior.
Full Linux vendor evidence and the S7 independent-review/repair loop remain open.

## Validation

See the machine-readable [proof receipt](../compatibility/acp/tool-probe-proof.json)
for exact commands, source hashes, result counts and log hashes. The synthetic
suite runs on both the Python fixture image and the pinned vendor image without
invoking either vendor adapter. Linux CI now requires it and retains its console
log on failure or success. This follow-up does not turn synthetic results into
live vendor compatibility claims.

A test-tightening attempt initially required the guest to `stat` the primary Git
configuration before attempting a write. That assertion failed: this fixture's
mount set deliberately leaves the shared Git directory outside the guest. The
corrected check binds the attempted path to the exact host-verified primary
configuration and permits only inaccessible/read-only errors, including ENOENT
for a path absent from the guest namespace. The host still checks that the real
file exists and is unchanged. The failed test attempt remains in the receipt;
no mount or production containment policy was broadened to make it pass.
