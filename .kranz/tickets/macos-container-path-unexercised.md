---
state: done
state-note: "Done via the ticket's honest alternative: GitHub's hosted macOS ARM runner rejected Colima because virtualization is unavailable, so the release-supported container sandbox host is now Linux only. macOS session and gate resolution fail closed with guidance to use the native Seatbelt process provider; rust-macos requires git,sandbox-exec and records container skips instead of pretending to exercise them. The 2026-08-21 macOS operator Colima receipt remains evidence, but it is not a continuously renewable CI release gate."
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

## Done when

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
