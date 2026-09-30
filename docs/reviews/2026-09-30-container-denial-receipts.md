# Container denial receipts: file-access fixtures

Base: public `a37c9981`. This is a test-only slice of
[container-denial-proof-receipts](../../.kranz/tickets/container-denial-proof-receipts.md).
The ticket remains open. Production profiles, resource limits, supervision
and cleanup behavior are unchanged.

## Findings and change

The legacy command-gate fixture accepted any unsuccessful command as an
access denial, including a guest that never started. The worker fixture also
used a chained command, so failure of an earlier assertion could prevent the
read-only-root write from running.

Each file-access probe now needs all four pieces of evidence:

1. A unique start witness written by the guest and read by the host.
2. The expected operation exit code and access-error diagnostic.
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
remain. Linux CI explicitly requires the three named tests and refuses skips
or zero-test matches. The synthetic receipt test also rejects absent witnesses,
unconfirmed cleanup and a timeout after emitting an otherwise valid receipt.

## Audit boundary

The cache and late-created-authority fixtures already require guest progress
and successful completion of their full assertion sequence. The contained
external-evaluator fixtures consume structured results and require cleanup.
They were inspected but not changed in this slice.

The separate direct-socket bypass assertion in
`container_egress::tests::container_per_host_egress_live_proof` still accepts any
unsuccessful `docker run ... nc` result. Its ordinary-bridge positive control
does not prove that the negative guest started. That requires a subsequent
networking-fixture change; this slice does not close the entire audit ticket.

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

Local validation passed: full workspace tests (3,146 passed, zero failed,
10 ignored), formatting, Clippy with warnings denied, workspace build, strict
Rust documentation, `cargo deny`, domain lint and Gitleaks. The synthetic
startup, timeout, missing-tool, unexpected-success and incomplete-receipt
controls ran successfully. The production portions of both edited Rust files
were also compared byte-for-byte against the base and are unchanged.

A working Docker daemon is unavailable on the local host; runtime-gated
container tests skipped explicitly. No live container proof is claimed.
The named Linux CI proofs must pass before merge.
