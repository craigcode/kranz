# ACP operational follow-ups

This review starts from public v0.4.0, commit
`fccdef95334f82f4595005407e2ba7caeacc824c`. It resolves the reported cancellation,
diagnostic and operational-boundary observations without adding ACP client
filesystem/terminal services or making new provider calls.

## Findings and dispositions

The expired-permission finding reproduced: `permission_tick` notified the shared
batch cancellation channel after closing one request. The ACP backend already
enforces its own absolute/monotonic session deadline. Removing the batch signal
keeps sibling response capabilities live, while expiry still closes the affected
request. The regression uses two real broker records and response channels,
expires one and successfully resolves the other; replay retains both outcomes.
Existing parallel-buffer tests cover delivery during active sibling runs. The
backend's `read_frame` deadline and error-to-cleanup path were inspected; the
new broker regression does not wait five minutes or invoke a live provider.

Mission control cancellation is intentionally broader than the old documentation.
Only exact permission answers and noninterrupting messages apply during an ACP
batch. Pause/resume, interrupting messages, configuration/revision changes,
grant decisions and structured answers first cancel and join the workers,
preserving inbox order. The test covers every current control variant and the
noninterrupting message exception. Documentation now states that stale/no-op
controls also take this conservative boundary; runtime behavior is unchanged.

An extra mission egress grant previously failed with a generic boundary-drift
error. The refusal now identifies the fixed profile allowlist. It still occurs
before reading credentials, exposes no destination, and never broadens egress.
The regression uses an absent credential file to verify that order.

The real Docker probe confirms that default-bridge peers can reach the relay and
use its allowlist. A separate allowed loopback fixture is the positive control;
denied destinations and direct routes remain denied. This is documented as a
trusted-daemon, single-operator assumption, not hostile-tenant isolation. The
synthetic cache probe exercises the actual mount argv with fixture Rust/Cargo/npm
cache files: cache contents are readable, writes fail and Cargo credentials do
not cross. It reads no real operator cache contents. Cache confidentiality and
host-side mutation are explicit limits. A separately scoped
[qualification ticket](../../.kranz/tickets/acp-cache-relay-isolation.md) owns
possible profile changes; this patch does not silently revise those boundaries.

The [Docker 25.0.5 release notes](https://docs.docker.com/engine/release-notes/25.0/#2505)
and [Moby advisory](https://github.com/moby/moby/security/advisories/GHSA-mq39-4gv4-mvpx)
confirm the reported internal-network DNS issue. Filtered egress now refuses
unrecognized, prerelease or stable daemons below the conservative 25.0.5 floor
before resource creation. Patched older branches are not automatically qualified.
On Docker 29.2.1 / macOS ARM64 with Colima, external DNS resolves on the ordinary
bridge and fails on the internal-only network. This proves that tested setup,
not every resolver configuration or older daemon. No historical vulnerable
daemon was installed or tested.

Optional container tests now make a bounded daemon `info` probe. An installed CLI
with no usable daemon emits `KRANZ_TEST_SKIP`; any explicit ACP, external-gate or
mount proof flag turns that condition into a failure. The regression executes a
discoverable fake CLI with a failed daemon probe in isolated child processes,
testing the optional case and each of the three required flags. Actual proof
test bodies retain their fail-closed behavior after admission.

## Five-axis self-review

Correctness preserves exact consent, per-session expiry and ordered mission-wide
authority changes. Readability improves operator refusals and names the full
control policy. Architecture reuses the existing deadline, notifier, mount and
bounded-command mechanisms. Security tightens DNS admission without claiming
cache secrecy or tenant isolation. Performance adds one bounded version query
when provisioning filtered egress; test daemon probes have a five-second bound.
There is no performance benchmark or new provider qualification claim. This is
assistant self-review, not an independent human security audit.

## Validation

The full workspace suite passed **3,116 tests, 0 failures, 10 ignored**, with
ACP, external-gate and mount containment proof flags enabled. Clippy with
warnings denied, formatting, the locked workspace build, strict docs, cargo
deny, domain/public-tree audits, notices, package licenses and packaging tests
all passed. No dashboard source changed.

The separate exact ignored Docker egress proof passed one executed test,
including bridge-relay and DNS controls, denied destination/direct routing,
and owned cleanup. The exact synthetic cache proof also passed one test with
the expected readable-source/denied-write marker and no skip. CI now explicitly
requests that proof; the updated workflow passes actionlint.

[Commands, source hashes and output digests](evidence/2026-09-22-acp-operational-followups.json)
identify this local verification. Raw logs are retained in the operator
verification archive. GitHub checks must still pass on the proposed commit
before its protected merge.

The first GitHub run flagged four source-linked knowledge notes as stale. Their
ACP profile, consent, cache/relay and test descriptions have been reviewed and
updated with current source citations. `knowledge-refresh` and the domain audit
pass after that documentation correction; the tested runtime source is unchanged.
The dependent lessons index was refreshed after CI identified that additional
link in the knowledge graph; the final committed notes are checked again before
pushing. The earlier pre-commit check is retained as such.
