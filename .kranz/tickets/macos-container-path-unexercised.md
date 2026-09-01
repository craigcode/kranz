---
state: open
state-note: "Reopened 2026-08-31. The v0.2.0 narrowing stands on its own evidence and is NOT being reverted: hosted macOS cannot renew a CI receipt. What reopens this is a third option the ticket did not consider — proving the contract on the operator's own host at run time instead of in a lane that cannot execute. A 2026-08-31 M4 Pro receipt showed the macOS path passing its live container tests once its mounts were real, and showed the specific defect that made them fail: a runtime can accept a bind mount and share nothing, silently."
title: The macOS container path is claimed-supported but never exercised in CI
priority: 2
schedule: once
---

## Goal

Make the macOS container tests actually run in CI, or stop claiming macOS as a
supported container host. Today they report `ok` without executing.

## Why this exists

Before this resolution, `sandbox_container.rs` stated the support matrix as:

> Host support is deliberately macOS/Linux only. Session and gate resolution
> fail closed on Windows even when `docker.exe` is present [...] Runtime
> detection is not evidence that those mounts enforce the declared policy.

Linux honours that claim: the `rust-linux-container-egress` job does real
container work and guards itself with an anti-vacuity grep
(`test result: ok\. 1 passed`), and the ubuntu lane's smoke tests run against
the preinstalled Docker.

Hosted macOS did not honor that claim. A 2026-08-21 operator-host Colima
receipt proved the Docker egress fixture once, but GitHub-hosted macOS had no
runtime, so the ordinary live container tests took their early-return skip
paths and the job reported `ok`. Attempting to provision Colima on the hosted
ARM runner failed deterministically with `VZErrorDomain Code=2`:
virtualization is unavailable on that hardware. The operator receipt is useful
evidence, but it is not a continuously renewable release gate.

This was found while closing `m7-windows-containment-parity`. The Windows
container tests had the identical problem for a different reason —
`command_available` did not consult `PATHEXT`, so `detect()` never saw
`docker.exe`. Fixing that lookup turned those tests on for the first time and
they immediately failed on two real bugs (a verbatim `\\?\` path breaking
docker's colon-delimited `-v` parser, and the daemon's Windows-container mode
being unable to pull Linux images). Windows is now gated with a loud skip via
`host_supports_container_contract`, since the provider refuses that platform
anyway. macOS needed either a maintained self-hosted runner or a narrower
public support claim; this ticket chose the latter for v0.2.0.

The general lesson worth carrying: a runtime-gated test that returns early
still prints `ok`, so a skip is indistinguishable from a pass in CI output.
Every such gate is a place where support can be claimed without evidence.

## Reopened: prove the host instead of claiming the platform

The narrowing answered this ticket's question honestly, and the answer was
right for the evidence available. Hosted macOS runners are guests without
nested virtualization, so `VZErrorDomain Code=2` is the hardware refusing and
no CI lane can renew a macOS receipt.

But "cannot be proven in CI" and "does not work" are different claims, and
v0.2.0 shipped the second while only the first was established.

### What the 2026-08-31 receipt showed

Apple M4 Pro, Colima 0.10.3, Docker 29.2.1, `KRANZ_REQUIRED_CAPABILITIES=git,sandbox-exec,container`:

- Full workspace suite: 2584 passed, 2 failed.
- Both failures were the same defect, and neither was in the provider.
- With the mounted paths actually shared, both pass:
  `test result: ok. 2 passed; 0 failed; 1050 filtered out`.

### The defect the receipt found

A container runtime can accept `-v /host/path:/guest` for a path its daemon
cannot see, create an empty directory inside its VM, mount that, and exit 0.
Proven directly: a file written on the host before the run was invisible
inside the container, exit code 0 throughout.

In production that is silent loss of work. The worker writes into a VM that
teardown destroys, and the validator judges a tree where nothing landed. No
error is raised at any point, which makes it strictly worse than a refusal.

Colima's default mount set is the home directory alone, and macOS puts
`TMPDIR` under `/var/folders`, so kranz's own scratch is outside it. The
hazard is not macOS-specific: Docker Desktop keeps its own file-sharing list,
a remote `DOCKER_HOST` shares no local path at all, and a rootless daemon can
sit in its own mount namespace.

### Why this changes the support question

Platform allowlists answer "did someone prove this OS once". The mount proof
answers "does THIS host honor the contract right now", which is the question
that actually protects a mission. A macOS host with real mounts is safer than
an unproven Linux host with a remote daemon, and the allowlist gets both
backwards.

## Done when

- [x] Session AND gate resolution take a bind-mount proof on any platform
  without a renewable CI receipt, and refuse with the failing path and a
  remedy rather than a platform verdict. Both, deliberately: with only the
  session gated, a proven host would run its worker contained and then fail
  at its own merge gate.
- [x] Every declared root is proven, not a representative one. Sharing is
  per path, and the original failure was exactly a host that shared the
  worktree but not the scratch.
- [x] `host_supports_container_contract` agrees with resolution, so live
  container tests run wherever the provider would run.
- [x] Windows stays refused regardless of any proof: its gap is the POSIX
  guest-path and `/dev/null` authority-mask contract, which no mount proof
  addresses.
- [x] The Linux path is unchanged and pays no probe cost, because CI renews
  its receipt continuously.
- [ ] The macOS scratch root is chosen so a default Colima install works
  without the operator reconfiguring mounts. Today `TMPDIR` under
  `/var/folders` is outside Colima's default share, so the proof correctly
  refuses and the operator has to act. Refusing beats losing work silently,
  but picking a shared scratch would beat both.

## Originally done when

- [x] Provisioning a runtime proved impractical on hosted macOS: the runner
  exposes no virtualization for Colima.
- [x] The module's release-support claim is narrowed to Linux, backed by the
  anti-vacuity-guarded `rust-linux-container-egress` receipt.
- [x] macOS skips the container capability visibly and does not require it;
  enforced container configuration fails closed with guidance to use the
  supported native Seatbelt process provider.
- [x] The existing macOS operator receipt remains documented without being
  misrepresented as a renewable hosted-CI guarantee.

## Notes

Deliberately not folded into the Windows branch that found it: this is a
pre-existing gap, not a regression that branch introduced. A physical
self-hosted Mac runner could reopen macOS container support later; that would
require a maintained required check, not merely runtime detection.
