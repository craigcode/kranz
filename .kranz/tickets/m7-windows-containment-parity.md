---
state: open
state-note: Accepted plan plus phases 1-4 and the hosted phase-5 gate receipt are implemented: process enforcement resolves the production stable AppContainer launcher for sessions, validators, and gates; protected Windows CI proves the hostile boundary, exact DACL restoration, ordinary Node/Rust gates, and retained overhead samples while the container provider remains fail-closed. Only the dedicated operator-controlled Windows 11 receipt remains open.
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
- Each agent launch uses a disposable profile SID; each resolved engine-gate
  posture owns one disposable SID across its validation/final-gate command
  batch, so expensive ACL preparation is once per posture rather than once per
  assertion. Because ordinary Windows tools probe the local drive root during
  startup, an elevated one-time host-preparation script installs Microsoft's
  exact non-inheriting `0x00120088` metadata ACE for `S-1-15-2-1` and
  `S-1-15-2-2` on each relevant local drive. Those ACEs grant no root listing,
  content read, or write rights. The ordinary launcher checks them read-only
  and fails closed with the preparation command when either exact tuple is
  absent. The ACL lease retains a no-follow handle for every path-specific DACL
  it changes, removes only the unique package SID's ACEs root-first, then
  deletes every private plan and the profile. A bounded host-local mutex
  serializes ACL read/modify/write
  batches, so overlapping launches preserve each other's grants. Authority,
  mission metadata, shared Cargo cache, and validator checkout denies are
  applied before spawn.
- Protected Windows CI performs the same explicit host-preparation step, then
  runs the exact production helper receipt through the built CLI and proves an
  AppContainer token plus LPAC behavior by denying a
  sibling root deliberately granted to regular AppContainers, allow/deny
  behavior, shared-Git read, tampered Git-pointer refusal, network denial, overlap-safe
  no-follow DACL cleanup, an unchanged prepared drive-root DACL, and exact
  uncontended restoration before checking
  positive session/gate resolution and continued container refusal.
- Native `.exe` backends are required; batch shims are refused at preparation
  to avoid forwarding model arguments through `cmd.exe`.

This closes production integration. Protected hosted CI also proves ordinary
Node/Rust contract gates and representative retained-posture overhead. It does
not yet prove per-host `fs` filtering or the final operator-controlled Windows
11 receipt.

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
