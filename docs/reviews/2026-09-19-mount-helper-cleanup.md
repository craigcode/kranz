# Mount helper cleanup review

The concurrent ACP fixture previously reached the 90-second bind-mount deadline
and left its trusted sentinel helper running. The original failed batch remains
in [the concurrency receipt](../compatibility/acp/cleanup-concurrency-proof.json).
This follow-up owns the preflight container independently of its Docker client.
No provider adapter or credential is involved.

## Change

Before creation, a private unmounted ledger records a random owner label, image
ID, helper name, host PID and probe directory. Docker control uses the existing
bounded process runner with a cleared, frozen client environment. Creation and
attached execution share the existing 90-second deadline. The image is inspected
as Linux with no anonymous volumes, then frozen to its ID; a missing image keeps
the ordinary preflight pull behavior within the same deadline.

The helper overrides the image entrypoint and runs a fixed shell watchdog as
PID 1, with no network, no capabilities, no privilege escalation, a read-only
image and only the fresh mode-0700 sentinel directory mounted writable. The
payload child performs filesystem I/O. The watchdog signals the parent after
85 seconds without touching that mount; PID-1 exit ends the private namespace.
The helper uses the sentinel directory owner's UID/GID, matching the existing
worker/relay mapping and preserving access without DAC override on Linux. Image
health checks are disabled, and Docker's automatic proxy environment injection
is explicitly cleared. No host credentials enter the guest.

Cleanup inventories immutable owner labels and full IDs, rejects ambiguous or
mismatched identity, and removes only those IDs. It requires a successful empty
inventory, polling through Docker's asynchronous auto-removal. Cleanup is bounded
by 15 seconds, including a five-second absence window; normal process reaping has
the existing bounded runner's allowance. A failed proof never becomes successful
because removal succeeded. Cancellation and dropped futures retain ownership.

An interrupted create with no observed object stays unconfirmed because a late
object can appear. Engine death between create and start can leave a stopped
object. Both leave private recovery intent rather than claiming absence. Engine
death during execution is bounded by the guest watchdog; its ledger remains for
reconciliation even when `--rm` has removed the container. Unreachable daemon
cleanup fails closed. There is no automatic broad prune or promise to recover
from an unresponsive kernel. Non-Docker runtimes refuse required mount preflight
before spawn; ordinary Linux paths using the existing CI contract are unchanged.

## Regression evidence

The proof family `mount_helper_v1` exercises successful two-way sentinels, four
concurrent probes, a FIFO-blocked writer, host timeout, guest watchdog expiry,
explicit cancellation, dropped futures, interrupted creation after a real daemon
object exists, engine SIGKILL, name collision and name reuse. A real successful
sentinel exchange followed by a deliberately unavailable inventory must fail and
retain a mode-0600 recovery record. Deterministic controls additionally reject
malformed inventory, a mismatched full ID, empty inventory after interrupted
creation, and deletion acknowledgement without absence.

The Linux Docker CI job requires each real proof by exact test name and rejects
the skip marker. Local checks use an empty Docker credential configuration;
no Keychain request, vendor credential read or model call is part of this work.

One final-code workspace attempt failed the existing native ACP test
`acp_compat_v1_handshake_rejects_version_and_missing_session_identity`: its outer
three-second test timeout elapsed. All eleven mount-helper tests passed in that
attempt. The unchanged handshake test passed alone in 0.64 seconds. No container
path runs in that test; the exact cause of the timing failure is unproven. The
failed attempt is retained separately from the subsequent full gate result;
neither the production handshake deadline nor the test timeout was relaxed.

The first eight concurrent end-to-end invocations failed before ACP initialization
because the new wrapper canonicalized a declared mission parent before creating
it. The previous probe created missing parents. That behavior is restored, and
the real round-trip test now includes a nested, initially absent mount root.
The local harness also explicitly places `TMPDIR` under Colima's shared scratch
base, matching the documented macOS setup. These eight failed invocations remain
recorded separately from the corrected batch; they are not qualification passes.
All eight corrected invocations passed at parallelism four while the workspace
gate compiled. Each invocation ran three native and three contained synthetic
sessions. These use the fresh final-code probe binary, with no vendor calls.

Validation results are recorded in the accompanying
[mount-helper proof summary](../compatibility/acp/mount-helper-cleanup-proof.json).
The prior failed stress receipts remain unchanged. This closes only the mount
preflight cleanup work after its supported-host checks; broader S6 qualification,
production ACP admission and S7 mission acceptance remain separate.

## Five-axis self-review

Correctness: both sentinel directions must close, and daemon absence is a separate
required result. Creation uncertainty, cancellation and engine death have distinct
outcomes. Review tightened the shared deadline check before later control spawns;
it also added real negative controls for unavailable inventory and same-name
replacement. Readability: sentinel rendering stays shared with the original argv
helper, while daemon ownership is confined to the mount-proof module.
Architecture: the existing bounded Docker control utility is reused without a new
dependency, protocol change or mission execution primitive. Security: the guest
has no control credentials or network; immutable labels and IDs prevent cleanup
from targeting a replacement by name. Windows compilation excludes the Unix-only
implementation and diagnostic helper. Performance: successful preflight adds image
inspection, creation and ownership queries; its existing per-process mount cache
still avoids repeated proofs. Cleanup polling is bounded and does not retry the
proof or invoke a provider. This is a self-review, not an independent agent review.
