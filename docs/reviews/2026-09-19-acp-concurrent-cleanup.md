# ACP concurrent cleanup review

Provider-free concurrent compatibility probes exposed two races in PR #69.
These checks use the pinned Alpine Python image and synthetic credentials;
no vendor adapter, model call or Keychain access is involved.

## Findings and changes

Recovery first listed another live run's egress network, then tried to inspect
it after that run had removed it. The missing network incorrectly failed the
new run's startup. Recovery now inventories immutable full network IDs. If an
inspection fails, a successful, unfiltered fresh inventory must confirm that
exact ID is absent before recovery skips it. Inspection failures for an existing
network, inventory failures and malformed inventories still fail closed.
A network recreated under the same name has a different identity.

After that fix, concurrent probes overlapping the workspace suite exposed
cleanup failures. One reported the new `container removal failed` stage; three
reported Docker's `removal ... is already in progress` during egress teardown.
The ACP container inherits `--rm`, so automatic deletion can race its explicit
removal and immediate final inventory. Removal now polls for confirmed owner-label
absence within a five-second deadline, matching the existing final Docker-control
query's budget. It does not resend the deletion or accept Docker's acknowledgement
as proof. Inventory errors and deadline expiry remain failures, with recovery
evidence retained. Namespace lease expiry and the one-second host-process and
stderr cleanup limits are unchanged.

ACP failures now distinguish container removal, stderr task failure and stderr
drain timeout. The error retains a fixed stage; daemon details remain in the
existing tracing event. No temporary diagnostic printing is retained.

The original earlier cleanup flake had no component detail, so these reproduced
races cannot conclusively identify that historical occurrence. Its failed receipt
remains a failure. The changes address independently reproduced defects, and
broader containment qualification stays open.

## Regression evidence

Five deterministic recovery tests cover disappearance, an existing network,
unavailable or malformed inventory, invalid identity, and successful owner inspection. The Linux
live egress proof additionally deletes an inventoried network and reuses its
name before inspection, checking that the replacement survives. Two synthetic
Docker-control tests cover delayed deletion after a rejected concurrent removal
and a namespace that persists despite a successful removal response.

Before the first fix, one of eight concurrent harness invocations failed at
network inspection. Twelve invocations passed after that fix, then four further
invocations overlapping the workspace suite exposed the deletion race above.
Each complete invocation exercises three native and three contained sessions.
With both fixes, eleven of twelve invocations passed while the final workspace
suite was running. The remaining invocation timed out in the pre-existing
90-second bind-mount proof, before ACP initialization (zero events). Its trusted
sentinel helper remained running after timeout. Explicit removal by the inspected
container ID succeeded, and a subsequent inspect confirmed absence. This is a
separate unclosed preflight-lifetime gap, tracked at priority 1 in
[`container-mount-proof-owned-cleanup`](../../.kranz/tickets/container-mount-proof-owned-cleanup.md)
and added as a dependency of S6. Neither that run nor the batch is recorded as a
pass. The retained [concurrency summary](../compatibility/acp/cleanup-concurrency-proof.json)
includes the failed batches, final source hashes and private result digests.
Final validation: all four workspace gates passed. The full suite passed 3,029
tests with zero failures and ten existing ignores, including all eight required
ACP daemon proofs and the real external-evaluator proofs. Clippy with warnings
denied, formatting and workspace build passed. Final daemon inventories showed
zero containers and zero Kranz egress networks or volumes. The new deterministic
name-reuse case is part of the Linux-only live egress test and awaits CI; it is
not counted as a local live-test pass. Staged secret scanning, knowledge freshness
and domain lint also passed. Colima was restored to its original stopped state.

## Five-axis review

Correctness: absence is verified against daemon inventory, not inferred from an
error string or deletion acknowledgement. Readability: inspection and removal
confirmation have separate, explicit failure paths. Architecture: recovery stays
in the existing egress owner and ACP lifetime modules; protocol and mission
admission contracts are unchanged. Security: immutable IDs prevent name reuse
from redirecting inspection, all uncertain outcomes still fail, and no authority
or egress is added. Performance: normal owner inspection adds no query; failed
inspection adds one inventory query, while pending deletion polls every 50 ms
within the bounded confirmation window. This is a self-review.
