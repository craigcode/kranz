# ACP branch reconciliation with main

This merge combines the qualified v2 admission and R4 terminal budget branch
(`87c6481b`) with main at `47cc3a8f`. The common ancestor is `ca9d93bf`.
Main had independently implemented terminal deadlines and request caps while
also adding tool-subject security fixes, pane observation types, recovery and
credential utilities, and a client-terminal compatibility probe.

## Resolution

The terminal provider retains R4's atomic reservation-to-authority-to-entry
transition, typed recoverable limits, bounded status tombstones, confirmed
cleanup, 600-second command/session ceiling, separate 300-second retention,
and observed child OOM preference. This replaces main's competing non-reserving
capacity check and deadline configuration. The nine existing real-container
terminal regressions remain required by name in CI.

Upstream tool-call subject tracking, pane observation contracts, compatibility
probe, and nonterminal changes remain present. The probe's explicit public
fixture constructor uses the code-owned R4 defaults; it still refuses any
released worker profile. Main's resource classification helper and regression
remain intact. Recoverable terminal refusals carry typed RPC/audit evidence;
this merge does not infer a failed attempt from a recoverable refusal or wire
an unproven terminal cause into namespace resource attribution.

V2 admission still checks its qualified host tuple before credential reads and
pins the selected local Docker client/endpoint through normal worker cleanup.
V1 profile definitions and configured resource ceilings are unchanged. The
original October qualification receipts and source hashes remain historical
evidence, not claims that those runs exercised this merged tree.

The parent resource ticket is open for R5. Its incoming `done` state described
implementation, but did not supply the multi-minute build, delayed wait,
unpolled flood, remaining exhaustion and cooperative-cancel qualification in
its acceptance contract. Pane APIs and a probe mode likewise do not constitute
a real adapter or consumer qualification. No paid provider run is part of this
reconciliation.

## Five-axis review

Self-review of the merge resolution:

- Correctness: reserve capacity before consent, preserve in-flight create counts,
  release completed control identities, and retain upstream tool-subject fixes.
- Readability: one budget representation and one control-accounting path remain;
  documentation identifies the still-open qualification boundary.
- Architecture: retain upstream pane/probe interfaces without changing mission
  lifecycle or persisted event/state contracts.
- Security: preserve exact one-use consent, cleared child environments, pinned
  namespace execution, fail-closed cleanup and v2 pre-credential admission.
- Performance: retain bounded handles, creation counts, output tails, controls
  and receipts; normal shutdown does not wait for output retention expiry.

## Validation

All four required Rust workspace gates passed: 3,236 tests passed, zero failed
and 16 ignored; Clippy across all targets with warnings denied; formatting;
and workspace build. All nine named actual-container terminal proofs passed
without skip markers. Six compatibility-probe example tests passed. The
[validation summary](evidence/2026-11-07-acp-main-reconciliation.json) binds the
resolved sources and retained local gate logs by SHA-256.

Dashboard gates passed after `npm ci`: 266 tests, typecheck, build, embedded
sync/check and lint. The embedded bundle exactly matches main. Lint retains
existing `react(set-state-in-effect)` warnings. Even G2 passed clean install,
high-severity dependency audit, 22 tests, pack and lint. Both npm audits reported
zero vulnerabilities. Knowledge freshness, domain lint, staged secret scan and
public-tree audit passed; optional private operator vocabulary was unconfigured.

The first Rust compile attempt found two upstream test calls that still treated
profile preparation as synchronous. The credential-renewal regression now awaits
the async admission API; the subsequent full workspace run above passed.

The additional exact-host admission regression **failed admission**
before credential use: Docker reported `MemTotal = 6,198,095,872` bytes, versus
`6,198,104,064` in the qualified policy, an 8,192-byte difference. Colima still
reports a configured 6 GiB (`6,442,450,944` bytes), Docker 29.2.1 and kernel
6.8.0-100-generic. The cause of the changed usable-memory observation is not
established by this check. The exact comparison remains unchanged. This is a
retained failed current-host attempt, not a fresh passing v2 qualification.
A reviewed admission-policy follow-up with renewed proof is needed before v2
can run on this currently observed tuple; the original October passes remain
valid only for their recorded tuple.

Final Docker inventory had no containers or owned networks. Colima was restored
to its original stopped state. No provider was invoked, no real credentials were
used, and no source edits were made in the primary trajectory-board checkout.
