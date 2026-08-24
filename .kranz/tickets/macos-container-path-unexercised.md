---
state: done
state-note: "Done: the rust-macos job provisions Colima (brew install colima docker; colima start) so the live container tests execute instead of skipping, and declares KRANZ_REQUIRED_CAPABILITIES=git,sandbox-exec,container so a future runtime regression FAILS the job rather than silently reverting to skips - the anti-vacuity guard this ticket asked for, delivered by the mechanism from skipped-gates-are-indistinguishable-from-passes. NOT VERIFIED LOCALLY: no macOS host was available, so the Colima step, its runtime cost, and any macOS-specific container bug it surfaces are proven only by CI. Expect it to surface something - the equivalent Windows change did, immediately, and those bugs were real."
title: The macOS container path is claimed-supported but never exercised in CI
priority: 2
schedule: once
---

## Goal

Make the macOS container tests actually run in CI, or stop claiming macOS as a
supported container host. Today they report `ok` without executing.

## Why this exists

`sandbox_container.rs` states the support matrix plainly:

> Host support is deliberately macOS/Linux only. Session and gate resolution
> fail closed on Windows even when `docker.exe` is present [...] Runtime
> detection is not evidence that those mounts enforce the declared policy.

Linux honours that claim: the `rust-linux-container-egress` job does real
container work and guards itself with an anti-vacuity grep
(`test result: ok\. 1 passed`), and the ubuntu lane's smoke tests run against
the preinstalled Docker.

macOS does not. GitHub's macOS runners ship no container runtime, so
`sandbox_container::detect()` returns `None`, every live container test takes
its early-return skip path, and the job reports `ok`. Half of a documented
two-platform support claim rests on tests that have never run.

This was found while closing `m7-windows-containment-parity`. The Windows
container tests had the identical problem for a different reason —
`command_available` did not consult `PATHEXT`, so `detect()` never saw
`docker.exe`. Fixing that lookup turned those tests on for the first time and
they immediately failed on two real bugs (a verbatim `\\?\` path breaking
docker's colon-delimited `-v` parser, and the daemon's Windows-container mode
being unable to pull Linux images). Windows is now gated with a loud skip via
`host_supports_container_contract`, since the provider refuses that platform
anyway. macOS has no such excuse: it is claimed as supported.

The general lesson worth carrying: a runtime-gated test that returns early
still prints `ok`, so a skip is indistinguishable from a pass in CI output.
Every such gate is a place where support can be claimed without evidence.

## Done when

- The `rust-macos` job provisions a container runtime — `brew install colima
  docker && colima start` is the cheap option — and the live container tests
  execute rather than skip.
- The macOS container run is anti-vacuity guarded the way
  `rust-linux-container-egress` already is, so a future runtime regression
  fails the job instead of silently reverting to skips.
- Any bug the newly-executing tests surface is fixed or ticketed. Assume they
  will surface something: Windows did, and macOS bind-mount and path semantics
  differ from Linux.
- If provisioning a runtime proves impractical on hosted macOS runners, the
  alternative is honest rather than silent: narrow the module's support claim
  to Linux and make macOS skip loudly through
  `host_supports_container_contract`.

## Notes

Deliberately not folded into the Windows branch that found it: this is a
pre-existing gap, not a regression that branch introduced, and it wants its own
receipt. Runner cost is roughly two to four minutes for the Colima start.
