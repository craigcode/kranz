# Gate approval driver checkpoint

This is the first real S5 stage integration. It is a draft checkpoint; the
mission configuration refusal remains in place. Revisions, milestone validation,
final deliverable checks, scratch integration merge, and pending operator
surfaces are still required for full S5 acceptance.

## Changes and correctness

Initial plan approval pins evaluator declarations and every checker dependency
from the immutable base. It freezes the normalized plan, policy and existing
advisory diagnostics, records a request before execution, and records result and
engine disposition after cleanup. The full evaluator ladder must permit progress
before any result is consumed. A base or mission-branch change refuses consumption
before branch creation or plan commit. Git failure after consumption records an
attempted action, not a completed approval; retry creates new evaluation IDs.

Named approval lints keep their advisory meaning. Their diagnostic records do
not invent command exit codes or satisfy a required command receipt. Blocking
failure/error and escalation do not become passes. Local repository authority
and the local API mutation capability have distinct attribution. A policy actor
cannot approve a plan, and neither capability label claims verified human presence.

The additive `gate.evaluation-closed` event and default-absent record field are a
contract-change implementation of the existing crash/replay requirements.
Recovery and a new explicit approval attempt close unconsumed attempts. They
preserve the old result and consent, never invoke an evaluator, and never replay
an effect. CLI tail and auditor summary distinguish closed attempts from pending
work. Existing logs without these fields retain their prior representation.

## Security

The existing Docker evaluator owns process containment. The stage driver neither
mounts the checkout nor copies provider state. Its installed Docker CLI cannot
come from the repository. Checker execution has a private host cache directory
shared with the Docker daemon; retained evidence uses pinned no-follow directory
handles, exclusive leaf creation and file synchronization. Cleanup failure or a
panic preserves private recovery files rather than deleting a potentially live
container's mounts. Only confirmed cleanup permits their removal.

Raw and retained hashes remain distinct. Retained text uses the existing scrubber;
binary input retention is an explicit omission marker. Missing or changed retained
files remain unresolved through the existing exporter. No new exporter or token
store is introduced. Evaluator cleanup, redaction and result correlation remain
owned by the S3 implementation.

## Readability, architecture and performance

The driver reuses the existing input builder, reducer, event writer, contained
process runner and export path. The approval API remains synchronous; a scoped
thread hosts the short-lived Tokio runtime, avoiding a nested runtime panic when
called by the server. Legacy approval entry points retain their behavior. Checker
ordering comes from the pinned declaration set, with mechanical checks before
judgment and a 32-registration cap. Existing stream, artifact and attempt caps
continue to apply. Retention and evaluator execution are bounded but add I/O and
container startup per configured check.

## Validation

The real Docker proof exercises pass, blocking failure, escalation, nonzero exit,
restart without consumption, retained hash checks, unchanged primary source and
HEAD, and a base advance while a passing checker is running. No vendor CLI,
provider credential, Keychain request or paid model is involved. The dedicated
Linux evaluator job requires the named approval proofs without skip markers.

Full workspace validation passed: 3,004 tests, zero failures and ten existing
ignored tests; Clippy with warnings denied, formatting and build also passed.
The 16 protocol schema checks, staged secret scan, domain lint and knowledge
refresh passed. The embedded Rust license inventory includes the new runtime
use of tempfile and its dependency. This is a five-axis self-review, not an
independent audit, merge approval or release proof.
Basic live Claude and Codex compatibility is already recorded in PR #60; enforced
ACP worker containment and S7 governed mission acceptance remain separate work.
