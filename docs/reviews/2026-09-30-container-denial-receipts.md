# Container denial receipts: file and network fixtures

Base: public `a37c9981`. Updated 2026-10-02. This is the test-only implementation of
[container-denial-proof-receipts](../../.kranz/tickets/container-denial-proof-receipts.md).
The ticket remains open. Production profiles, resource limits, supervision
and cleanup behavior are unchanged.

## Findings and change

The legacy command-gate fixture accepted any unsuccessful command as an
access denial, including a guest that never started. The worker fixture also
used a chained command, so failure of an earlier assertion could prevent the
read-only-root write from running.

Each access probe now needs all four pieces of evidence:

1. A unique start witness written by the guest and read by the host.
2. The expected operation failure: an exit code and access-error diagnostic for files, or an allowed route-error classification for sockets.
3. An exact receipt and a successful overall probe exit.
4. A successful, empty runtime inventory for the probe's random owner label.

Hidden-file reads require the missing-path error from `cat`; read-only writes
require the read-only-filesystem error. A successful read or write, missing
tool, unexpected error, startup failure or supervision timeout fails the proof.
Explicit `set +e` around the operation preserves its actual status even on
shells that otherwise collapse a subshell's error to exit 1 under `set -e`.

The fixture wrapper uses production-generated container argv and the existing
bounded runner, adding only an observation label. If normal removal is not
confirmed, the proof fails. Recovery inspects each full ID and its owner label
before removing that ID, then rechecks inventory. It never removes by name or
prunes unrelated resources. This is fixture cleanup, not a new production
recovery contract; interrupted or delayed creation still cannot count as a
passing denial.

The existing allowed file access, host canary and unchanged audit-log controls
remain. Linux CI explicitly requires the five named file-access and synthetic tests and refuses skips
or zero-test matches. The synthetic receipt test also rejects absent witnesses,
unconfirmed cleanup and a timeout after emitting an otherwise valid receipt.

## Audit boundary

The cache and late-created-authority fixtures already require guest progress
and successful completion of their full assertion sequence. The contained
external-evaluator fixtures consume structured results and require cleanup.
They were inspected but not changed in this slice.

The direct-socket bypass fixture now runs a pinned Python socket probe.
Successful reads from the same target on the ordinary bridge bracket the
attempt from the internal network. The target reports ready only after listen
and has a 120-second guest lifetime. The negative probe accepts only network
unreachable, host unreachable or socket timeout; connection refusal/reset,
invalid descriptors, exhausted descriptors and unexpected success fail it.
The probe must write its start witness, emit its exact receipt, finish normally
and leave no owned container. The dedicated egress CI job requires the final
`KRANZ_SOCKET_DENIAL_PROVEN` marker as well as a successful test exit.

Synthetic Python cases cover all accepted and rejected error classes and the
positive control's target bytes. Synthetic runtime cases prove that cleanup
never removes an unowned ID, rejects malformed/unavailable inventory, and
refuses to treat a deletion acknowledgment as absence. Normal auto-removal
may settle within a bounded inventory wait; unavailable inventory still fails
immediately. Forced recovery never converts a failed probe into a pass.

## Review

Self-review, not an independent review:

- Correctness: fresh witnesses bind evidence to individual probes; the error
  branch must complete successfully, and cleanup uncertainty cannot pass.
- Readability: one test-only helper owns receipt admission and observed cleanup.
- Architecture: only tests and their CI invocation change; production argv,
  persisted state and gate behavior stay unchanged.
- Security: only synthetic data crosses into these fixtures; recovery checks
  full IDs and random ownership labels before removal.
- Performance: work is confined to tests; there is one extra worker probe and
  bounded inventory after each denial.

## Validation status

The final October 2 implementation passed 3,148 workspace tests (zero failed,
10 ignored), formatting, Clippy with warnings denied, the workspace build,
strict Rust documentation, Cargo deny, domain lint and Gitleaks. Synthetic
fixture validation needs no provider credentials or model calls. A deliberate
negative control that accepted connection refusal as denial failed as expected.
Docker is unavailable locally; no live container result is claimed until the
required Linux CI jobs supply it.
