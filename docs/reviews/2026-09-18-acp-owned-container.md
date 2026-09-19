# ACP owned-container review

This follows the no-spawn preflight in PR #69. It adds an owned Docker boundary
to the direct ACP backend API while keeping ordinary enforced-ACP mission
configuration closed. S6 vendor certification and S7 acceptance remain open.

## Five-axis review

Correctness: the supervisor requires a fresh lease renewal before peer launch.
Engine death, stalled host renewal and blocked I/O cannot leave the worker
authorized indefinitely. Namespace death reaches detached children; process-group
cleanup alone did not. Normal completion still delivers the real ACP report.
The engine records the fixture's nonempty change as a feature commit without
altering primary checkout bytes or the base branch. Cleanup uncertainty becomes
failure and retained recovery evidence.

Security: the existing mounts, authority masks and network relay remain the
policy boundary. The host lease and supervisor are outside writable mounts;
PID 1 disables dumpability before executing a peer. Worker and Docker-control
environments are separate. A random immutable label and deletion by inspected
ID prevent a name collision or rename from redirecting cleanup. Images are
pinned and must have a trusted Python runtime; immutability alone is not a
certificate for arbitrary image contents. No credential inheritance, Keychain
access, native-provider home mount or default promotion is introduced.

Architecture: this adds lifetime ownership to the existing container provider,
reuses its policy and bounded Docker command runner, and leaves the ACP wire
protocol and mission authority contracts intact. A successful fixture is kept
distinct from production admission and vendor certification. The raw Init
receipt carries actual boundary identity without changing persisted event types.

Readability: container preparation, lease ownership and daemon cleanup live in
one module. The small guest supervisor handles no shell syntax or model output;
its failure diagnostics contain only a fixed stage and exception class. The
operator guide states the trust boundary, proof scope and remaining work.

Performance: the heartbeat writes eight bytes twice per second. Three daemon
I/O threads hold at most one 64 KiB chunk each, while the main supervisor checks
the lease independently. Namespace admission adds image/mount checks and bounded
Docker control calls. It is a session boundary cost, not a per-token operation.

## Validation record

The initial macOS run refused a temp root not shared by Colima. After selecting
a shared scratch root, renewal initially failed when replacing the lease inode;
keeping one inode and updating a fixed-size counter passed the same tests.
Unreadable lease evidence never renews authority. The first ten focused proofs
passed, including real daemon cleanup, host SIGKILL, a dead owner's delayed start,
hostile I/O, worktree delivery and filtered egress. A pure admission regression
also requires refusal of floating images, wrong runtimes, off-mode boundaries
and mismatched working directories before container creation.

The final eleven focused tests (eight daemon proofs, two pure admission tests
and the re-executed owner helper) also pass on the pinned Linux ARM64 vendor
image. The native and contained deterministic compatibility probes pass. The
probe's credential-policy tests require immutable images and refuse Keychain
use from a Linux container.

After the operator separately authorized one fixed Codex prompt, the private
native-login copy authenticated and returned the exact report without tool use.
The overall qualification failed because the proxy denied five attempts to an
additional hostname. The failed redacted receipt is retained; no retry or
allowlist broadening occurred. Claude was not called, and Keychain was not
accessed. Authentication success does not turn that qualification into a pass.

Final local validation: `cargo test --workspace` passed 3,022 tests with zero
failures and ten existing ignores, with both real Docker proof suites enabled.
Workspace Clippy with warnings denied, formatting, build, domain lint (981
files), staged secret scan and knowledge freshness passed. The final workspace
run also passed doctests; an earlier run overlapped a rebuild in the shared
target directory and failed crate-metadata resolution, so the final gates were
run without concurrent compiler jobs. The earlier native cancellation regression
was fixed without changing sandbox-off semantics, and all 27 native ACP tests
pass.

Linux CI verified the required daemon proofs and the provider-free contained
probe on commit `6d64b4c`. Its knowledge-refresh check caught the newly changed
CI file against the guide's prior UTC verification date; the guide was reviewed,
updated to describe the new checks, and reverified. This is a five-axis
self-review, not an independent audit.

All reported CI checks on `9666dd5` subsequently passed, including Windows Rust;
the optional smoke job was skipped. The [endpoint investigation](2026-09-19-codex-contained-egress.md)
keeps the unexpected hostname blocked and records a credential-free configuration
check against the pinned image. The subsequent probe change writes file-only
credential storage and disabled plugin features into the disposable Codex home
before adapter startup. The separately authorized follow-up is recorded below;
full provider containment certification remains open.

## Probe startup follow-up

Correctness: both key and copied-login paths receive the same private startup
file before spawn; preflight only previews it. The receipt distinguishes a
completed write from a preview and does not claim effective runtime enforcement.
Security: the file is created exclusively, uses private permissions on Unix and
contains no credential. Native settings and history remain outside the child
home, the endpoint stays blocked, and the original failed receipt is preserved.
Architecture: this is confined to the no-tools probe, with no mission defaults
or containment admission change. Readability: one TOML constant supplies both
the file and receipt. Performance: it adds one small local file write per probe.

The synthetic peer checks the startup file before reading ACP initialization.
Both authentication paths pass natively and in the pinned vendor container,
using fake credentials, and the source homes remain unchanged. The existing
five probe policy tests pass. An initial test-only assertion incorrectly
required redaction of the fake key literal in a copied-login run where no key
was supplied; it now checks exact-value redaction on the explicit-key path.

The same fixture checks also pass on CI's pinned Alpine Python image. One
additional run on that image, concurrent with the full workspace suite, returned
the report but failed cleanup confirmation. A subsequent daemon inventory was
empty and no recovery ledger remained for that fixture. The serial rerun passed;
the failing component was not identified, and cleanup deadlines and failure
semantics were not relaxed. This remains an observed cleanup flake, not evidence
that the earlier failed run passed.

The follow-up workspace suite passed 3,022 tests with zero failures and ten
existing ignores, with both Docker proof suites enabled. Workspace Clippy with
warnings denied, formatting and build passed. Knowledge freshness and domain lint
(984 files) passed.

One newly authorized contained Codex call then passed with that policy in 8.5
seconds, reporting `gpt-6-astra`. The fixed report, no-tool constraint, zero denied
connections and unchanged workspace passed the probe, including cleanup. A
separate daemon query by immutable owner label confirmed absence. The
[proof summary](../compatibility/acp/codex-contained-plugins-disabled-proof.json)
links the account-redacted receipt and hashes its private source. No retry,
allowlist expansion or Keychain access occurred. This closes this basic Codex
report check, not Claude proof, production admission or S7 acceptance.

The [concurrent cleanup follow-up](2026-09-19-acp-concurrent-cleanup.md) records
two independently reproduced races, their bounded fixes and regression evidence.
The historical cleanup failure remains retained, without claiming a definitive
attribution that its original diagnostic cannot support.

## Claude contained report follow-up

After the operator provisioned a private OAuth token, one authorized contained
Claude prompt passed on engine commit `31ad9c1` and the pinned Linux ARM64 image.
The exact report, zero tool events and denied connections, unchanged workspace
and cleanup all passed in 5.2 seconds. A separate query confirmed owner-label
absence and empty container/egress inventories; Colima was returned to stopped.
No retry or Keychain access occurred. The [proof summary](../compatibility/acp/claude-contained-oauth-proof.json)
records the versions, source/export hashes, redactions and limits.

Correctness: preserve the initial `Not logged in` notification despite the
successful token-backed request; it is not an authentication verdict. Preserve
both models named in usage rather than claiming the configured `default` is a
specific model or that one ACP prompt implies one provider request. Security:
the credential stays in the private provisioned file and ephemeral session
channel; account limits and command inventories are removed from the export.
Architecture: this changes evidence only, with production admission still
closed. Readability: the guides distinguish basic provider passes from the open
mount-proof and mission-acceptance work. Performance: one 5.2-second probe,
with adapter-reported cost USD 0.0426852 and no hard dollar cap. This is a self-review.

Engine source is unchanged from the prior four-gate validation: 3,029 tests,
zero failures, ten existing ignores, Clippy, formatting and build passed.
Linux CI has also passed the new live network-disappearance/name-reuse case on
`31ad9c1`. This receipt addition receives JSON/hash, redaction, secret-scan,
knowledge-freshness and domain-lint checks; no new full Rust run is claimed.
