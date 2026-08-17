---
state: open
state-note: Accepted plan plus phases 1-4 implemented: process enforcement resolves the production stable AppContainer launcher for sessions, validators, and gates; protected Windows CI proves the hostile boundary and exact DACL restoration while the container provider remains fail-closed. Phase 5 normal Node/Rust gates, overhead, and a dedicated Windows 11 receipt remain open.
title: Define and prove a fail-closed Windows containment path for enforced sessions
priority: 1
schedule: once
---

## Goal

Close the Windows gap in M7 without pretending that Job Objects or restricted
tokens provide filesystem isolation. Choose and implement the supported Windows
boundary for `sandbox.enforce: fs | fs+net`, then prove on a Windows host that a
hostile child cannot escape it.

## Decision direction

The process provider stays fail-closed on Windows unless a native primitive can
enforce the same declared writable roots. AppContainer is the primary native
candidate because it can express filesystem and network capabilities; the
existing OCI provider also stays refused until Windows guest paths, authority
masks, and container mode have a concrete hostile-host proof. Restricted-token
or runtime-discovery claims are not accepted from API-level inference alone.

Accepted design: [`docs/scoping/m7-windows-containment.md`](../../docs/scoping/m7-windows-containment.md).

## Phase 1 — fail-closed proof

- Session resolution rejects both Windows process enforcement and the unproved
  Windows container mount contract before spawn.
- Engine-run validation/final/merge gates apply the same refusal.
- The Windows CI leg runs exact named session and gate tests before the full
  workspace suite.
- `enforce: off` is unchanged and remains explicitly unsandboxed.

This closes the unsafe inference gap; it does not close the availability ticket.

## Phase 2 — native capability probe

- `kranz sandbox-probe [--json]` records the Windows version and checks
  `processmodel.dll` for Microsoft's experimental process-sandbox export.
- The DLL is loaded from System32 only. The probe neither calls the export nor
  creates a profile, changes an ACL, or starts a child.
- API presence is reported as capability evidence and never enables production
  containment or relaxes the phase-1 fail-closed behavior.
- The Windows CI leg prints the real-host report before running exact,
  anti-vacuity-checked refusal tests.

This closes the discovery step in the accepted delivery sequence; it does not
prove a stable serialization contract, containment, or availability.

## Phase 3 — stable AppContainer hostile fixture

- A unique disposable profile launches a copy of the engine test executable
  with `PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES` and no capabilities.
- AppContainer-SID ACLs grant the fixture toolchain read/execute and grant only
  the fixture worktree and private scratch read/write/execute. The real checkout
  and its ACLs are untouched.
- The child is created suspended and assigned to the existing kill-on-close Job
  Object before its primary thread resumes.
- The receipt verifies the AppContainer token, allowed reads/writes, denied
  toolchain and sibling writes, and denied access to a live loopback listener.
- Windows CI runs the parent proof by an exact collision-free test name and
  keeps production session/gate enforcement fail-closed afterward.

This proves the stable native primitive on the hosted Windows runner. It does
not yet integrate agent environment construction, authority masks, bounded
output, validator read denial, or every production spawn site.

## Phase 4 — production AppContainer integration

- `provider: process` with `fs` or `fs+net` resolves
  `SandboxBackend::AppContainer`, implemented as a less-privileged
  AppContainer (LPAC) that opts out of `ALL APPLICATION PACKAGES`; Windows
  container enforcement remains refused until its distinct mount contract has
  a hostile receipt.
- Workers preserve the cleared child environment and bounded stdio path.
  Validators receive the mandatory AppContainer wrap and real-checkout source
  read denies. Validation, final, and merge-gate commands use the same launcher.
- The trusted launcher creates the hostile child suspended and assigns it to a
  kill-on-close Job Object before resume. `fs` grants internet-client access;
  `fs+net` supplies no network capability and is hard offline.
- Each launch uses a disposable profile SID and an ACL lease that retains a
  no-follow handle for every changed DACL, removes only that SID's ACEs
  root-first, then deletes the private plan and profile. A bounded host-local
  mutex serializes ACL read/modify/write batches, so overlapping launches
  preserve each other's grants. Authority, mission metadata, shared Cargo
  cache, and validator checkout denies are applied before spawn.
- Protected Windows CI runs the exact production helper receipt through the
  built CLI and proves AppContainer + LPAC token isolation, denial of a sibling
  root deliberately granted to regular AppContainers, allow/deny behavior,
  shared-Git read, tampered Git-pointer refusal, network denial, overlap-safe
  no-follow DACL cleanup, and exact uncontended restoration before checking
  positive session/gate resolution and continued container refusal.
- Native `.exe` backends are required; batch shims are refused at preparation
  to avoid forwarding model arguments through `cmd.exe`.

This closes production integration. It does not yet prove ordinary Node/Rust
contract gates, representative overhead, per-host `fs` filtering, or the final
operator-controlled Windows 11 receipt.

## Acceptance hints

- `provider: process` plus non-off enforcement resolves only the proven stable
  AppContainer boundary; unavailable preparation fails before the hostile child
  starts.
- The selected provider supports `fs`; `fs+net` is either hard-offline or uses
  a non-bypassable egress boundary. Proxy environment variables alone do not
  count.
- Windows CI covers resolution, path/mount encoding, authority masks, and the
  fail-closed negative cases.
- A live Windows receipt runs a hostile brief that attempts an out-of-root
  write and network egress, records both denials, and verifies the allowed
  worktree change still passes its contract gate.
- No completion claim is made from cross-compilation or mocked Win32 calls.
