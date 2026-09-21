# v0.3.0 release corrections

The release review of `fba8f4243c8e3331b7118ddf8f22bf0072f71ec9`
found stale README installation references and a credential-bearing ACP launch
file retained after uncertain cleanup. The correction also addresses nested
snapshot credential paths and two bounded-time reliability findings before tagging.
The earlier rehearsal remains evidence of its original source revision.

## Corrections and regression evidence

- ACP launch arguments, cwd and environment now cross a length-prefixed stdin
  prelude consumed by the trusted PID 1 before ACP traffic. No launch file is
  written. The host bounds size and writes; the guest bounds size and continues
  checking the lease while input is partial. Real Docker tests cover credential
  delivery, intact ACP messages, malformed input, EOF and owner expiry. Failed
  cleanup still retains recovery metadata, with a synthetic token absent from
  every retained control file. This does not erase separate provider homes after
  an engine crash.
- Worker Docker creation has a 30-second startup cap instead of the five-second
  inspection cap. A delayed-control regression proves a six-second creation can
  finish; timeout remains a failed, uncertain creation with retained recovery
  state. There is no retry and no broad container cleanup.
- External stage evaluators receive one deadline pinned before the first check:
  two minutes per applicable checker plus three minutes for setup, cleanup and
  final rechecks. Per-checker execution remains capped at two minutes. The
  lifecycle regression consumes three passing decisions after six minutes,
  rejects a changed subject and still rejects consumption at expiry. Both
  synchronous and asynchronous stage drivers use the same calculation.
- Snapshot selection excludes conventional private paths at every directory
  depth and files containing a PEM private-key BEGIN line. Public certificates
  and similarly named source paths stay eligible. The regression includes
  tracked nested secrets, selection receipts and a certificate becoming private
  without HEAD changing. Exclusions are visible coverage limits; this is not a
  general secret scanner.
- README installation, registry and release references use 0.3.0. The release
  version check now validates those references and rejects one stale install
  command even when all other references are current.

## Five-axis implementation review

Correctness review traced startup failure through the existing cleanup path and
stage decisions through the unchanged subject/authority consumption checks.
Readability review keeps the private launch protocol beside its supervisor and
documents both its limits and the separate credential-home lifetime. Architecture
review adds no dependency, public protocol or persisted event-schema change;
existing Docker controls, bounded stdin and gate lifecycle rules are reused.
Security review checked that no provider credential is serialized to the recovery
directory or Docker arguments, and that stalled input cannot suspend lease
checks. Performance review keeps launch data capped at 1 MiB and scans snapshot
contents within existing per-file and total byte limits.

This is the implementation review, not a new independent-agent review. The
existing live Claude/Codex receipts retain their original source hashes; no new
provider call or Keychain access was made for these corrections. The changed
startup transport is exercised with synthetic credentials in real Docker.

## Verification and release boundary

Local workspace tests passed 3,071 tests with zero failures and 10 ignored;
ACP, external-evaluator and mount Docker proofs were enabled. Clippy with warnings
denied, formatting, locked build, strict docs, dependency policy, domain lint,
public tree/history audits and package-license checks passed. The standalone ACP
proof also passed all 26 matching tests. The release-version check accepted 0.3.0
and rejected a stale README command. Secret scanning identified a dummy PEM test
block; the fixture now constructs its non-secret header at runtime, without a
scanner waiver. Cross-platform results are recorded with the correction PR.
Required CI, a new release rehearsal and native archive smoke checks must pass
on the resulting main commit before tagging. This document does not claim a tag,
GitHub release or registry publication.
